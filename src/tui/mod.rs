//! Live terminal dashboard built on ratatui.

pub mod access;
pub mod app;
mod clipboard;
pub mod theme;
pub mod ui;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::event::{Event as TermEvent, EventStream};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use tokio::sync::broadcast::error::RecvError;

pub use app::App;

use crate::api::AppState;
use crate::events::Event;

/// Set while a command runs with the terminal attached (an install from the
/// Commands view). Ctrl-C then belongs to that command, so the caller's stop
/// signal should be ignored: see [`command_running`].
static COMMAND_RUNNING: AtomicBool = AtomicBool::new(false);

/// Whether the dashboard has stepped aside for a command right now. A
/// Ctrl-C that arrives meanwhile is meant for the command, not for Hashi.
#[must_use]
pub fn command_running() -> bool {
    COMMAND_RUNNING.load(Ordering::Acquire)
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
                        let helper = plan.helper();
                        let result = run_attached(terminal, plan).await;
                        input = EventStream::new();
                        app.command_finished(helper, result);
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

/// Leaves the dashboard, runs `plan` with the terminal attached (so `sudo`
/// can prompt), waits for Enter, then takes the terminal back. The server
/// keeps running throughout; bus events wait in the subscription.
async fn run_attached(
    terminal: &mut DefaultTerminal,
    plan: crate::install::Plan,
) -> Result<(), String> {
    ratatui::restore();
    COMMAND_RUNNING.store(true, Ordering::Release);
    let helper = plan.helper();
    println!("\n  ✦ hashi install {helper}\n");
    println!("  {}\n", plan.text().replace('\n', "\n  "));
    let result = tokio::task::spawn_blocking(move || crate::install::run(&plan))
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r.map_err(|e| format!("{e:#}")));
    match &result {
        Ok(()) => println!("\n  done."),
        Err(e) => println!("\n  {e}"),
    }
    println!("  Press Enter to go back to the dashboard.");
    let _ = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)
    })
    .await;
    COMMAND_RUNNING.store(false, Ordering::Release);
    if let Err(e) = reenter(terminal) {
        return Err(format!("could not restore the dashboard: {e}"));
    }
    result
}

/// Takes the terminal back after [`ratatui::restore`].
fn reenter(terminal: &mut DefaultTerminal) -> std::io::Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen)?;
    terminal.clear()
}

/// Applies events that are already waiting, up to a bound so input stays
/// responsive. Returns `false` once the bus has closed.
fn drain(app: &mut App, events: &mut tokio::sync::broadcast::Receiver<Event>) -> bool {
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
