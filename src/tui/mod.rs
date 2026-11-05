//! Live terminal dashboard built on ratatui.

pub mod app;
mod clipboard;
pub mod theme;
pub mod ui;

use std::time::Duration;

use crossterm::event::{Event as TermEvent, EventStream};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use tokio::sync::broadcast::error::RecvError;

pub use app::App;

use crate::api::AppState;
use crate::events::Event;

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
    // `ratatui::init` installs a panic hook that restores the terminal.
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, App::new(state, bootstrap, tunnel), events).await;
    ratatui::restore();
    result
}

async fn event_loop(
    terminal: &mut DefaultTerminal,
    mut app: App,
    mut events: tokio::sync::broadcast::Receiver<Event>,
) -> anyhow::Result<()> {
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
            maybe = input.next() => match maybe {
                Some(Ok(TermEvent::Key(key))) => {
                    app.on_key(key);
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
