//! Codex backend: `codex exec --json`, which streams JSONL events.

use std::ffi::OsString;

use serde_json::Value;

use super::{AgentConfig, AgentError, AgentOutput, Invocation, Usage};

/// Extra environment for the child. None needed.
#[must_use]
pub fn env(_workdir: &std::path::Path) -> Vec<(String, String)> {
    Vec::new()
}

/// Builds the argument vector for a Codex `exec` run.
///
/// The sandbox is passed as a `-c` override because `codex exec resume` does
/// not accept `--sandbox`, while both forms accept config overrides.
///
/// No MCP override is passed. `-c mcp_servers={}` would not hide the owner's
/// servers: Codex deep-merges the `-c` layer into the config tables
/// (`codex-rs/config/src/overrides.rs`, `merge_toml_values`), so an empty
/// table leaves every configured server in place. Turning servers off needs
/// their names (`-c mcp_servers.<name>.enabled=false`), which only the owner
/// knows, so that is left to `extra_args`.
#[must_use]
pub fn build_args(cfg: &AgentConfig, inv: Invocation<'_>) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["exec".into()];
    if inv.resume.is_some() {
        args.push("resume".into());
    }
    args.extend([
        "--json".into(),
        "--skip-git-repo-check".into(),
        "-c".into(),
        format!("sandbox_mode=\"{}\"", cfg.sandbox).into(),
    ]);
    if let Some(model) = inv.model {
        args.extend(["-m".into(), model.into()]);
    }
    args.extend(cfg.extra_args.iter().map(OsString::from));
    if let Some(session) = inv.resume {
        args.push(session.into());
    }
    args.push("--".into());
    args.push(inv.prompt.into());
    args
}

