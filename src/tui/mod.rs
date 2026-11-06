//! Live terminal dashboard built on ratatui.

pub mod access;
pub mod app;
pub mod cache;
pub mod clipboard;
pub mod forms;
mod manage;
mod panels;
pub mod parity;
pub mod qr;
pub mod theme;
pub mod ui;
pub mod usage;

use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crossterm::event::{Event as TermEvent, EventStream};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use tokio::sync::broadcast::error::RecvError;

pub use app::{App, PendingCommand};

use crate::api::AppState;
use crate::events::Event;

/// How many commands run with the terminal attached right now (an install
/// from the Commands view, and the prompt after it). Ctrl-C then belongs to
/// that command, so the caller's stop signal should be ignored: see
/// [`command_running`].
static COMMAND_RUNNING: AtomicUsize = AtomicUsize::new(0);

/// Whether the dashboard has stepped aside for a command right now. A
/// Ctrl-C that arrives meanwhile is meant for the command, not for Hashi.
#[must_use]
pub fn command_running() -> bool {
    COMMAND_RUNNING.load(Ordering::Acquire) > 0
}

/// How long the stop signal is still read as the command's after the
/// command and its "Press Enter" prompt end, so a signal already on its way
/// is not taken as one for Hashi.
const SIGNAL_GRACE: Duration = Duration::from_millis(50);

/// The stop future for [`run_until`] when Hashi owns the terminal: resolves
/// on the next termination signal that arrives while no command is attached.
/// Signals that arrive while one is (Ctrl-C reaches every process in the
/// terminal's foreground group) are read and dropped, as long as the
/// dashboard keeps polling this future during the command, which it does.
pub async fn stop_unless_attached(signals: &mut crate::shutdown::Signals) {
    loop {
        signals.recv().await;
        if !command_running() {
            break;
        }
    }
}

/// One count in [`COMMAND_RUNNING`], given back when dropped, even if the
/// command's future is.
struct Attached;

impl Attached {
    fn begin() -> Self {
        COMMAND_RUNNING.fetch_add(1, Ordering::AcqRel);
        Self
    }
}

