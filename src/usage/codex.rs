//! Codex's live quota, read from a short-lived `codex app-server` started
//! with the account's own `CODEX_HOME`.
//!
//! The app-server speaks line-delimited JSON-RPC on stdio, without a
//! `jsonrpc` field:
//!
//! 1. `{"method":"initialize","id":0,"params":{"clientInfo":{...}}}`; the
//!    answer carries `codexHome`, which must be the account's directory;
//! 2. the notification `{"method":"initialized"}`;
//! 3. `{"method":"account/rateLimits/read","id":1,"params":
//!    {"excludeResetCreditDetails":true}}`, answered with
//!    `ordinaryUsageAllowed`, `rateLimits` (`primary` and `secondary`
//!    windows of `{usedPercent, windowDurationMins, resetsAt}` in Unix
//!    seconds, `planType`, `credits`, `rateLimitReachedType`) and
//!    `rateLimitsByLimitId`;
//! 4. the process is stopped.
//!
//! Codex refreshes its own token while it reads; Hashi only reads the
//! answer. The caller skips the read while a run on the same account is
//! active, so two refreshes never race.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::{Source, Window, clamp_percent, duration_label};
use crate::agent::EnvOverride;

/// The longest a whole read may take.
pub const TIMEOUT: Duration = Duration::from_secs(20);

/// The longest line Hashi reads from the app-server. An answer is a few
/// kilobytes; a longer line is refused instead of buffered without bound.
pub const MAX_LINE: usize = 1024 * 1024;

/// Directory under the Hashi home that holds each read's working
/// directory.
pub const WORK_DIR: &str = "codex-app-server";

/// What one read found.
#[derive(Debug, Clone, PartialEq)]
pub struct Quota {
    /// The windows.
    pub windows: Vec<Window>,
    /// The plan, such as `plus` or `pro`.
    pub plan: Option<String>,
    /// Set when Codex says the limit is reached: until the latest reset of
    /// a window, Unix ms, or `Some(None)` when it gives none.
    pub reached: Option<Option<i64>>,
}

/// Starts `bin app-server` with `env` applied, reads the account's rate
/// limits and stops it. `expected_home` is the `CODEX_HOME` the account
/// uses; the server's `codexHome` must be the same directory. The server
/// runs in a new, empty, owner-only directory under `<home>/`
/// [`WORK_DIR`], removed afterwards, in its own process group (a Job
/// Object on Windows) that is killed when the read ends, times out or is
/// dropped.
///
/// # Errors
///
/// Returns a message for a server that cannot start, answers out of turn,
/// reports another home, reports an error, sends a line over [`MAX_LINE`]
/// or times out. The message never contains a credential.
pub async fn read(
    bin: &Path,
    env: Option<&EnvOverride>,
    expected_home: Option<&Path>,
    home: &Path,
) -> Result<Quota, String> {
    read_within(bin, env, expected_home, home, TIMEOUT).await
}

/// [`read`] with its own time limit.
async fn read_within(
    bin: &Path,
    env: Option<&EnvOverride>,
    expected_home: Option<&Path>,
    home: &Path,
    limit: Duration,
) -> Result<Quota, String> {
    let work = WorkDir::create(home)
        .map_err(|e| format!("codex app-server has no working directory: {}", e.kind()))?;
    let result = tokio::time::timeout(limit, read_inner(bin, env, expected_home, &work.0)).await;
    drop(work);
    match result {
        Ok(r) => r,
        Err(_) => Err("codex app-server did not answer in time".to_owned()),
    }
}

/// A fresh working directory for one read, removed on drop.
struct WorkDir(PathBuf);

