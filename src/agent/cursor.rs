//! Cursor CLI backend: `cursor-agent -p --output-format json`.
//!
//! Cursor CLI is closed source, so everything here comes from the official
//! docs rather than the code:
//!
//! - <https://cursor.com/docs/cli/headless>
//! - <https://cursor.com/docs/cli/reference/parameters>
//! - <https://cursor.com/docs/cli/reference/output-format>
//!
//! Safety: `-f`/`--force` (alias `--yolo`) is never passed, and is stripped
//! from operator supplied extra arguments too. Without it, print mode only
//! proposes changes and does not apply them, which is the right default when
//! the bridge is shared with other people. `--approve-mcps` (approve every
//! MCP server without asking) is stripped the same way, so the owner's MCP
//! servers are not handed to shared users by an extra argument.

use std::ffi::OsString;
use std::path::Path;

use serde::Deserialize;

use super::{AgentConfig, AgentError, AgentOutput, Invocation};

/// Flags that let Cursor apply changes, run commands or start MCP servers
/// without confirmation.
const UNSAFE_FLAGS: [&str; 4] = ["-f", "--force", "--yolo", "--approve-mcps"];

/// Sandbox mode passed to `--sandbox`. A constant rather than a config field
/// because `AgentConfig::sandbox` holds Codex policy names.
const SANDBOX: &str = "enabled";

/// Extra environment for the child.
///
/// None: the docs list only `CURSOR_API_KEY` (inherited from Hashi's own
/// environment when the owner sets it) and document no variable that turns
/// off update or telemetry prompts.
#[must_use]
pub fn env(_workdir: &Path) -> Vec<(String, String)> {
    Vec::new()
}

/// Builds the argument vector for a print-mode run.
///
/// `--trust` skips the workspace trust prompt, which is documented as
/// headless only. The prompt goes after `--`. Cursor documents a positional
/// `[prompt...]` but not `--` itself (UNVERIFIED: it is honoured by the
/// common Node argument parsers, but has not been checked against a real
/// binary).
#[must_use]
pub fn build_args(cfg: &AgentConfig, inv: Invocation<'_>) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "-p".into(),
        "--output-format".into(),
        "json".into(),
        "--trust".into(),
        "--sandbox".into(),
        SANDBOX.into(),
    ];
    if let Some(model) = inv.model {
        args.extend(["--model".into(), model.into()]);
    }
    if let Some(chat) = inv.resume {
        args.extend(["--resume".into(), chat.into()]);
    }
    args.extend(
        cfg.extra_args
            .iter()
            .filter(|a| !is_unsafe_flag(a))
            .map(OsString::from),
    );
    args.push("--".into());
    args.push(inv.prompt.into());
    args
}

fn is_unsafe_flag(arg: &str) -> bool {
    let name = arg.split_once('=').map_or(arg, |(name, _)| name);
    UNSAFE_FLAGS.contains(&name)
}

/// The documented `json` result object, which matches Claude Code's shape.
#[derive(Debug, Deserialize)]
struct ResultEnvelope {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

/// Parses the single JSON result object Cursor prints in `json` mode.
///
/// The docs say a failed run writes nothing well formed to stdout, only to
/// stderr with a non-zero exit, so missing JSON is a [`AgentError::Parse`]
/// and `execute` then reports stderr. Token usage is not documented, so it is
/// always `None` rather than read from guessed field names.
///
/// # Errors
///
/// Returns [`AgentError::Parse`] when there is no result object and
/// [`AgentError::Reported`] when the result is flagged as an error.
pub fn parse_output(stdout: &str) -> Result<AgentOutput, AgentError> {
    // The result is the last JSON line. Taking the last one also copes with
    // `stream-json`, whose terminal event has the same shape.
    let line = stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with('{'))
        .ok_or_else(|| AgentError::Parse("no JSON result in output".to_owned()))?;
    let env: ResultEnvelope =
        serde_json::from_str(line).map_err(|e| AgentError::Parse(e.to_string()))?;

    if env.kind.as_deref().is_some_and(|k| k != "result") {
        return Err(AgentError::Parse(
            "output did not end with a result".to_owned(),
        ));
    }