impl Drop for Attached {
    fn drop(&mut self) {
        COMMAND_RUNNING.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Runs `work` (a command and the prompt after it) with [`command_running`]
/// set, polling `stop` the whole time and for [`SIGNAL_GRACE`] afterwards.
/// A stop future such as [`stop_unless_attached`] therefore reads each signal
/// while it still counts as the command's and drops it, instead of finding
/// it queued once the dashboard is back. Returns `work`'s output and whether
/// `stop` resolved anyway, for a stop future that ignores the flag.
async fn attached<T, S>(mut stop: Pin<&mut S>, work: impl Future<Output = T>) -> (T, bool)
where
    S: Future<Output = ()> + ?Sized,
{
    let _flag = Attached::begin();
    let mut work = std::pin::pin!(work);
    let mut stopped = false;
    let out = loop {
        tokio::select! {
            out = &mut work => break out,
            () = stop.as_mut(), if !stopped => stopped = true,
        }
    };
    if !stopped {
        stopped = tokio::time::timeout(SIGNAL_GRACE, stop.as_mut())
            .await
            .is_ok();
    }
    (out, stopped)
}

/// Most pending terminal events [`discard_pending_input`] throws away.
const MAX_DISCARD: usize = 4096;

/// Reads and drops every event `poll` says is waiting, up to [`MAX_DISCARD`].
/// Returns how many it dropped.
fn discard_pending_input(
    mut poll: impl FnMut() -> std::io::Result<bool>,
    mut read: impl FnMut() -> std::io::Result<TermEvent>,
) -> usize {
    let mut n = 0;
    while n < MAX_DISCARD && matches!(poll(), Ok(true)) {
        if read().is_err() {
            break;
        }
        n += 1;
    }
    n
}

/// Drops keys typed ahead, so keys meant for the dashboard do not reach a
/// command and keys meant for the command do not fire in the dashboard.
fn discard_type_ahead() {
    discard_pending_input(
        || crossterm::event::poll(Duration::ZERO),
        crossterm::event::read,
    );
}

/// [`crate::util::one_line`] that also drops control and format characters
/// (bidi controls, zero-width spaces), for every cell and title the
/// dashboard draws from text it did not write.
#[must_use]
pub fn one_line(s: &str) -> String {
    crate::util::one_line(&crate::diagnostics::display_line(s))
}

/// Runs the dashboard until the user quits.
///
/// `events` must be subscribed before the server starts so startup messages
/// are not lost.
///
/// # Errors
///
/// Fails if the terminal cannot be drawn to.
pub async fn run(
    state: AppState,
    events: tokio::sync::broadcast::Receiver<Event>,
    bootstrap: Option<String>,
    tunnel: bool,
) -> anyhow::Result<()> {
    run_until(state, events, bootstrap, tunnel, std::future::pending()).await
}

/// Like [`run`], but also stops when `stop` resolves, for example on a
/// termination signal. The terminal is restored either way.
///
/// # Errors
///
/// Fails if the terminal cannot be drawn to.
pub async fn run_until(
    state: AppState,
    events: tokio::sync::broadcast::Receiver<Event>,
    bootstrap: Option<String>,
    tunnel: bool,
    stop: impl Future<Output = ()>,
) -> anyhow::Result<()> {
    // `ratatui::init` installs a panic hook that restores the terminal.
    let mut terminal = ratatui::init();
    let app = App::new(state, bootstrap, tunnel);
    let result = event_loop(&mut terminal, app, events, stop).await;
    ratatui::restore();
    result
}

async fn event_loop(
    terminal: &mut DefaultTerminal,
    mut app: App,
    mut events: tokio::sync::broadcast::Receiver<Event>,
    stop: impl Future<Output = ()>,
) -> anyhow::Result<()> {
    let mut stop = std::pin::pin!(stop);
    let mut input = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Draw only when something changed: input, a bus event, a data refresh
    // or an animation frame. An idle dashboard costs almost nothing.
    let mut dirty = true;
    while !app.should_quit {
        if dirty {
            terminal.draw(|frame| ui::render(&mut app, frame))?;
            dirty = false;
        }
        tokio::select! {
            () = &mut stop => break,
            maybe = input.next() => match maybe {
                Some(Ok(TermEvent::Key(key))) => {
                    app.on_key(key);
                    if let Some(plan) = app.take_pending_command() {
                        // The stream's reader thread must stop before the
                        // command reads the terminal, or it eats keystrokes.
                        drop(input);
                        let done = plan.clone();
                        let (result, stopped) = run_attached(terminal, plan, stop.as_mut()).await;
                        input = EventStream::new();
                        app.command_finished(&done, result);
                        if stopped {
                            break;
                        }
                    }
                    dirty = true;
                }
                // Resize, focus and paste: redraw to be safe.
                Some(Ok(_)) => dirty = true,
                Some(Err(e)) => return Err(e.into()),
                None => break,
            },
            msg = events.recv() => {
                match msg {
                    Ok(ev) => app.on_event(ev),
                    Err(RecvError::Lagged(n)) => app.notify(format!("dropped {n} events")),
                    Err(RecvError::Closed) => break,
                }
                // Fold a burst of events into one frame.
                if !drain(&mut app, &mut events) {
                    break;
                }
                dirty = true;
            },
            _ = tick.tick() => dirty |= app.on_tick(),
        }
    }
    Ok(())
}

/// Leaves the dashboard, runs `cmd` with the terminal attached (so `sudo`
/// or an agent's sign-in can prompt), waits for Enter, then takes the
/// terminal back. The server keeps running throughout; bus events wait in
/// the subscription. Returns a line for the dashboard to show, and whether
/// `stop` resolved meanwhile. Signals that arrive while the command or the
/// prompt runs are read through `stop` (see [`attached`]).
async fn run_attached<S>(
    terminal: &mut DefaultTerminal,
    cmd: PendingCommand,
    stop: Pin<&mut S>,
) -> (Result<String, String>, bool)
where
    S: Future<Output = ()> + ?Sized,
{
    discard_type_ahead();
    ratatui::restore();
    let (result, stopped) = attached(stop, run_command(cmd)).await;
    if let Err(e) = reenter(terminal) {
        return (
            Err(format!("could not restore the dashboard: {e}")),
            stopped,
        );
    }
    discard_type_ahead();
    (result, stopped)
}

/// The attached part of [`run_attached`]: runs `cmd`, prints its outcome
/// and waits for Enter.
async fn run_command(cmd: PendingCommand) -> Result<String, String> {
    println!("\n  ✦ {}\n", cmd.title());
    let job = match cmd {
        PendingCommand::Install(plan) => {
            println!("  {}\n", plan.text().replace('\n', "\n  "));
            tokio::task::spawn_blocking(move || {
                crate::install::run(&plan).map(|()| "done.".to_owned())
            })
        }
        PendingCommand::SignIn {
            home,
            agent,
            name,
            bin,
        } => {
            if let Some(line) = crate::accounts::ops::sign_in_line(&home, agent, &name, &bin) {
                println!("  {line}\n");
            }
            if let Some(hint) = crate::accounts::sign_in(agent).and_then(|s| s.hint) {
                println!("  ({hint})\n");
            }
            tokio::task::spawn_blocking(move || {
                let clean = crate::accounts::ops::run_sign_in(&home, agent, &name, &bin)?;
                let state = crate::accounts::ops::signed_in(
                    &home,
                    agent,
                    &name,
                    crate::accounts::AccountKind::Login,
                    &bin,
                    true,
                );
                Ok(if clean {
                    state
                } else {
                    format!("{state} (the sign-in command did not exit cleanly)")
                })
            })
        }
        PendingCommand::Update { home } => tokio::task::spawn_blocking(move || update(&home)),
    };
    let result = job
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r.map_err(|e| format!("{e:#}")));
    match &result {
        Ok(msg) => println!("\n  {msg}"),
        Err(e) => println!("\n  {e}"),
    }
    println!("  Press Enter to go back to the dashboard.");
    let _ = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)
    })
    .await;
    result
}