impl WorkDir {
    fn create(home: &Path) -> std::io::Result<Self> {
        let parent = home.join(WORK_DIR);
        if std::fs::symlink_metadata(&parent).is_ok_and(|m| !m.is_dir()) {
            std::fs::remove_file(&parent)?;
        }
        private_dir(&parent)?;
        let dir = parent.join(uuid::Uuid::new_v4().simple().to_string());
        private_dir(&dir)?;
        Ok(Self(dir))
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        // `remove_dir_all` does not follow symlinks.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Creates `dir` (not its parents) owner-only.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    match std::fs::create_dir(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => {}
        Err(e) => return Err(e),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

async fn read_inner(
    bin: &Path,
    env: Option<&EnvOverride>,
    expected_home: Option<&Path>,
    cwd: &Path,
) -> Result<Quota, String> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.arg("app-server")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // The same hygiene as a run: Hashi's own secrets never reach the child,
    // and an account drops the owner's credential variables first.
    let remove: &[&str] = env.map_or(&[], |e| e.remove.as_slice());
    for (key, _) in std::env::vars_os() {
        let upper = key.to_string_lossy().to_ascii_uppercase();
        if crate::agent::runner::is_hashi_secret(&key)
            || remove
                .iter()
                .any(|r| crate::agent::runner::env_name_matches(r, &upper))
        {
            cmd.env_remove(&key);
        }
    }
    cmd.envs(crate::agent::runner::base_env());
    if let Some(env) = env {
        cmd.envs(env.set.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    }
    // Dropped on every early return (and with the whole future on a
    // timeout), which kills the server's whole process tree.
    let mut server = crate::agent::runner::spawn_contained(&mut cmd)
        .map_err(|e| format!("codex app-server could not start: {}", e.kind()))?;
    let child = server.child_mut();
    let mut stdin = child.stdin.take().ok_or("codex app-server has no stdin")?;
    let stdout = child
        .stdout
        .take()
        .ok_or("codex app-server has no stdout")?;
    let mut lines = BufReader::new(stdout);
    let result = talk(&mut stdin, &mut lines, expected_home).await;
    drop(stdin);
    server.stop().await;
    parse_rate_limits(&result?)
        .ok_or_else(|| "codex app-server sent unreadable rate limits".to_owned())
}

/// The JSON-RPC exchange; returns the `account/rateLimits/read` result.
async fn talk<R: tokio::io::AsyncBufRead + Unpin>(
    stdin: &mut tokio::process::ChildStdin,
    lines: &mut R,
    expected_home: Option<&Path>,
) -> Result<Value, String> {
    let init = json!({
        "method": "initialize",
        "id": 0,
        "params": {"clientInfo": {"name": "hashi", "title": null, "version": env!("CARGO_PKG_VERSION")}},
    });
    send(stdin, &init).await?;
    let result = answer(lines, 0).await?;
    if let Some(expected) = expected_home {
        let home = result
            .get("codexHome")
            .and_then(Value::as_str)
            .ok_or("codex app-server did not say which CODEX_HOME it uses")?;
        if !same_dir(Path::new(home), expected) {
            return Err(
                "codex app-server is using another CODEX_HOME than the account's".to_owned(),
            );
        }
    }
    send(stdin, &json!({"method": "initialized"})).await?;
    send(
        stdin,
        &json!({
            "method": "account/rateLimits/read",
            "id": 1,
            "params": {"excludeResetCreditDetails": true},
        }),
    )
    .await?;
    answer(lines, 1).await
}

async fn send(stdin: &mut tokio::process::ChildStdin, msg: &Value) -> Result<(), String> {
    let mut line = msg.to_string();
    line.push('\n');
    stdin
        .write_all(line.as_bytes())
        .await
        .map_err(|_| "codex app-server closed its input".to_owned())?;
    stdin
        .flush()
        .await
        .map_err(|_| "codex app-server closed its input".to_owned())
}

/// The next line of `reader` without its newline, or `None` at the end.
/// A line over `max` bytes is an error, and is never buffered whole.
///
/// # Errors
///
/// Returns a message for a read error or an overlong line.
pub async fn next_line_capped<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> Result<Option<String>, String> {
    let mut line: Vec<u8> = Vec::new();
    loop {
        let buf = reader
            .fill_buf()
            .await
            .map_err(|_| "codex app-server output could not be read".to_owned())?;
        if buf.is_empty() {
            return Ok((!line.is_empty()).then(|| String::from_utf8_lossy(&line).into_owned()));
        }
        let (chunk, done) = match buf.iter().position(|b| *b == b'\n') {
            Some(i) => (&buf[..i], i + 1),
            None => (buf, buf.len()),
        };
        if line.len() + chunk.len() > max {
            return Err(format!(
                "codex app-server sent a line over {} KiB",
                max / 1024
            ));
        }
        line.extend_from_slice(chunk);
        let found = done > chunk.len();
        reader.consume(done);
        if found {
            return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
        }
    }
}

/// Reads lines until the answer to request `id`, skipping notifications
/// and anything that is not JSON. Returns its `result`.
async fn answer<R: tokio::io::AsyncBufRead + Unpin>(
    lines: &mut R,
    id: i64,
) -> Result<Value, String> {
    loop {
        let line = next_line_capped(lines, MAX_LINE)
            .await?
            .ok_or("codex app-server exited before answering")?;
        let Ok(msg) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if msg.get("id").and_then(Value::as_i64) != Some(id) {
            continue;
        }
        if let Some(err) = msg.get("error") {
            let text = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            return Err(describe_error(text));
        }
        return msg
            .get("result")
            .cloned()
            .ok_or_else(|| "codex app-server answered without a result".to_owned());
    }
}

/// The note for a sign-in the usage endpoint refused. The poller matches
/// on it to wait longer and to name the command that fixes it.
pub const SIGN_IN_REJECTED: &str = "the Codex sign-in was rejected";

/// A short, plain reason for an app-server error. The raw message can
/// carry a whole HTTP response (status, headers, JSON body), which is
/// never shown: only what went wrong in words.
#[must_use]
pub fn describe_error(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| lower.contains(n));
    if has(&[
        "401",
        "403",
        "unauthorized",
        "forbidden",
        "sign in",
        "log in",
        "login",
    ]) {
        SIGN_IN_REJECTED.to_owned()
    } else if has(&["429", "too many requests", "rate limited"]) {
        "the Codex usage endpoint is busy, trying again shortly".to_owned()
    } else if has(&[
        "500",
        "502",
        "503",
        "504",
        "server error",
        "unavailable",
        "bad gateway",
    ]) {
        "the Codex usage endpoint is down, trying again shortly".to_owned()
    } else if has(&[
        "timed out",
        "timeout",
        "connect",
        "dns",
        "network",
        "resolve",
    ]) {
        "the Codex usage endpoint could not be reached".to_owned()
    } else {
        "Codex could not read its rate limits".to_owned()
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(a) == canon(b)
}

/// The `CODEX_HOME` of the default account: the owner's `CODEX_HOME`, or
/// `~/.codex`.
#[must_use]
pub fn default_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".codex")))
}

