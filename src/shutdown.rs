//! Termination signals for `hashi serve`.
//!
//! The server stops gracefully (every run's process group killed, its
//! request put back in the queue, the tunnel closed) on Ctrl-C, and also on
//! SIGTERM and SIGHUP on Unix, or when the console closes or the system shuts
//! down on Windows. A second signal while it is stopping exits at once.

use std::io;

/// Handlers for every signal that should stop the server. Installing them
/// replaces the default action (immediate death) for the process.
#[derive(Debug)]
pub struct Signals {
    #[cfg(unix)]
    int: tokio::signal::unix::Signal,
    #[cfg(unix)]
    term: tokio::signal::unix::Signal,
    #[cfg(unix)]
    hup: tokio::signal::unix::Signal,
    #[cfg(windows)]
    ctrl_c: tokio::signal::windows::CtrlC,
    #[cfg(windows)]
    ctrl_break: tokio::signal::windows::CtrlBreak,
    #[cfg(windows)]
    close: tokio::signal::windows::CtrlClose,
    #[cfg(windows)]
    shutdown: tokio::signal::windows::CtrlShutdown,
}

impl Signals {
    /// Installs the handlers. Must be called inside a Tokio runtime.
    ///
    /// # Errors
    ///
    /// Fails if a handler cannot be registered.
    #[cfg(unix)]
    pub fn install() -> io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            int: signal(SignalKind::interrupt())?,
            term: signal(SignalKind::terminate())?,
            hup: signal(SignalKind::hangup())?,
        })
    }

    /// Installs the handlers. Must be called inside a Tokio runtime.
    ///
    /// # Errors
    ///
    /// Fails if a handler cannot be registered.
    #[cfg(windows)]
    pub fn install() -> io::Result<Self> {
        use tokio::signal::windows;
        Ok(Self {
            ctrl_c: windows::ctrl_c()?,
            ctrl_break: windows::ctrl_break()?,
            close: windows::ctrl_close()?,
            shutdown: windows::ctrl_shutdown()?,
        })
    }

    /// Installs nothing on platforms without signal support.
    ///
    /// # Errors
    ///
    /// Never fails.
    #[cfg(not(any(unix, windows)))]
    pub fn install() -> io::Result<Self> {
        Ok(Self {})
    }

    /// Waits for the next stop signal and returns its name.
    #[cfg(unix)]
    pub async fn recv(&mut self) -> &'static str {
        tokio::select! {
            _ = self.int.recv() => "interrupt",
            _ = self.term.recv() => "SIGTERM",
            _ = self.hup.recv() => "SIGHUP",
        }
    }

    /// Waits for the next stop signal and returns its name.
    #[cfg(windows)]
    pub async fn recv(&mut self) -> &'static str {
        tokio::select! {
            _ = self.ctrl_c.recv() => "interrupt",
            _ = self.ctrl_break.recv() => "Ctrl-Break",
            _ = self.close.recv() => "console closed",
            _ = self.shutdown.recv() => "system shutdown",
        }
    }

    /// Waits forever on platforms without signal support.
    #[cfg(not(any(unix, windows)))]
    pub async fn recv(&mut self) -> &'static str {
        std::future::pending().await
    }
}

/// Exit status used when a second signal cuts the graceful stop short.
pub const FORCED_EXIT_CODE: i32 = 130;

/// Spawns a task that exits the process at once on the next stop signal,
/// for use while a graceful shutdown is in progress. Workers drop their runs
/// (killing each process group) as soon as the shutdown starts, so this only
/// cuts short the remaining wait, for example on a stalled connection.
pub fn exit_on_next(mut signals: Signals) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let name = signals.recv().await;
        eprintln!("\n{name} again, exiting now");
        std::process::exit(FORCED_EXIT_CODE);
    })
}