    // The docs only describe success objects; an error result is handled the
    // way Claude Code reports one (UNVERIFIED for Cursor).
    if env.is_error || env.subtype.as_deref().is_some_and(|s| s != "success") {
        let reason = env
            .result
            .filter(|r| !r.trim().is_empty())
            .or(env.subtype)
            .unwrap_or_else(|| "Cursor reported an error".to_owned());
        return Err(AgentError::Reported(reason));
    }

    let text = env
        .result
        .ok_or_else(|| AgentError::Parse("result object has no `result`".to_owned()))?;
    Ok(AgentOutput {
        text,
        session_id: env.session_id.filter(|s| !s.trim().is_empty()),
        usage: None,
        rate_limits: Vec::new(),
    })
}

/// Cursor CLI's limit wording and error codes, from its CLI bundle. In print
/// mode a failure is printed to stderr with exit code 1 and no JSON result,
/// so the text arrives as the summary of an `Exit` error.
const LIMIT_MESSAGES: [&str; 11] = [
    "hit your usage limit",
    "usage limit",
    "rate limit",
    "resource_exhausted",
    "quota",
    "free_user_usage_limit",
    "pro_user_usage_limit",
    "free_user_rate_limit_exceeded",
    "pro_user_rate_limit_exceeded",
    "rate_limited",
    "generic_rate_limit_exceeded",
];

