//! Diagnostics: secret redaction, the log file, error ids, crash reports and
//! the `hashi report` bundle.
//!
//! Everything here stays on the local machine. Nothing is uploaded: the user
//! reads the report and decides whether to attach it to an issue.

mod report;

pub use report::{ReportInput, build_report, issue_url};

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use time::OffsetDateTime;
use time::macros::format_description;
use tracing_subscriber::fmt::MakeWriter;

/// File name of the active log inside the logs directory.
pub const LOG_FILE: &str = "hashi.log";
/// The log is rotated, at startup and while running, once it grows past
/// this many bytes.
pub const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
/// How many rotated logs (`hashi.log.1` and up) are kept.
pub const KEEP_LOGS: usize = 3;
/// How many crash files are kept; older ones are removed.
pub const KEEP_CRASHES: usize = 10;

const MASK: &str = "[redacted]";

// ── redaction ──────────────────────────────────────────────────────────────

/// Masks secrets and the user's home directory in `input`.
///
/// Covers Hashi tokens and invite codes, `Bearer` credentials, Cloudflare
/// tunnel tokens (keyed or bare), common provider API keys, AWS access key
/// ids, Google OAuth access tokens and JWTs. Running it twice gives the same
/// result as running it once.
#[must_use]
pub fn redact(input: &str) -> String {
    static HOME: OnceLock<Option<String>> = OnceLock::new();
    let home = HOME.get_or_init(|| dirs::home_dir().map(|p| p.display().to_string()));
    redact_with_home(input, home.as_deref())
}

/// [`redact`] with an explicit home directory, so tests do not depend on the
/// machine they run on.
#[must_use]
pub fn redact_with_home(input: &str, home: Option<&str>) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < input.len() {
        let rest = &input[i..];
        let boundary = i == 0 || !is_word_byte(bytes[i - 1]);
        if let Some((consumed, replacement)) = match_secret(rest, boundary) {
            out.push_str(&replacement);
            i += consumed;
            continue;
        }
        let Some(ch) = rest.chars().next() else {
            break;
        };
        out.push(ch);
        i += ch.len_utf8();
    }
    match home {
        Some(home) => replace_home(&out, home),
        None => out,
    }
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_key_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

fn is_credential_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'+' | b'/' | b'=' | b'-')
}

/// Length of the leading run of bytes in `s` that satisfy `pred`.
fn run(s: &str, pred: impl Fn(u8) -> bool) -> usize {
    s.bytes().take_while(|b| pred(*b)).count()
}

fn starts_with_ignore_case(s: &str, prefix: &str) -> bool {
    s.len() >= prefix.len() && s.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

/// Tries each secret pattern at the start of `rest`. Returns the number of
/// bytes consumed and what to write instead.
fn match_secret(rest: &str, boundary: bool) -> Option<(usize, String)> {
    if let Some(m) = match_keyed_tunnel_token(rest) {
        return Some(m);
    }
    if !boundary {
        return None;
    }
    match_hashi(rest)
        .or_else(|| match_bearer(rest))
        .or_else(|| match_provider_key(rest))
        .or_else(|| match_aws_key(rest))
        .or_else(|| match_google_oauth(rest))
        .or_else(|| match_jwt(rest))
        .or_else(|| match_tunnel_blob(rest))
}

/// AWS access key ids: `AKIA` (long-term) or `ASIA` (temporary) followed by
/// exactly 16 uppercase letters or digits.
fn match_aws_key(rest: &str) -> Option<(usize, String)> {
    let prefix = ["AKIA", "ASIA"].into_iter().find(|p| rest.starts_with(p))?;
    let tail = &rest[prefix.len()..];
    let len = run(tail, |b| b.is_ascii_uppercase() || b.is_ascii_digit());
    let next = tail.as_bytes().get(len);
    if len != 16 || next.is_some_and(|b| is_word_byte(*b)) {
        return None;
    }
    Some((prefix.len() + len, format!("{prefix}{MASK}")))
}

/// Google OAuth access tokens, `ya29.` followed by a long base64url run.
fn match_google_oauth(rest: &str) -> Option<(usize, String)> {
    const PREFIX: &str = "ya29.";
    let tail = rest.strip_prefix(PREFIX)?;
    let len = run(tail, is_key_byte);
    (len >= 20).then(|| (PREFIX.len() + len, format!("{PREFIX}{MASK}")))
}

/// A bare Cloudflare tunnel token: base64 JSON (`eyJ`) with no dots, far
/// longer than any word. JWTs are matched first, by [`match_jwt`].
fn match_tunnel_blob(rest: &str) -> Option<(usize, String)> {
    if !rest.starts_with("eyJ") {
        return None;
    }
    let len = run(rest, |b| {
        b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'_' | b'-')
    });
    (len >= 80).then(|| (len, MASK.to_owned()))
}