/// One snapshot's windows, keyed `<prefix>primary` and `<prefix>secondary`.
fn snapshot_windows(snap: &Value, prefix: &str, name: Option<&str>, out: &mut Vec<Window>) {
    for slot in ["primary", "secondary"] {
        let Some(w) = snap.get(slot).filter(|w| w.is_object()) else {
            continue;
        };
        let Some(used) = w.get("usedPercent").and_then(Value::as_f64) else {
            continue;
        };
        let mins = w.get("windowDurationMins").and_then(Value::as_i64);
        let resets = w
            .get("resetsAt")
            .and_then(Value::as_i64)
            .map(|s| s.saturating_mul(1_000));
        let base = match mins {
            Some(m) if m > 0 => duration_label(m),
            _ => format!("Codex {slot}"),
        };
        let label = match name {
            Some(n) => format!("{base}, {n}"),
            None => base,
        };
        out.push(Window::new(
            format!("{prefix}{slot}"),
            label,
            clamp_percent(used),
            resets,
            mins,
            Source::Poll,
        ));
    }
}

/// Parses an `account/rateLimits/read` result.
#[must_use]
pub fn parse_rate_limits(result: &Value) -> Option<Quota> {
    let main = result.get("rateLimits").filter(|r| r.is_object())?;
    let mut windows = Vec::new();
    snapshot_windows(main, "", None, &mut windows);
    let main_id = main.get("limitId").and_then(Value::as_str);
    if let Some(by_id) = result.get("rateLimitsByLimitId").and_then(Value::as_object) {
        let mut ids: Vec<&String> = by_id.keys().collect();
        ids.sort();
        for id in ids {
            if Some(id.as_str()) == main_id || id == "codex" {
                continue;
            }
            let safe: String = id
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
                .take(32)
                .collect();
            if safe.is_empty() {
                continue;
            }
            let snap = &by_id[id];
            let name = snap
                .get("limitName")
                .and_then(Value::as_str)
                .map(|n| crate::util::truncate(n.trim(), 40))
                .filter(|n| !n.is_empty() && !n.chars().any(char::is_control))
                .unwrap_or_else(|| safe.clone());
            snapshot_windows(snap, &format!("{safe}."), Some(&name), &mut windows);
        }
    }
    let plan = main
        .get("planType")
        .and_then(Value::as_str)
        .filter(|p| crate::agent::is_safe_arg(p))
        .map(str::to_owned);
    let reached_type = main
        .get("rateLimitReachedType")
        .is_some_and(|t| !t.is_null());
    let blocked = result.get("ordinaryUsageAllowed").and_then(Value::as_bool) == Some(false);
    let reached = (reached_type || blocked).then(|| {
        windows
            .iter()
            .filter(|w| w.used_percent >= 100.0)
            .filter_map(|w| w.resets_at)
            .max()
            .or_else(|| windows.iter().filter_map(|w| w.resets_at).max())
    });
    Some(Quota {
        windows,
        plan,
        reached,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lines_are_read_up_to_the_cap_and_no_further() {
        let mut ok: &[u8] = b"{\"id\":0}\nlast";
        assert_eq!(
            next_line_capped(&mut ok, 64).await.unwrap().as_deref(),
            Some("{\"id\":0}")
        );
        assert_eq!(
            next_line_capped(&mut ok, 64).await.unwrap().as_deref(),
            Some("last")
        );
        assert_eq!(next_line_capped(&mut ok, 64).await.unwrap(), None);
        // A line one byte over the cap, with or without its newline.
        let long = vec![b'x'; MAX_LINE + 1];
        let mut r = BufReader::new(long.as_slice());
        let err = next_line_capped(&mut r, MAX_LINE).await.unwrap_err();
        assert!(err.contains("1024 KiB"), "{err}");
        let mut exact = vec![b'x'; MAX_LINE];
        exact.push(b'\n');
        let mut r = BufReader::with_capacity(4096, exact.as_slice());
        let line = next_line_capped(&mut r, MAX_LINE).await.unwrap().unwrap();
        assert_eq!(line.len(), MAX_LINE);
    }

    #[cfg(unix)]
    fn script(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("codex");
        std::fs::write(&bin, body).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_hung_server_and_its_children_are_killed_and_the_cwd_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let out = dir.path().join("out");
        // Records its working directory, starts a grandchild in the
        // background and then never answers.
        let bin = script(
            dir.path(),
            &format!(
                "#!/bin/sh\npwd > '{o}.cwd'\nls -A > '{o}.ls'\nsleep 300 &\necho $! > '{o}.pid'\nsleep 300\n",
                o = out.display()
            ),
        );
        let err = read_within(&bin, None, None, &home, Duration::from_secs(2))
            .await
            .unwrap_err();
        assert!(err.contains("in time"), "{err}");
        let cwd = std::fs::read_to_string(out.with_extension("cwd")).unwrap();
        let parent = std::fs::canonicalize(home.join(WORK_DIR)).unwrap();
        assert!(
            Path::new(cwd.trim()).starts_with(&parent),
            "{cwd} is not under {}",
            parent.display()
        );
        assert_eq!(
            std::fs::read_to_string(out.with_extension("ls")).unwrap(),
            ""
        );
        let mode = std::fs::metadata(&parent).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        assert!(!Path::new(cwd.trim()).exists(), "removed after the read");
        let pid: i32 = std::fs::read_to_string(out.with_extension("pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let mut gone = false;
        for _ in 0..50 {
            if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err() {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            gone,
            "the server's background child {pid} outlived the read"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_endless_line_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let bin = script(dir.path(), "#!/bin/sh\nexec yes x | tr -d '\\n'\n");
        let err = read_within(&bin, None, None, dir.path(), Duration::from_secs(10))
            .await
            .unwrap_err();
        assert!(err.contains("over 1024 KiB"), "{err}");
    }

    #[test]
    fn rate_limits_parse_into_windows() {
        let v = json!({
            "ordinaryUsageAllowed": true,
            "rateLimits": {
                "limitId": "codex",
                "primary": {"usedPercent": 37, "windowDurationMins": 300, "resetsAt": 1_800_000_000},
                "secondary": {"usedPercent": 64, "windowDurationMins": 10080, "resetsAt": 1_800_500_000},
                "planType": "plus",
                "credits": {"hasCredits": false, "unlimited": false, "balance": null},
                "rateLimitReachedType": null
            },
            "rateLimitsByLimitId": {
                "codex": {"primary": {"usedPercent": 37, "windowDurationMins": 300, "resetsAt": 1}},
                "codex_other": {"limitName": "GPT-5.5", "primary": {"usedPercent": 5, "windowDurationMins": null, "resetsAt": null}}
            }
        });
        let q = parse_rate_limits(&v).unwrap();
        assert_eq!(q.plan.as_deref(), Some("plus"));
        assert_eq!(q.reached, None);
        assert_eq!(q.windows.len(), 3, "{:?}", q.windows);
        assert_eq!(q.windows[0].label, "5 hours");
        assert_eq!(q.windows[0].resets_at, Some(1_800_000_000_000));
        assert_eq!(q.windows[1].label, "week");
        assert!(q.windows[1].is_weekly());
        assert_eq!(q.windows[2].key, "codex_other.primary");
        assert_eq!(q.windows[2].label, "Codex primary, GPT-5.5");
    }

    #[test]
    fn app_server_errors_become_short_plain_notes() {
        let raw = "failed to fetch codex rate limits: GET https://chatgpt.com/backend-api/wham/usage \
                   failed: 401 Unauthorized; content-type=text/plain; body={\n  \"error\": {\n    \"message\": \"x\"";
        assert_eq!(describe_error(raw), SIGN_IN_REJECTED);
        assert_eq!(
            describe_error("GET /usage failed: 503 Service Unavailable"),
            "the Codex usage endpoint is down, trying again shortly"
        );
        assert_eq!(
            describe_error("GET /usage failed: 429 Too Many Requests"),
            "the Codex usage endpoint is busy, trying again shortly"
        );
        assert_eq!(describe_error(""), "Codex could not read its rate limits");
        for text in [raw, "503", "429", "weird"] {
            let note = describe_error(text);
            assert!(!note.contains("http") && !note.contains('{'), "{note}");
        }
    }

    #[test]
    fn a_reached_limit_names_the_latest_full_window() {
        let v = json!({
            "ordinaryUsageAllowed": false,
            "rateLimits": {
                "primary": {"usedPercent": 100, "windowDurationMins": 300, "resetsAt": 1_800_000_000},
                "secondary": {"usedPercent": 70, "windowDurationMins": 10080, "resetsAt": 1_800_500_000},
                "rateLimitReachedType": "rate_limit_reached"
            },
            "rateLimitsByLimitId": null
        });
        let q = parse_rate_limits(&v).unwrap();
        assert_eq!(q.reached, Some(Some(1_800_000_000_000)));
        assert!(parse_rate_limits(&json!({})).is_none());
    }
}