/// Whether a failed run's error says this account is at its limit: the
/// messages and codes above, or a bare 429 status. "Your usage limits will
/// reset when your monthly cycle ends on M/D/YYYY" gives the reset date.
/// See [`super::limit_hit`].
#[must_use]
pub fn limit_hit(error: &str) -> Option<Option<time::OffsetDateTime>> {
    if !(super::limit::has_status(error, "429")
        || super::limit::contains_any(error, &LIMIT_MESSAGES))
    {
        return None;
    }
    let now = time::OffsetDateTime::now_utc();
    Some(
        super::limit::after_marker(error, "cycle ends on ")
            .and_then(|rest| super::limit::us_date_utc(&rest))
            .filter(|t| *t > now)
            .or_else(|| super::limit::reset_time(error, now)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentKind;

    const SUCCESS: &str = include_str!("../../tests/fixtures/cursor_success.json");
    const ERROR: &str = include_str!("../../tests/fixtures/cursor_error.json");
    const STREAM: &str = include_str!("../../tests/fixtures/cursor_stream.jsonl");

    fn cfg() -> AgentConfig {
        AgentConfig {
            bin: "cursor-agent".into(),
            ..AgentConfig::default_for(AgentKind::Cursor)
        }
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn inv<'a>(prompt: &'a str, resume: Option<&'a str>, model: Option<&'a str>) -> Invocation<'a> {
        Invocation {
            prompt,
            resume,
            model,
        }
    }

    #[test]
    fn new_run_uses_print_mode_with_json() {
        let args = strings(&build_args(&cfg(), inv("hi", None, None)));
        assert_eq!(
            args,
            [
                "-p",
                "--output-format",
                "json",
                "--trust",
                "--sandbox",
                "enabled",
                "--",
                "hi"
            ]
        );
    }

    #[test]
    fn model_flag_only_when_set() {
        let without = strings(&build_args(&cfg(), inv("hi", None, None)));
        assert!(!without.iter().any(|a| a == "--model"));
        let with = strings(&build_args(&cfg(), inv("hi", None, Some("gpt-5"))));
        assert!(with.windows(2).any(|w| w == ["--model", "gpt-5"]));
    }

    #[test]
    fn resume_passes_chat_id() {
        let id = "c6b62c6f-7ead-4fd6-9922-e952131177ff";
        let args = strings(&build_args(&cfg(), inv("again", Some(id), None)));
        assert!(args.windows(2).any(|w| w == ["--resume", id]));
        let fresh = strings(&build_args(&cfg(), inv("again", None, None)));
        assert!(!fresh.iter().any(|a| a == "--resume" || a == "--continue"));
    }

    #[test]
    fn flag_looking_prompt_stays_after_separator() {
        let args = strings(&build_args(&cfg(), inv("--force", None, None)));
        let n = args.len();
        assert_eq!(args[n - 2], "--");
        assert_eq!(args[n - 1], "--force");
        assert_eq!(args.iter().filter(|a| *a == "--").count(), 1);
    }

    #[test]
    fn force_never_appears_even_from_extra_args() {
        let mut c = cfg();
        c.extra_args = ["--force", "-f", "--yolo", "--force=true", "--approve-mcps"]
            .map(str::to_owned)
            .to_vec();
        let args = strings(&build_args(&c, inv("hi", Some("abc"), Some("m"))));
        let before_prompt = &args[..args.len() - 2];
        for flag in ["--force", "-f", "--yolo", "--force=true"] {
            assert!(
                !before_prompt.iter().any(|a| a == flag),
                "{flag} in {args:?}"
            );
        }
    }

    #[test]
    fn approve_mcps_never_appears_even_from_extra_args() {
        let mut c = cfg();
        c.extra_args = ["--approve-mcps", "--approve-mcps=true", "--model-x"]
            .map(str::to_owned)
            .to_vec();
        let args = strings(&build_args(&c, inv("--approve-mcps", None, None)));
        let before_prompt = &args[..args.len() - 2];
        assert!(
            !before_prompt
                .iter()
                .any(|a| a.starts_with("--approve-mcps")),
            "{args:?}"
        );
        // Unrelated extra arguments still pass through, and the prompt is untouched.
        assert!(before_prompt.iter().any(|a| a == "--model-x"));
        assert_eq!(args.last().map(String::as_str), Some("--approve-mcps"));
    }

    #[test]
    fn parses_success_fixture() {
        let out = parse_output(SUCCESS).unwrap();
        assert_eq!(out.text, "I added a unit test for `add` in tests/add.rs.");
        assert_eq!(
            out.session_id.as_deref(),
            Some("c6b62c6f-7ead-4fd6-9922-e952131177ff")
        );
        assert_eq!(out.usage, None);
    }

    #[test]
    fn parses_stream_json_terminal_result() {
        let out = parse_output(STREAM).unwrap();
        assert_eq!(out.text, "pong");
        assert_eq!(out.session_id.as_deref(), Some("stream-chat-1"));
    }

    #[test]
    fn error_result_is_reported() {
        let err = parse_output(ERROR).unwrap_err();
        assert!(matches!(&err, AgentError::Reported(m) if m == "model quota exhausted"));
    }

    #[test]
    fn empty_stdout_is_a_parse_error() {
        assert!(matches!(parse_output(""), Err(AgentError::Parse(_))));
        assert!(matches!(parse_output("  \n"), Err(AgentError::Parse(_))));
        assert!(matches!(
            parse_output("Error: not logged in"),
            Err(AgentError::Parse(_))
        ));
    }

    #[test]
    fn stream_without_result_is_a_parse_error() {
        let partial = r#"{"type":"system","subtype":"init","session_id":"x"}"#;
        assert!(matches!(parse_output(partial), Err(AgentError::Parse(_))));
    }

    #[test]
    fn limit_messages_are_recognised() {
        assert!(limit_hit("model quota exhausted").is_some());
        assert!(limit_hit("HTTP 429 Too Many Requests").is_some());
        assert!(limit_hit("request 14290 failed").is_none());
        assert!(limit_hit("not signed in").is_none());
        for msg in [
            "agent exited with exit status: 1: You've hit your usage limit",
            "agent exited with exit status: 1: Error: rate limit reached, slow down",
            "ConnectError: [resource_exhausted] Error",
            "ERROR_CODE PRO_USER_USAGE_LIMIT",
            "FREE_USER_RATE_LIMIT_EXCEEDED",
            "GENERIC_RATE_LIMIT_EXCEEDED",
            "RATE_LIMITED",
        ] {
            assert!(limit_hit(msg).is_some(), "{msg}");
        }
    }

    #[test]
    fn monthly_cycle_end_is_the_reset() {
        let year = time::OffsetDateTime::now_utc().year() + 1;
        let msg = format!(
            "You\u{2019}ve hit your usage limit. Your usage limits will reset when your monthly cycle ends on 3/1/{year}."
        );
        let at = limit_hit(&msg).unwrap().unwrap();
        assert_eq!(
            (at.year(), at.month(), at.day()),
            (year, time::Month::March, 1)
        );
        assert_eq!(limit_hit("You've hit your usage limit"), Some(None));
    }
}