/// `hashi_` + hex tokens and `hashi_inv_` + hex invite codes. Short display
/// prefixes such as `hashi_1a2b3c4d` are left alone.
fn match_hashi(rest: &str) -> Option<(usize, String)> {
    let head = if rest.starts_with("hashi_inv_") {
        "hashi_inv_".len()
    } else if rest.starts_with("hashi_") {
        "hashi_".len()
    } else {
        return None;
    };
    let hex = run(&rest[head..], |b| b.is_ascii_hexdigit());
    if hex < 32 {
        return None;
    }
    let consumed = head + hex;
    Some((consumed, format!("{}…{MASK}", &rest[..10])))
}

/// `Bearer <credential>`. A Hashi token after `Bearer` is left to
/// [`match_hashi`], which keeps its recognisable prefix.
fn match_bearer(rest: &str) -> Option<(usize, String)> {
    if !starts_with_ignore_case(rest, "bearer") {
        return None;
    }
    let after_word = &rest["bearer".len()..];
    let spaces = run(after_word, |b| b == b' ' || b == b'\t');
    if spaces == 0 {
        return None;
    }
    let value = &after_word[spaces..];
    if value.starts_with("hashi_") {
        return None;
    }
    let len = run(value, is_credential_byte);
    if len < 8 {
        return None;
    }
    let consumed = "bearer".len() + spaces + len;
    let kept = &rest[.."bearer".len() + spaces];
    Some((consumed, format!("{kept}{MASK}")))
}

/// `TUNNEL_TOKEN=<x>`, `--tunnel-token <x>` and `tunnel_token: Some("<x>")`.
fn match_keyed_tunnel_token(rest: &str) -> Option<(usize, String)> {
    if !(starts_with_ignore_case(rest, "tunnel_token")
        || starts_with_ignore_case(rest, "tunnel-token"))
    {
        return None;
    }
    let key_len = "tunnel_token".len();
    let after_key = &rest[key_len..];
    let mut sep = run(after_key, |b| {
        matches!(b, b' ' | b'=' | b':' | b'"' | b'\'')
    });
    if after_key[sep..].starts_with("Some(") {
        sep += "Some(".len();
        sep += run(&after_key[sep..], |b| b == b'"' || b == b'\'');
    }
    if sep == 0 {
        return None;
    }
    let value = &after_key[sep..];
    let len = run(value, |b| {
        b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'_' | b'.' | b'-')
    });
    let explicit = after_key[..sep].contains('=');
    let plausible = len >= 20 && value[..len].bytes().any(|b| b.is_ascii_digit());
    if len == 0 || !(explicit || plausible) {
        return None;
    }
    let consumed = key_len + sep + len;
    Some((consumed, format!("{}{MASK}", &rest[..key_len + sep])))
}

/// Well known provider key formats, keeping only the vendor prefix.
fn match_provider_key(rest: &str) -> Option<(usize, String)> {
    const KEYS: [(&str, usize); 12] = [
        ("sk-ant-", 16),
        ("sk-", 20),
        ("github_pat_", 20),
        ("ghp_", 20),
        ("gho_", 20),
        ("ghu_", 20),
        ("ghs_", 20),
        ("ghr_", 20),
        ("AIza", 30),
        ("xoxb-", 10),
        ("xoxp-", 10),
        ("xoxa-", 10),
    ];
    for (prefix, min) in KEYS {
        if let Some(tail) = rest.strip_prefix(prefix) {
            let len = run(tail, is_key_byte);
            if len >= min {
                return Some((prefix.len() + len, format!("{prefix}{MASK}")));
            }
        }
    }
    None
}

/// Three base64url segments joined by dots. Either it starts like a JWT
/// header (`eyJ`) or every segment is long enough to be noise, not prose.
fn match_jwt(rest: &str) -> Option<(usize, String)> {
    let is_b64 = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
    let mut lens = [0usize; 3];
    let mut pos = 0;
    for (n, len) in lens.iter_mut().enumerate() {
        if n > 0 {
            if rest.as_bytes().get(pos) != Some(&b'.') {
                return None;
            }
            pos += 1;
        }
        *len = run(&rest[pos..], is_b64);
        if *len == 0 {
            return None;
        }
        pos += *len;
    }
    // A fourth segment means a hostname or a dotted path, not a JWT.
    let bytes = rest.as_bytes();
    if bytes.get(pos) == Some(&b'.') && bytes.get(pos + 1).is_some_and(|b| is_b64(*b)) {
        return None;
    }
    let header = rest.starts_with("eyJ") && lens.iter().all(|l| *l >= 4) && pos >= 30;
    let noise = lens.iter().all(|l| *l >= 16) && pos >= 64;
    if header || noise {
        Some((pos, "[redacted-jwt]".to_owned()))
    } else {
        None
    }
}

