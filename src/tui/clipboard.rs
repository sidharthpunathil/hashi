//! Copies text to the system clipboard.
//!
//! Native tools (`pbcopy`, `wl-copy`, `xclip`) are tried first because they
//! work in every terminal. OSC 52 is the fallback and also works over SSH in
//! terminals that support it.

use std::cell::RefCell;
use std::io::Write;
use std::process::{Command, Stdio};

thread_local! {
    /// Where [`copy`] puts text on this thread while [`capture`] is on.
    static CAPTURED: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

/// A test seam: until the returned guard drops, [`copy`] on this thread
/// records the text instead of touching the clipboard.
#[must_use]
pub fn capture() -> Capture {
    CAPTURED.with(|c| *c.borrow_mut() = Some(Vec::new()));
    Capture(())
}

/// Turns [`capture`] off when dropped.
#[derive(Debug)]
pub struct Capture(());

impl Capture {
    /// Every text copied on this thread since [`capture`], oldest first.
    #[must_use]
    pub fn copied(&self) -> Vec<String> {
        CAPTURED.with(|c| c.borrow().clone().unwrap_or_default())
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        CAPTURED.with(|c| *c.borrow_mut() = None);
    }
}

/// Copies `text`, returning a human-readable reason on failure.
///
/// # Errors
///
/// Fails when neither a native tool nor OSC 52 could be used.
pub fn copy(text: &str) -> Result<(), String> {
    let captured = CAPTURED.with(|c| {
        c.borrow_mut()
            .as_mut()
            .map(|list| list.push(text.to_owned()))
            .is_some()
    });
    if captured {
        return Ok(());
    }
    const TOOLS: [(&str, &[&str]); 5] = [
        ("pbcopy", &[]),
        ("clip", &[]),
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
    ];
    for (tool, args) in TOOLS {
        if let Some(bin) = crate::agent::locate(tool)
            && pipe(&bin, args, text).is_ok()
        {
            return Ok(());
        }
    }
    crossterm::execute!(
        std::io::stdout(),
        crossterm::clipboard::CopyToClipboard::to_clipboard_from(text)
    )
    .map_err(|e| e.to_string())
}

fn pipe(bin: &std::path::Path, args: &[&str], text: &str) -> std::io::Result<()> {
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())?;
    }
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(status.to_string()))
    }
}
