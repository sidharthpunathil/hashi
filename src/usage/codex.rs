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
/// uses; the server's `codexHome` must be the same directory.
///
/// # Errors
///
/// Returns a message for a server that cannot start, answers out of turn,
/// reports another home, reports an error or times out. The message never
/// contains a credential.
pub async fn read(
    bin: &Path,
    env: Option<&EnvOverride>,
    expected_home: Option<&Path>,
    cwd: &Path,
) -> Result<Quota, String> {
    match tokio::time::timeout(TIMEOUT, read_inner(bin, env, expected_home, cwd)).await {
        Ok(r) => r,
        Err(_) => Err("codex app-server did not answer in time".to_owned()),
    }
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
        .stderr(Stdio::null())
        .kill_on_drop(true);
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
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("codex app-server could not start: {}", e.kind()))?;
    let mut stdin = child.stdin.take().ok_or("codex app-server has no stdin")?;
    let stdout = child
        .stdout
        .take()
        .ok_or("codex app-server has no stdout")?;
    let mut lines = BufReader::new(stdout).lines();

    let init = json!({
        "method": "initialize",
        "id": 0,
        "params": {"clientInfo": {"name": "hashi", "title": null, "version": env!("CARGO_PKG_VERSION")}},
    });
    send(&mut stdin, &init).await?;
    let result = answer(&mut lines, 0).await?;
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
    send(&mut stdin, &json!({"method": "initialized"})).await?;
    send(
        &mut stdin,
        &json!({
            "method": "account/rateLimits/read",
            "id": 1,
            "params": {"excludeResetCreditDetails": true},
        }),
    )
    .await?;
    let result = answer(&mut lines, 1).await?;
    drop(stdin);
    let _ = child.start_kill();
    let _ = child.wait().await;
    parse_rate_limits(&result)
        .ok_or_else(|| "codex app-server sent unreadable rate limits".to_owned())
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

/// Reads lines until the answer to request `id`, skipping notifications
/// and anything that is not JSON. Returns its `result`.
async fn answer(
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    id: i64,
) -> Result<Value, String> {
    loop {
        let line = lines
            .next_line()
            .await
            .map_err(|_| "codex app-server output could not be read".to_owned())?
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
                .unwrap_or("an error");
            return Err(format!(
                "codex app-server answered: {}",
                crate::util::truncate(&crate::diagnostics::redact(text), 200)
            ));
        }
        return msg
            .get("result")
            .cloned()
            .ok_or_else(|| "codex app-server answered without a result".to_owned());
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