/// Replaces `home` with `~` where it is a whole path prefix, so `/Users/sid`
/// does not touch `/Users/sidney`.
fn replace_home(s: &str, home: &str) -> String {
    let home = home.trim_end_matches(['/', '\\']);
    if home.len() < 2 {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for (start, _) in s.match_indices(home) {
        if start < last {
            continue;
        }
        let end = start + home.len();
        let next = s.as_bytes().get(end);
        let whole =
            next.is_none_or(|b| !(b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')));
        if whole {
            out.push_str(&s[last..start]);
            out.push('~');
            last = end;
        }
    }
    out.push_str(&s[last..]);
    out
}

// ── terminal output ────────────────────────────────────────────────────────

/// Longest error printed by [`headless_line`], in characters. The database
/// keeps the full text.
pub const HEADLESS_ERROR_CHARS: usize = 500;

/// Bidirectional and isolate controls (U+200E, U+200F, U+061C, U+202A to
/// U+202E and U+2066 to U+2069), which can make text display in a different
/// order from the bytes.
#[must_use]
pub fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{200e}' | '\u{200f}' | '\u{061c}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

/// Replaces every control character (C0, DEL and C1, so nothing can start an
/// escape sequence) and every bidi control with `?`. With `keep_lines`,
/// newlines and tabs stay; otherwise they become spaces.
#[must_use]
pub fn neutralize_controls(s: &str, keep_lines: bool) -> String {
    s.chars()
        .map(|c| match c {
            '\n' | '\t' if keep_lines => c,
            '\n' | '\t' | '\r' => ' ',
            c if c.is_control() || is_bidi_control(c) => '?',
            c => c,
        })
        .collect()
}

/// `s` made safe to print on one terminal line: control and bidi characters
/// are neutralised, then secrets and the home directory are redacted as in
/// the log.
#[must_use]
pub fn terminal_safe(s: &str) -> String {
    redact(&neutralize_controls(s, false))
}

/// [`terminal_safe`] with an explicit home directory, for tests.
#[must_use]
pub fn terminal_safe_with_home(s: &str, home: Option<&str>) -> String {
    redact_with_home(&neutralize_controls(s, false), home)
}

/// The line `hashi serve --headless` prints for an event. Every string in it
/// can come from an agent, a token holder or `cloudflared`, so all of it goes
/// through [`terminal_safe`].
#[must_use]
pub fn headless_line(ev: &crate::events::Event) -> String {
    use crate::events::Event;
    fn short(id: &str) -> &str {
        id.get(..8).unwrap_or(id)
    }
    let line = match ev {
        Event::Queued { id, agent, token } => {
            format!("  ◌ {} queued for {agent} by {token}", short(id))
        }
        Event::Started { id, agent } => format!("  ⠿ {agent} picked up {}", short(id)),
        Event::Finished {
            id,
            agent,
            ok: true,
            duration_ms,
            ..
        } => format!("  ✓ {agent} answered {} in {duration_ms}ms", short(id)),
        Event::Finished {
            id, agent, error, ..
        } => format!(
            "  ✗ {agent} failed {}: {}",
            short(id),
            crate::util::truncate(
                error.as_deref().unwrap_or("unknown error"),
                HEADLESS_ERROR_CHARS
            )
        ),
        Event::Held { reason, detail } => format!("  ! held     {reason}: {detail}"),
        Event::TunnelReady(url) => format!("  ⌁ public   {url}"),
        Event::TunnelDown(why) => format!("  ! tunnel   {why}"),
        Event::Log(msg) => format!("  · {msg}"),
    };
    terminal_safe(&line)
}

// ── private files ──────────────────────────────────────────────────────────

/// Writes `contents` to `path`, readable only by its owner on Unix. A file
/// that already existed is truncated and its mode tightened to 0600.
///
/// # Errors
///
/// Fails if the file cannot be opened, written or secured.
pub fn write_private(path: &Path, contents: &str) -> io::Result<()> {
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Only regular files: never chmod a device such as /dev/stdout.
        if file.metadata()?.is_file() {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
    }
    file.write_all(contents.as_bytes())?;
    file.flush()
}

// ── error ids ──────────────────────────────────────────────────────────────

/// A short random id such as `E-1a2b3c4d`, logged with a failure and shown to
/// the user so a report can be matched to the log lines.
#[must_use]
pub fn new_error_id() -> String {
    let bytes: [u8; 4] = rand::random();
    format!("E-{}", hex::encode(bytes))
}

// ── log file ───────────────────────────────────────────────────────────────

/// Rotates `dir/hashi.log` when it is larger than `max_bytes`: it becomes
/// `hashi.log.1`, older files shift up, and only `keep` old files remain.
/// Returns whether a rotation happened.
///
/// # Errors
///
/// Fails if a file cannot be renamed or removed.
pub fn rotate_logs(dir: &Path, max_bytes: u64, keep: usize) -> io::Result<bool> {
    let active = dir.join(LOG_FILE);
    let size = match std::fs::metadata(&active) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if size <= max_bytes {
        return Ok(false);
    }
    let old = |n: usize| dir.join(format!("{LOG_FILE}.{n}"));
    if keep == 0 {
        std::fs::remove_file(&active)?;
        return Ok(true);
    }
    match std::fs::remove_file(old(keep)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    for n in (1..keep).rev() {
        let from = old(n);
        if from.exists() {
            std::fs::rename(&from, old(n + 1))?;
        }
    }
    std::fs::rename(&active, old(1))?;
    Ok(true)
}

/// Opens `dir/hashi.log` for appending, readable only by its owner.
///
/// # Errors
///
/// Fails if the file cannot be opened.
pub fn open_log_file(dir: &Path) -> io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(dir.join(LOG_FILE))
}

/// `dir/hashi.log`, opened for appending, that rotates itself once it grows
/// past a size limit.
///
/// The check runs on [`flush`](Write::flush). [`RedactingWriter`] writes a
/// whole event and then flushes while it holds the writer lock, so a
/// rotation always falls between two events and never splits one.
#[derive(Debug)]
pub struct RotatingLog {
    dir: PathBuf,
    max_bytes: u64,
    keep: usize,
    file: Option<File>,
    size: u64,
}

impl RotatingLog {
    /// Opens `dir/hashi.log`, rotating at `max_bytes` and keeping `keep` old
    /// files.
    ///
    /// # Errors
    ///
    /// Fails if the file cannot be opened.
    pub fn open(dir: &Path, max_bytes: u64, keep: usize) -> io::Result<Self> {
        let file = open_log_file(dir)?;
        let size = file.metadata().map_or(0, |m| m.len());
        Ok(Self {
            dir: dir.to_path_buf(),
            max_bytes,
            keep,
            file: Some(file),
            size,
        })
    }

    fn file(&mut self) -> io::Result<&mut File> {
        if self.file.is_none() {
            let file = open_log_file(&self.dir)?;
            self.size = file.metadata().map_or(0, |m| m.len());
            self.file = Some(file);
        }
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("log file unavailable"))
    }

    fn rotate_if_needed(&mut self) {
        if self.size <= self.max_bytes {
            return;
        }
        // Close first: Windows will not always rename an open file.
        self.file = None;
        if rotate_logs(&self.dir, self.max_bytes, self.keep).is_err() {
            // Keep appending to the old file and try again after another
            // `max_bytes`, rather than on every line.
            self.size = 0;
        }
        // Reopened (and its size read) on the next write.
    }
}

impl Write for RotatingLog {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let n = self.file()?.write(data)?;
        self.size = self.size.saturating_add(n as u64);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(file) = self.file.as_mut() {
            file.flush()?;
        }
        self.rotate_if_needed();
        Ok(())
    }
}

/// A tracing writer factory that redacts every log line before writing it.
#[derive(Debug)]
pub struct RedactingMakeWriter<W> {
    inner: Arc<Mutex<W>>,
}

impl<W> Clone for RedactingMakeWriter<W> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<W: Write> RedactingMakeWriter<W> {
    /// Wraps `inner`.
    pub fn new(inner: W) -> Self {
        Self {
            inner: Arc::new(Mutex::new(inner)),
        }
    }
}

impl<'a, W: Write + 'a> MakeWriter<'a> for RedactingMakeWriter<W> {
    type Writer = RedactingWriter<W>;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriter {
            inner: Arc::clone(&self.inner),
            buf: Vec::new(),
        }
    }
}

