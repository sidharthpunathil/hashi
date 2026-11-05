//! Copies text to the system clipboard.
//!
//! Native tools (`pbcopy`, `wl-copy`, `xclip`) are tried first because they
//! work in every terminal. OSC 52 is the fallback and also works over SSH in
//! terminals that support it.

use std::io::Write;
use std::process::{Command, Stdio};

/// Copies `text`, returning a human-readable reason on failure.
///
/// # Errors
///
/// Fails when neither a native tool nor OSC 52 could be used.
pub fn copy(text: &str) -> Result<(), String> {
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