/// Folds the JSONL event stream into a single result.
///
/// # Errors
///
/// Returns [`AgentError::Reported`] when the turn failed and
/// [`AgentError::Parse`] when no assistant message was produced.
pub fn parse_output(stdout: &str) -> Result<AgentOutput, AgentError> {
    let mut session_id = None;
    let mut messages: Vec<String> = Vec::new();
    let mut usage = None;
    let mut failure: Option<String> = None;

    for line in stdout.lines().map(str::trim).filter(|l| l.starts_with('{')) {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match event.get("type").and_then(Value::as_str) {
            Some("thread.started") => {
                session_id = event
                    .get("thread_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            Some("item.completed") => {
                let item = &event["item"];
                if item.get("type").and_then(Value::as_str) == Some("agent_message")
                    && let Some(text) = item.get("text").and_then(Value::as_str)
                {
                    messages.push(text.to_owned());
                }
            }
            Some("turn.completed") => {
                let u = &event["usage"];
                let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                usage = Some(Usage {
                    input_tokens: n("input_tokens"),
                    output_tokens: n("output_tokens"),
                    cost_usd: None,
                });
            }
            Some("turn.failed") => {
                failure = event
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or(failure);
            }
            Some("error") if failure.is_none() => {
                failure = event
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            _ => {}
        }
    }

    if let Some(text) = messages.pop() {
        return Ok(AgentOutput {
            text,
            session_id,
            usage,
            rate_limits: Vec::new(),
        });
    }
    match failure {
        Some(reason) => Err(AgentError::Reported(reason)),
        None => Err(AgentError::Parse(
            "Codex produced no agent message".to_owned(),
        )),
    }
}

/// Codex's limit messages. "You’ve hit your usage limit" is printed with a
/// curly apostrophe, which matching ignores.
const LIMIT_MESSAGES: [&str; 3] = ["hit your usage limit", "out of credits", "spend cap"];

/// Whether a failed run's error says this account is at its limit, and when
/// it resets if the message says. See [`super::limit_hit`].
#[must_use]
pub fn limit_hit(error: &str) -> Option<Option<time::OffsetDateTime>> {
    if !super::limit::contains_any(error, &LIMIT_MESSAGES) {
        return None;
    }
    let now = time::OffsetDateTime::now_utc();
    Some(super::limit::local_offset().and_then(|offset| reset_at(error, now, offset)))
}

/// The reset time in Codex's " Try again at {t}." suffix, which is local
/// time: `%-I:%M %p` for today, `%b %-d{st|nd|rd|th}, %Y %-I:%M %p` for
/// another day. "Try again later." and anything unreadable give `None`, so
/// the cooldown applies.
fn reset_at(
    error: &str,
    now: time::OffsetDateTime,
    offset: time::UtcOffset,
) -> Option<time::OffsetDateTime> {
    let rest = super::limit::after_marker(error, "try again at ")?;
    super::limit::local_clock(&rest, now, offset).filter(|t| *t > now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentKind;

    const SUCCESS: &str = include_str!("../../tests/fixtures/codex_success.jsonl");
    const FAILURE: &str = include_str!("../../tests/fixtures/codex_failure.jsonl");

    fn cfg() -> AgentConfig {
        AgentConfig {
            bin: "codex".into(),
            ..AgentConfig::default_for(AgentKind::Codex)
        }
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn build_args_for_new_thread() {
        let args = strings(&build_args(
            &cfg(),
            Invocation {
                prompt: "hi",
                resume: None,
                model: None,
            },
        ));
        assert_eq!(args[0], "exec");
        assert!(args.contains(&"sandbox_mode=\"read-only\"".to_owned()));
        assert_eq!(&args[args.len() - 2..], ["--", "hi"]);
    }

    #[test]
    fn build_args_for_resume_puts_session_before_prompt() {
        let inv = Invocation {
            prompt: "again",
            resume: Some("t-1"),
            model: Some("gpt-5.4"),
        };
        let args = strings(&build_args(&cfg(), inv));
        assert_eq!(&args[..2], ["exec", "resume"]);
        assert_eq!(&args[args.len() - 3..], ["t-1", "--", "again"]);
    }

    #[test]
    fn parse_output_returns_last_agent_message() {
        let out = parse_output(SUCCESS).unwrap();
        assert_eq!(out.text, "pong");
        assert_eq!(
            out.session_id.as_deref(),
            Some("01a1aafc-c38b-7d83-ad09-745cc785e5de")
        );
        assert_eq!(out.usage.unwrap().output_tokens, 6);
    }

    #[test]
    fn parse_output_reports_turn_failure() {
        let err = parse_output(FAILURE).unwrap_err();
        assert!(
            err.to_string()
                .contains("access token could not be refreshed")
        );
    }

    #[test]
    fn parse_output_without_messages_is_parse_error() {
        assert!(matches!(
            parse_output("{\"type\":\"turn.started\"}"),
            Err(AgentError::Parse(_))
        ));
    }

    #[test]
    fn limit_messages_are_recognised() {
        for msg in [
            "You\u{2019}ve hit your usage limit. Upgrade to Pro or try again later.",
            "You've hit your usage limit.",
            "You are out of credits",
            "This workspace reached its spend cap",
        ] {
            assert!(limit_hit(msg).is_some(), "{msg}");
        }
        assert!(limit_hit("please sign in again").is_none());
    }

    #[test]
    fn limit_reset_is_read_from_the_try_again_at_suffix() {
        let now = time::OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap(); // 08:00 UTC
        let utc = time::UtcOffset::UTC;
        let today = "You\u{2019}ve hit your usage limit. Try again at 3:45 PM.";
        let t = reset_at(today, now, utc).unwrap();
        assert_eq!((t - now).whole_minutes(), 7 * 60 + 45);
        let other = "You\u{2019}ve hit your usage limit. Upgrade to Pro (https://chatgpt.com/explore/pro) or try again at Jan 16th, 2027 9:05 AM.";
        let t = reset_at(other, now, utc).unwrap();
        assert_eq!((t - now).whole_minutes(), 25 * 60 + 5);
        let minus5 = time::UtcOffset::from_hms(-5, 0, 0).unwrap();
        let t = reset_at(today, now, minus5).unwrap();
        assert_eq!((t - now).whole_minutes(), 12 * 60 + 45);
        assert_eq!(
            reset_at(
                "You\u{2019}ve hit your usage limit. Try again later.",
                now,
                utc
            ),
            None
        );
        assert_eq!(
            limit_hit("You\u{2019}ve hit your usage limit. Try again later."),
            Some(None)
        );
    }
}