/// Buffers one event and writes it, redacted, when dropped.
#[derive(Debug)]
pub struct RedactingWriter<W: Write> {
    inner: Arc<Mutex<W>>,
    buf: Vec<u8>,
}

impl<W: Write> RedactingWriter<W> {
    fn emit(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let text = redact(&String::from_utf8_lossy(&self.buf));
        self.buf.clear();
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| io::Error::other("log writer lock poisoned"))?;
        inner.write_all(text.as_bytes())?;
        inner.flush()
    }
}

impl<W: Write> Write for RedactingWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.emit()
    }
}

impl<W: Write> Drop for RedactingWriter<W> {
    fn drop(&mut self) {
        // Nowhere to report a failed log write; dropping the line is the
        // only option.
        let _ = self.emit();
    }
}

// ── crash reports ──────────────────────────────────────────────────────────

/// UTC timestamp for file names, such as `20261105T093531Z`.
#[must_use]
pub fn file_timestamp(now: OffsetDateTime) -> String {
    now.format(format_description!(
        "[year][month][day]T[hour][minute][second]Z"
    ))
    .unwrap_or_else(|_| now.unix_timestamp().to_string())
}

/// Version, OS, architecture and build target, one per line.
#[must_use]
pub fn build_info() -> String {
    format!(
        "hashi {}\nos: {} ({})\narch: {}\ntarget: {}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::FAMILY,
        std::env::consts::ARCH,
        env!("HASHI_BUILD_TARGET"),
    )
}

/// Installs a panic hook that saves a redacted crash report into `logs_dir`
/// and tells the user to run `hashi report`. The previous hook still runs,
/// so the dashboard's terminal restore keeps working.
pub fn install_panic_hook(logs_dir: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-string panic payload".to_owned());
        let location = info
            .location()
            .map_or_else(|| "unknown".to_owned(), ToString::to_string);
        let thread = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_owned();
        let backtrace = std::backtrace::Backtrace::force_capture();
        let text = crash_text(&message, &location, &thread, &backtrace.to_string());
        match write_crash(&logs_dir, &text, OffsetDateTime::now_utc()) {
            Ok(path) => eprintln!(
                "hashi crashed. Details are in {}. Run `hashi report` and attach the file to a GitHub issue.",
                redact(&path.display().to_string())
            ),
            Err(_) => eprintln!(
                "hashi crashed. Run `hashi report` and attach the file to a GitHub issue."
            ),
        }
    }));
}