/// `hashi update`, already confirmed in the dashboard.
fn update(home: &std::path::Path) -> anyhow::Result<String> {
    use crate::update::{Outcome, Sources};
    let exe = std::env::current_exe()?;
    let current = env!("CARGO_PKG_VERSION");
    Ok(
        match crate::update::install(home, &Sources::default(), &exe, None, false, &mut |_| true)? {
            Outcome::UpToDate(tag) => format!("hashi v{current} is already the latest ({tag})"),
            Outcome::Declined => "not updated".to_owned(),
            Outcome::Updated(tag) => {
                format!("updated to {tag}: quit (q) and start hashi again to use it")
            }
        },
    )
}

/// Takes the terminal back after [`ratatui::restore`].
fn reenter(terminal: &mut DefaultTerminal) -> std::io::Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen)?;
    terminal.clear()
}

/// Applies events that are already waiting, up to a bound so input stays
/// responsive. Returns `false` once the bus has closed.
pub fn drain(app: &mut App, events: &mut tokio::sync::broadcast::Receiver<Event>) -> bool {
    use tokio::sync::broadcast::error::TryRecvError;
    for _ in 0..256 {
        match events.try_recv() {
            Ok(ev) => app.on_event(ev),
            Err(TryRecvError::Lagged(n)) => app.notify(format!("dropped {n} events")),
            Err(TryRecvError::Empty) => return true,
            Err(TryRecvError::Closed) => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn interrupt_self() {
        let pid = std::process::id().to_string();
        let sent = std::process::Command::new("kill")
            .args(["-INT", &pid])
            .status()
            .expect("kill runs");
        assert!(sent.success());
    }

    /// A Ctrl-C meant for an install or sign-in, or pressed at the "Press
    /// Enter" prompt after it, reaches Hashi too. It must not stop Hashi once
    /// the dashboard is back; one that arrives afterwards still does.
    #[cfg(unix)]
    #[tokio::test]
    async fn ctrl_c_meant_for_an_attached_command_is_swallowed() {
        let mut signals = crate::shutdown::Signals::install().expect("signal handlers");
        let mut stop = std::pin::pin!(stop_unless_attached(&mut signals));

        // During the command, then during the prompt.
        for window in ["command", "prompt"] {
            let ((), stopped) = attached(stop.as_mut(), async {
                assert!(command_running());
                interrupt_self();
                tokio::time::sleep(Duration::from_millis(200)).await;
            })
            .await;
            assert!(!stopped, "{window}: the stop future resolved in the window");
            assert!(!command_running());
            let late = tokio::time::timeout(Duration::from_millis(500), stop.as_mut()).await;
            assert!(
                late.is_err(),
                "{window}: the Ctrl-C meant for the command stopped Hashi afterwards"
            );
        }

        // Back in the dashboard, a signal stops Hashi as before.
        interrupt_self();
        let stopped = tokio::time::timeout(Duration::from_secs(5), stop.as_mut()).await;
        assert!(
            stopped.is_ok(),
            "a signal outside the window must stop Hashi"
        );
    }

    #[test]
    fn type_ahead_is_discarded_until_nothing_is_pending() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut queue: Vec<TermEvent> = "abc\r"
            .chars()
            .map(|c| TermEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)))
            .collect();
        let pending = std::cell::Cell::new(queue.len());
        let n = discard_pending_input(
            || Ok(pending.get() > 0),
            || {
                pending.set(pending.get() - 1);
                queue.pop().ok_or_else(|| std::io::Error::other("empty"))
            },
        );
        assert_eq!(n, 4);
        assert_eq!(pending.get(), 0);

        // A stream that never runs dry is cut off, and errors stop it.
        let endless = discard_pending_input(|| Ok(true), || Ok(TermEvent::FocusGained));
        assert_eq!(endless, MAX_DISCARD);
        assert_eq!(
            discard_pending_input(|| Ok(true), || Err(std::io::Error::other("gone"))),
            0
        );
        assert_eq!(
            discard_pending_input(|| Err(std::io::Error::other("gone")), || unreachable!()),
            0
        );
    }

    #[test]
    fn one_line_drops_invisible_characters() {
        assert_eq!(one_line("a\u{202e}b\n\u{200b}c\u{feff}"), "ab c");
    }
}