/// The redacted body of a crash file.
#[must_use]
pub fn crash_text(message: &str, location: &str, thread: &str, backtrace: &str) -> String {
    let when = crate::util::rfc3339(crate::util::now_ms()).unwrap_or_default();
    redact(&format!(
        "Hashi crash report\ntime: {when}\n{}thread: {thread}\nlocation: {location}\n\npanic: {message}\n\nbacktrace:\n{backtrace}\n",
        build_info()
    ))
}

/// Writes `text` to `logs_dir/crash-<timestamp>.txt` without overwriting an
/// earlier crash from the same second.
///
/// # Errors
///
/// Fails if the directory or file cannot be created.
pub fn write_crash(logs_dir: &Path, text: &str, now: OffsetDateTime) -> io::Result<PathBuf> {
    std::fs::create_dir_all(logs_dir)?;
    let stamp = file_timestamp(now);
    for n in 0..100 {
        let name = if n == 0 {
            format!("crash-{stamp}.txt")
        } else {
            format!("crash-{stamp}-{n}.txt")
        };
        let path = logs_dir.join(name);
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        match opts.open(&path) {
            Ok(mut file) => {
                file.write_all(text.as_bytes())?;
                drop(file);
                // Best effort: a crash note matters more than tidying up.
                let _ = prune_crashes(logs_dir, KEEP_CRASHES);
                return Ok(path);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("too many crash files for one second"))
}

/// Sort key for a crash file name: its timestamp, then its same-second
/// counter. `None` for files that are not crash files.
fn crash_order(name: &str) -> Option<(String, u32)> {
    let stem = name.strip_prefix("crash-")?.strip_suffix(".txt")?;
    match stem.split_once('-') {
        Some((stamp, n)) => Some((stamp.to_owned(), n.parse().ok()?)),
        None => Some((stem.to_owned(), 0)),
    }
}

/// Removes all but the newest `keep` crash files in `logs_dir`. Returns how
/// many were removed.
///
/// # Errors
///
/// Fails if the directory cannot be read. Files that cannot be removed are
/// skipped.
pub fn prune_crashes(logs_dir: &Path, keep: usize) -> io::Result<usize> {
    let mut crashes: Vec<((String, u32), PathBuf)> = std::fs::read_dir(logs_dir)?
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name();
            let order = crash_order(name.to_str()?)?;
            Some((order, e.path()))
        })
        .collect();
    if crashes.len() <= keep {
        return Ok(0);
    }
    crashes.sort_by(|a, b| b.0.cmp(&a.0));
    let mut removed = 0;
    for (_, path) in crashes.into_iter().skip(keep) {
        if std::fs::remove_file(path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/Users/alice";

    fn r(s: &str) -> String {
        redact_with_home(s, Some(HOME))
    }

    fn token() -> String {
        format!("hashi_{}", "1a2b3c4d".repeat(8))
    }

    #[test]
    fn masks_hashi_tokens_keeping_ten_chars() {
        let out = r(&format!("token {} created", token()));
        assert_eq!(out, "token hashi_1a2b…[redacted] created");
    }

    #[test]
    fn masks_invite_codes() {
        let code = format!("hashi_inv_{}", "ab".repeat(32));
        let out = r(&format!("https://h.example.com/claim/{code}"));
        assert_eq!(out, "https://h.example.com/claim/hashi_inv_…[redacted]");
    }

    #[test]
    fn leaves_display_prefixes_alone() {
        let s = "revoked hashi_1a2b3c4d";
        assert_eq!(r(s), s);
    }

    #[test]
    fn masks_bearer_credentials() {
        assert_eq!(
            r("Authorization: Bearer abcDEF123456789xyz"),
            "Authorization: Bearer [redacted]"
        );
        assert_eq!(
            r(&format!("authorization: bearer {}", token())),
            "authorization: bearer hashi_1a2b…[redacted]"
        );
    }

    #[test]
    fn keeps_bearer_placeholders_and_prose() {
        let s = "missing Authorization: Bearer <token> header";
        assert_eq!(r(s), s);
        let s = "the bearer of bad news";
        assert_eq!(r(s), s);
    }

    #[test]
    fn masks_tunnel_tokens() {
        assert_eq!(r("TUNNEL_TOKEN=eyJhIjoiMTIz"), "TUNNEL_TOKEN=[redacted]");
        assert_eq!(
            r("HASHI_TUNNEL_TOKEN=abc123 hashi serve"),
            "HASHI_TUNNEL_TOKEN=[redacted] hashi serve"
        );
        assert_eq!(
            r("cloudflared tunnel run --tunnel-token AbC123dEf456GhI789jKl0 x"),
            "cloudflared tunnel run --tunnel-token [redacted] x"
        );
        assert_eq!(r("--tunnel-token=short1"), "--tunnel-token=[redacted]");
        assert_eq!(
            r(r#"tunnel_token: Some("AbC123dEf456GhI789jKl0")"#),
            r#"tunnel_token: Some("[redacted]")"#
        );
    }

    #[test]
    fn keeps_tunnel_token_prose() {
        let s = "--tunnel-token requires --public-url";
        assert_eq!(r(s), s);
        let s = "--tunnel-token <TUNNEL_TOKEN>";
        assert_eq!(r(s), s);
    }

    #[test]
    fn masks_provider_keys() {
        let cases = [
            ("sk-ant-api03-AbCdEfGhIjKlMnOpQrSt", "sk-ant-[redacted]"),
            ("sk-proj-AbCdEfGhIjKlMnOpQrStUv", "sk-[redacted]"),
            ("ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123", "ghp_[redacted]"),
            (
                "github_pat_11ABCDEFG0123456789_abcdef",
                "github_pat_[redacted]",
            ),
            ("AIzaSyAbCdEfGhIjKlMnOpQrStUvWxYz0123456", "AIza[redacted]"),
            ("xoxb-1234567890-abcdefghij", "xoxb-[redacted]"),
            ("xoxp-1234567890-abcdefghij", "xoxp-[redacted]"),
        ];
        for (input, want) in cases {
            assert_eq!(
                r(&format!("key={input};")),
                format!("key={want};"),
                "{input}"
            );
        }
    }

    #[test]
    fn masks_jwts() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        assert_eq!(
            r(&format!("id_token={jwt} ok")),
            "id_token=[redacted-jwt] ok"
        );
    }

    #[test]
    fn keeps_hostnames_and_versions() {
        for s in [
            "https://quiet-river-1234.trycloudflare.com/v1/ask",
            "claude 2.0.31 (Claude Code)",
            "hashi.example.com",
            "src/api/mod.rs:42",
        ] {
            assert_eq!(r(s), s);
        }
    }

    #[test]
    fn replaces_home_directory() {
        assert_eq!(
            r("opening /Users/alice/.hashi/hashi.db"),
            "opening ~/.hashi/hashi.db"
        );
        assert_eq!(r("/Users/alice"), "~");
        assert_eq!(r("/Users/alicecooper/x"), "/Users/alicecooper/x");
        assert_eq!(redact_with_home("/Users/alice/x", None), "/Users/alice/x");
    }

    #[test]
    fn plain_text_is_untouched() {
        let s = "2026-11-05T09:35:31Z  INFO hashi::queue: request finished id=4f1c2d3e ok=true \
                 duration_ms=1234 橋 task-runner skeleton bearer";
        assert_eq!(r(s), s);
    }

    #[test]
    fn redaction_is_idempotent() {
        let s = format!(
            "Bearer {t} Bearer abcdefghijklmnop TUNNEL_TOKEN=abc --tunnel-token AbC123dEf456GhI789jKl0 \
             sk-ant-api03-AbCdEfGhIjKlMnOpQrSt ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123 \
             eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N \
             hashi_inv_{h} /Users/alice/x",
            t = token(),
            h = "cd".repeat(32)
        );
        let once = r(&s);
        assert_eq!(r(&once), once);
        assert!(!once.contains("1a2b3c4d1a2b"));
        assert!(!once.contains("/Users/alice"));
    }

    #[test]
    fn rotation_shifts_and_caps_old_logs() {
        let dir = tempfile::tempdir().unwrap();
        let p = |name: &str| dir.path().join(name);
        // Small files are left alone.
        std::fs::write(p(LOG_FILE), "tiny").unwrap();
        assert!(!rotate_logs(dir.path(), 10, 3).unwrap());

        for gen_ in 1..=5 {
            std::fs::write(p(LOG_FILE), format!("generation {gen_} {}", "x".repeat(20))).unwrap();
            assert!(rotate_logs(dir.path(), 10, 3).unwrap());
        }
        assert!(!p(LOG_FILE).exists());
        let read = |n: u8| std::fs::read_to_string(p(&format!("{LOG_FILE}.{n}"))).unwrap();
        assert!(read(1).starts_with("generation 5"));
        assert!(read(2).starts_with("generation 4"));
        assert!(read(3).starts_with("generation 3"));
        assert!(!p(&format!("{LOG_FILE}.4")).exists());
    }

    #[test]
    fn rotation_ignores_a_missing_log() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!rotate_logs(dir.path(), 10, 3).unwrap());
    }

    #[test]
    fn writer_redacts_each_event() {
        let dir = tempfile::tempdir().unwrap();
        let file = open_log_file(dir.path()).unwrap();
        let make = RedactingMakeWriter::new(file);
        {
            let mut w = make.make_writer();
            writeln!(w, "created {}", token()).unwrap();
        }
        let text = std::fs::read_to_string(dir.path().join(LOG_FILE)).unwrap();
        assert_eq!(text, "created hashi_1a2b…[redacted]\n");
    }

    #[test]
    fn writer_rotates_while_running_and_keeps_redacting() {
        let dir = tempfile::tempdir().unwrap();
        let p = |name: &str| dir.path().join(name);
        let make = RedactingMakeWriter::new(RotatingLog::open(dir.path(), 200, 3).unwrap());
        for n in 0..40 {
            let mut w = make.make_writer();
            writeln!(w, "line {n:02} created {} {}", token(), "x".repeat(40)).unwrap();
        }
        assert!(p(&format!("{LOG_FILE}.1")).exists());
        assert!(p(&format!("{LOG_FILE}.3")).exists());
        assert!(!p(&format!("{LOG_FILE}.4")).exists());
        // Still writing to a fresh active file after rotating.
        {
            let mut w = make.make_writer();
            writeln!(w, "after rotation").unwrap();
        }
        let active = std::fs::read_to_string(p(LOG_FILE)).unwrap();
        assert!(active.ends_with("after rotation\n"));
        for name in [
            LOG_FILE.to_owned(),
            format!("{LOG_FILE}.1"),
            format!("{LOG_FILE}.2"),
            format!("{LOG_FILE}.3"),
        ] {
            let text = std::fs::read_to_string(p(&name)).unwrap();
            assert!(!text.contains("1a2b3c4d1a2b"), "{name} leaked a token");
            for line in text.lines().filter(|l| l.starts_with("line")) {
                assert!(line.contains("hashi_1a2b…[redacted]"), "{name}: {line}");
            }
            // Each rotated file passed the limit by at most one line.
            if name != LOG_FILE {
                assert!(
                    text.len() > 200 && text.len() < 400,
                    "{name}: {}",
                    text.len()
                );
            }
        }
        let newest = std::fs::read_to_string(p(&format!("{LOG_FILE}.1"))).unwrap();
        assert!(newest.contains("line 39") || active.contains("line 39"));
    }

    #[test]
    fn concurrent_writes_rotate_without_splitting_lines() {
        let dir = tempfile::tempdir().unwrap();
        let make = RedactingMakeWriter::new(RotatingLog::open(dir.path(), 4096, 100).unwrap());
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let make = make.clone();
                std::thread::spawn(move || {
                    for n in 0..200 {
                        let mut w = make.make_writer();
                        // Several writes per event, as tracing does.
                        write!(w, "t{t} ").unwrap();
                        write!(w, "n{n:03} ").unwrap();
                        writeln!(w, "{}", "y".repeat(50)).unwrap();
                    }
                })
            })
            .collect();
        for th in threads {
            th.join().unwrap();
        }
        let mut lines = 0;
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
            for line in text.lines() {
                let parts: Vec<&str> = line.split(' ').collect();
                assert_eq!(parts.len(), 3, "split line: {line:?}");
                assert_eq!(parts[2].len(), 50, "split line: {line:?}");
                lines += 1;
            }
        }
        assert_eq!(lines, 8 * 200);
    }

    #[test]
    fn crash_files_are_capped_at_ten() {
        let dir = tempfile::tempdir().unwrap();
        // Unrelated files are left alone.
        std::fs::write(dir.path().join(LOG_FILE), "log").unwrap();
        let base = OffsetDateTime::UNIX_EPOCH;
        let mut paths = Vec::new();
        for n in 0..12 {
            let at = base + time::Duration::seconds(n / 2);
            paths.push(write_crash(dir.path(), &format!("crash {n}"), at).unwrap());
        }
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.starts_with("crash-"))
            .collect();
        assert_eq!(left.len(), KEEP_CRASHES);
        assert!(!paths[0].exists() && !paths[1].exists());
        assert!(paths[2..].iter().all(|p| p.exists()));
        assert!(dir.path().join(LOG_FILE).exists());
    }

    #[test]
    fn error_ids_look_right() {
        let id = new_error_id();
        assert_eq!(id.len(), 10);
        assert!(id.starts_with("E-"));
        assert!(id[2..].bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn crash_files_do_not_overwrite_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let now = OffsetDateTime::UNIX_EPOCH;
        let a = write_crash(dir.path(), "one", now).unwrap();
        let b = write_crash(dir.path(), "two", now).unwrap();
        assert_ne!(a, b);
        assert_eq!(
            a.file_name().unwrap().to_str().unwrap(),
            "crash-19700101T000000Z.txt"
        );
    }

    #[test]
    fn masks_aws_access_keys() {
        assert_eq!(
            r("aws_access_key_id = AKIAIOSFODNN7EXAMPLE end"),
            "aws_access_key_id = AKIA[redacted] end"
        );
        assert_eq!(r("ASIAIOSFODNN7EXAMPLE"), "ASIA[redacted]");
        // Too short, too long or lowercase: not a key.
        for s in [
            "AKIAIOSFODNN7",
            "AKIAIOSFODNN7EXAMPLEX",
            "AKIAiosfodnn7example",
        ] {
            assert_eq!(r(s), s);
        }
    }

    #[test]
    fn masks_google_oauth_tokens() {
        assert_eq!(
            r("token ya29.a0AfH6SMBx3kLmNoPqRsTuVwXyZ0123456789-_abc ok"),
            "token ya29.[redacted] ok"
        );
        let s = "ya29.short";
        assert_eq!(r(s), s);
    }

    #[test]
    fn masks_unkeyed_cloudflare_tunnel_tokens() {
        // Base64 of {"a":"<account>","t":"<tunnel>","s":"<secret>"}: no dots.
        let token = "eyJhIjoiMDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWYiLCJ0IjoiMDEyMzQ1NjctODlhYi1jZGVmLTAxMjMtNDU2Nzg5YWJjZGVmIiwicyI6IlpHVm1ZWFZzZEhObFkzSmxkQT09In0=";
        assert_eq!(
            r(&format!("cloudflared tunnel run {token} now")),
            "cloudflared tunnel run [redacted] now"
        );
        // JWTs keep their own label, and short eyJ strings are left alone.
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        assert_eq!(r(jwt), "[redacted-jwt]");
        assert_eq!(r("eyJhIjoiMTIz"), "eyJhIjoiMTIz");
    }

    #[test]
    fn new_patterns_are_idempotent() {
        let s = "AKIAIOSFODNN7EXAMPLE ya29.a0AfH6SMBx3kLmNoPqRsTuVwXyZ0123456789 \
                 eyJhIjoiMDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWYiLCJ0IjoiMDEyMzQ1NjctODlhYi1jZGVmLTAxMjMtNDU2Nzg5YWJjZGVmIn0=";
        let once = r(s);
        assert_eq!(r(&once), once);
    }

    fn has_unsafe_terminal_chars(s: &str) -> bool {
        s.chars().any(|c| c.is_control() || is_bidi_control(c))
    }

    #[test]
    fn terminal_text_has_no_escape_or_bidi_characters() {
        let hostile = "boom \u{1b}]0;owned\u{7}\u{1b}[2J\u{1b}[H\u{1b}]52;c;aGk=\u{7} \u{9b}31m \
                       \u{202e}txt.exe\u{202c} \u{2066}x\u{2069}\r\nnext\tline\u{7f}";
        let out = terminal_safe_with_home(hostile, None);
        assert!(!has_unsafe_terminal_chars(&out), "{out:?}");
        assert!(out.starts_with("boom "));
        assert!(out.contains("next line"));
    }

    #[test]
    fn terminal_text_is_redacted() {
        let out = terminal_safe_with_home(
            &format!("failed with {} in /Users/alice/bin", token()),
            Some(HOME),
        );
        assert_eq!(out, "failed with hashi_1a2b…[redacted] in ~/bin");
        // A control character cannot hide a secret from the redactor.
        let out = terminal_safe_with_home(&format!("x\u{1b}{}", token()), Some(HOME));
        assert!(!out.contains("1a2b3c4d1a2b"), "{out}");
    }

    #[test]
    fn headless_lines_are_sanitised() {
        use crate::agent::AgentKind;
        use crate::events::Event;
        let ev = Event::Finished {
            id: "0123456789abcdef".into(),
            agent: AgentKind::Claude,
            ok: false,
            duration_ms: 5,
            error: Some(format!(
                "agent exited with 1: \u{1b}]0;pwned\u{7}\u{1b}[2J {}",
                token()
            )),
        };
        let line = headless_line(&ev);
        assert!(!has_unsafe_terminal_chars(&line), "{line:?}");
        assert!(line.starts_with("  ✗ claude failed 01234567: agent exited with 1: "));
        assert!(!line.contains("1a2b3c4d1a2b"));

        let ev = Event::Queued {
            id: "abc".into(),
            agent: AgentKind::Codex,
            token: "bob\u{1b}[31m\u{202e}".into(),
        };
        let line = headless_line(&ev);
        assert!(!has_unsafe_terminal_chars(&line), "{line:?}");
        assert!(line.contains("queued for codex by bob"));
        for ev in [
            Event::TunnelDown("x\u{1b}[2Jy".into()),
            Event::TunnelReady("https://a\u{7}.example".into()),
            Event::Log("a\u{9b}b".into()),
        ] {
            assert!(!has_unsafe_terminal_chars(&headless_line(&ev)));
        }
    }

    #[test]
    fn headless_errors_are_capped() {
        use crate::agent::AgentKind;
        use crate::events::Event;
        let ev = Event::Finished {
            id: "id".into(),
            agent: AgentKind::Claude,
            ok: false,
            duration_ms: 5,
            error: Some("x".repeat(4_000)),
        };
        assert!(headless_line(&ev).chars().count() < HEADLESS_ERROR_CHARS + 40);
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    #[test]
    fn crash_files_are_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_crash(dir.path(), "boom", OffsetDateTime::UNIX_EPOCH).unwrap();
        assert_eq!(mode(&path), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn private_files_are_owner_only_even_when_they_existed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.md");
        write_private(&path, "one").unwrap();
        assert_eq!(mode(&path), 0o600);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&path, "two").unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
    }

    #[test]
    fn crash_text_is_redacted() {
        let text = crash_text(&format!("bad {}", token()), "src/x.rs:1:1", "main", "");
        assert!(text.contains("hashi_1a2b…[redacted]"));
        assert!(text.contains(env!("CARGO_PKG_VERSION")));
    }
}
