//! Gemini CLI backend: `gemini -o stream-json --prompt=<text>`.
//!
//! Verified against google-gemini/gemini-cli 0.69.0-nightly (see
//! `docs/research/agent-clis.md`, section 2).
//!
//! Output format: `stream-json` rather than `json`. In `json` mode a fatal
//! error (auth, quota, bad config) is written to stderr and stdout stays
//! empty, while `stream-json` always ends with a `result` event on stdout that
//! carries an explicit `status`. It is also line oriented, so a stray log line
//! on stdout cannot corrupt the parse, and the session id arrives first in the
//! `init` event.

use std::ffi::OsString;
use std::path::Path;

use serde_json::Value;

use super::{
    AgentConfig, AgentError, AgentOutput, Invocation, Usage, has_flag, is_safe_arg,
    neutralize_slash_command,
};

/// Extra environment for the child.
///
/// - `GEMINI_CLI_NO_RELAUNCH`: by default the CLI re-spawns itself as a child
///   Node process (to raise the heap limit). Hashi's timeout kill only reaches
///   the direct child, so without this the real worker would be orphaned.
/// - `GEMINI_TELEMETRY_ENABLED=false`: no telemetry from unattended runs.
#[must_use]
pub fn env(_workdir: &Path) -> Vec<(String, String)> {
    vec![
        ("GEMINI_CLI_NO_RELAUNCH".to_owned(), "1".to_owned()),
        ("GEMINI_TELEMETRY_ENABLED".to_owned(), "false".to_owned()),
    ]
}

/// Builds the argument vector for a headless run.
///
/// - `--skip-trust`: Hashi's per-token workspaces are never in the user's
///   trusted folder list, and an untrusted folder in headless mode exits 55.
/// - `--approval-mode default`: passed explicitly so a user-level
///   `general.defaultApprovalMode` setting cannot turn on `auto_edit`. In
///   headless mode the policy engine turns every "ask the user" decision into
///   a deny (`policy-engine.ts`), so shell commands, file writes and other
///   tools that need approval are refused instead of hanging. YOLO is never
///   used.
/// - Resume uses `--resume=<id>` as one argv element. The option is declared
///   with `skipValidation` and no `nargs`, so the attached form is the least
///   ambiguous. Gemini keeps the original session id on resume.
/// - The prompt is passed as a single `--prompt=<text>` element. yargs splits
///   an `--opt=value` element on the first `=` and takes the rest verbatim
///   (newlines included), so a prompt such as `--yolo` is a value, never a
///   flag. `--prompt` also forces headless mode regardless of TTY state. A
///   positional after `--` is avoided because gemini-cli's yargs setup does
///   not enable `populate--`, so whether it reaches the `[query..]`
///   positional is unverified.
/// - `cfg.max_turns` is not applied: Gemini has no CLI flag for it, only the
///   `model.maxSessionTurns` setting.
/// - `--allowed-mcp-server-names ""`: no MCP server from the owner's settings
///   is connected, so shared users cannot reach the owner's MCP tools or
///   resources. An empty value becomes `[""]`, which no server name matches
///   (`config.ts` `coerceCommaSeparated`, the "allow no MCP servers if the
///   flag is provided but empty" test, and `isBlockedBySettings` in
///   `mcp-client-manager.ts`). The owner opts in by passing the flag with the
///   server names in `extra_args`, which replaces this one.
#[must_use]
pub fn build_args(cfg: &AgentConfig, inv: Invocation<'_>) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "-o".into(),
        "stream-json".into(),
        "--skip-trust".into(),
        "--approval-mode".into(),
        "default".into(),
    ];
    if !has_flag(&cfg.extra_args, &[MCP_ALLOWLIST_FLAG]) {
        args.extend([MCP_ALLOWLIST_FLAG.into(), "".into()]);
    }
    // Callers validate these with `is_safe_arg`; checking again here keeps a
    // value that starts with `-` from ever being read as a flag.
    if let Some(model) = inv.model.filter(|m| is_safe_arg(m)) {
        args.extend(["-m".into(), model.into()]);
    }
    if let Some(session) = inv.resume.filter(|s| is_safe_arg(s)) {
        args.push(format!("--resume={session}").into());
    }
    args.extend(cfg.extra_args.iter().map(OsString::from));
    let mut prompt = OsString::from("--prompt=");
    // Headless Gemini runs a prompt that starts with `/` as a built-in slash
    // command (for example `/extensions install <url>`).
    prompt.push(neutralize_slash_command(inv.prompt));
    args.push(prompt);
    args
}

/// Restricts which configured MCP servers Gemini CLI connects to.
const MCP_ALLOWLIST_FLAG: &str = "--allowed-mcp-server-names";

/// Folds the `stream-json` event stream into a single result.
///
/// The answer is the assistant text after the last tool call, since earlier
/// text is narration between tool steps. Usage comes from `result.stats`; the
/// CLI reports no cost.
///
/// # Errors
///
/// Returns [`AgentError::Reported`] when the `result` event has
/// `status: "error"` (which can happen with exit code 0, for example when the
/// response is blocked) or when the run finished without any answer, and
/// [`AgentError::Parse`] when there is no `result` event at all.
pub fn parse_output(stdout: &str) -> Result<AgentOutput, AgentError> {
    let mut session_id: Option<String> = None;
    let mut text = String::new();
    let mut after_tool = false;
    let mut last_error: Option<String> = None;
    let mut result: Option<Value> = None;

    for line in stdout.lines().map(str::trim).filter(|l| l.starts_with('{')) {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let str_field = |key: &str| event.get(key).and_then(Value::as_str);
        match str_field("type") {
            Some("init") => {
                session_id = str_field("session_id")
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
            }
            Some("message") if str_field("role") == Some("assistant") => {
                let chunk = str_field("content").unwrap_or_default();
                if after_tool && !chunk.trim().is_empty() {
                    text.clear();
                    after_tool = false;
                }
                text.push_str(chunk);
            }
            Some("tool_use" | "tool_result") => after_tool = true,
            Some("error") => {
                if let Some(message) = str_field("message").filter(|m| !m.trim().is_empty()) {
                    // Prefer fatal errors over warnings for the final message.
                    let fatal = str_field("severity") != Some("warning");
                    if fatal || last_error.is_none() {
                        last_error = Some(message.to_owned());
                    }
                }
            }
            Some("result") => result = Some(event),
            _ => {}
        }
    }

    let Some(result) = result else {
        return Err(AgentError::Parse(
            "Gemini CLI produced no result event".to_owned(),
        ));
    };

    if result.get("status").and_then(Value::as_str) != Some("success") {
        let reason = result
            .pointer("/error/message")
            .and_then(Value::as_str)
            .filter(|m| !m.trim().is_empty())
            .map(str::to_owned)
            .or(last_error)
            .unwrap_or_else(|| "Gemini CLI reported an error".to_owned());
        return Err(AgentError::Reported(reason));
    }

    let text = text.trim().to_owned();
    if text.is_empty() {
        return Err(AgentError::Reported(last_error.unwrap_or_else(|| {
            "Gemini CLI finished without an answer".to_owned()
        })));
    }

    let usage = result.get("stats").filter(|s| s.is_object()).map(|stats| {
        let n = |k: &str| stats.get(k).and_then(Value::as_u64).unwrap_or(0);
        Usage {
            // `input_tokens` already includes cached input.
            input_tokens: n("input_tokens"),
            output_tokens: n("output_tokens"),
            cost_usd: None,
        }
    });

    Ok(AgentOutput {
        text,
        session_id,
        usage,
        rate_limits: Vec::new(),
    })
}

/// Gemini CLI's quota messages.
const LIMIT_MESSAGES: [&str; 6] = [
    "exhausted your daily quota",
    "exhausted your capacity",
    "quota exceeded",
    "quota_exhausted",
    "rate_limit_exceeded",
    "please retry in",
];

/// A "Please retry in Ns" wait no longer than this is a brief rate limit,
/// so the account gets the usual cooldown instead.
const MIN_RETRY_SECS: u64 = 300;

/// Whether a failed run's error says this account is at its limit, and when
/// it resets if the message says. "Please retry in Ns" sets the reset only
/// when N is over five minutes. See [`super::limit_hit`].
#[must_use]
pub fn limit_hit(error: &str) -> Option<Option<time::OffsetDateTime>> {
    if !super::limit::contains_any(error, &LIMIT_MESSAGES) {
        return None;
    }
    if let Some(rest) = super::limit::after_marker(error, "please retry in ") {
        return Some(
            super::limit::leading_seconds(&rest)
                .filter(|n| *n > MIN_RETRY_SECS)
                .map(|n| {
                    time::OffsetDateTime::now_utc()
                        + std::time::Duration::from_secs(n).min(super::limit::MAX_RESET)
                }),
        );
    }
    super::limit::hit(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentKind;

    const SUCCESS: &str = include_str!("../../tests/fixtures/gemini_success.jsonl");
    const FAILURE: &str = include_str!("../../tests/fixtures/gemini_failure.jsonl");
    const BLOCKED: &str = include_str!("../../tests/fixtures/gemini_blocked.jsonl");

    fn cfg() -> AgentConfig {
        AgentConfig {
            bin: "gemini".into(),
            ..AgentConfig::default_for(AgentKind::Gemini)
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
    fn build_args_for_new_run() {
        let args = strings(&build_args(&cfg(), inv("hi", None, None)));
        assert_eq!(
            args,
            [
                "-o",
                "stream-json",
                "--skip-trust",
                "--approval-mode",
                "default",
                "--allowed-mcp-server-names",
                "",
                "--prompt=hi"
            ]
        );
    }

    #[test]
    fn build_args_connects_no_mcp_server_by_default() {
        let args = strings(&build_args(&cfg(), inv("hi", Some("s-1"), Some("m"))));
        assert!(
            args.windows(2)
                .any(|w| w == ["--allowed-mcp-server-names", ""])
        );
    }

    #[test]
    fn owner_mcp_allowlist_replaces_the_default() {
        for extra in [
            vec!["--allowed-mcp-server-names".to_owned(), "docs".to_owned()],
            vec!["--allowed-mcp-server-names=docs".to_owned()],
        ] {
            let mut c = cfg();
            c.extra_args = extra.clone();
            let args = strings(&build_args(&c, inv("hi", None, None)));
            let flags = args
                .iter()
                .filter(|a| a.starts_with("--allowed-mcp-server-names"))
                .count();
            assert_eq!(flags, 1, "{args:?}");
            assert!(!args.iter().any(String::is_empty), "{args:?}");
            assert!(args.ends_with(&[extra, vec!["--prompt=hi".to_owned()]].concat()));
        }
    }

    #[test]
    fn build_args_never_auto_approves() {
        let args = strings(&build_args(&cfg(), inv("hi", Some("s-1"), Some("m"))));
        assert!(args.windows(2).any(|w| w == ["--approval-mode", "default"]));
        for bad in ["-y", "--yolo", "yolo", "auto_edit", "--approval-mode=yolo"] {
            assert!(!args.iter().any(|a| a == bad), "{bad} in {args:?}");
        }
    }

    #[test]
    fn build_args_for_resume() {
        let session = "3f1c2a9e-7b4d-4e2a-9c1f-0a8b7d6e5f41";
        let args = strings(&build_args(&cfg(), inv("again", Some(session), None)));
        assert!(args.contains(&format!("--resume={session}")));
        assert!(!args.iter().any(|a| a == "-m"));
        assert_eq!(args.last().map(String::as_str), Some("--prompt=again"));
    }

    #[test]
    fn build_args_passes_model_only_when_set() {
        let args = strings(&build_args(&cfg(), inv("hi", None, Some("gemini-2.5-pro"))));
        assert!(args.windows(2).any(|w| w == ["-m", "gemini-2.5-pro"]));
        let args = strings(&build_args(&cfg(), inv("hi", None, None)));
        assert!(!args.iter().any(|a| a == "-m"));
        assert!(!args.iter().any(|a| a.starts_with("--resume")));
    }

    #[test]
    fn build_args_drops_unsafe_model_and_resume() {
        let args = strings(&build_args(
            &cfg(),
            inv("hi", Some("--yolo"), Some("--approval-mode=yolo")),
        ));
        assert!(!args.iter().any(|a| a.contains("yolo")), "{args:?}");
    }

    #[test]
    fn prompt_starting_with_dashes_stays_one_value() {
        let args = strings(&build_args(&cfg(), inv("--yolo -y --help", None, None)));
        assert_eq!(
            args.last().map(String::as_str),
            Some("--prompt=--yolo -y --help")
        );
        // The prompt is a single element; nothing else in argv is user text.
        assert!(!args.iter().any(|a| a == "--yolo" || a == "-y"));
        assert_eq!(args.iter().filter(|a| a.starts_with("--prompt")).count(), 1);
    }

    #[test]
    fn prompt_with_equals_and_newlines_is_kept_verbatim() {
        let args = strings(&build_args(&cfg(), inv("a=b\n--c", None, None)));
        assert_eq!(args.last().map(String::as_str), Some("--prompt=a=b\n--c"));
    }

    #[test]
    fn leading_slash_is_not_a_slash_command() {
        let args = strings(&build_args(
            &cfg(),
            inv("/extensions install https://x", None, None),
        ));
        assert_eq!(
            args.last().map(String::as_str),
            Some("--prompt= /extensions install https://x")
        );
    }

    #[test]
    fn env_disables_relaunch() {
        let env = env(Path::new("/tmp"));
        assert!(
            env.iter()
                .any(|(k, v)| k == "GEMINI_CLI_NO_RELAUNCH" && v == "1")
        );
    }

    #[test]
    fn parse_output_returns_final_answer_session_and_usage() {
        let out = parse_output(SUCCESS).unwrap();
        assert_eq!(out.text, "It defines `add`, which sums two integers.");
        assert_eq!(
            out.session_id.as_deref(),
            Some("3f1c2a9e-7b4d-4e2a-9c1f-0a8b7d6e5f41")
        );
        assert_eq!(
            out.usage,
            Some(Usage {
                input_tokens: 8120,
                output_tokens: 30,
                cost_usd: None,
            })
        );
    }

    #[test]
    fn parse_output_reports_error_status() {
        let err = parse_output(FAILURE).unwrap_err();
        assert!(matches!(err, AgentError::Reported(_)), "{err:?}");
        assert_eq!(err.to_string(), "[API Error: 429 quota exceeded]");
    }

    #[test]
    fn parse_output_reports_blocked_response() {
        let err = parse_output(BLOCKED).unwrap_err();
        assert!(matches!(err, AgentError::Reported(_)), "{err:?}");
        assert!(err.to_string().contains("blocked"), "{err}");
    }

    #[test]
    fn parse_output_success_without_text_is_reported() {
        let out = concat!(
            r#"{"type":"init","session_id":"s","model":"m"}"#,
            "\n",
            r#"{"type":"error","severity":"warning","message":"Agent execution blocked: hook"}"#,
            "\n",
            r#"{"type":"result","status":"success","stats":{}}"#,
        );
        let err = parse_output(out).unwrap_err();
        assert!(matches!(err, AgentError::Reported(_)), "{err:?}");
        assert!(err.to_string().contains("Agent execution blocked"));
    }

    #[test]
    fn parse_output_without_result_is_parse_error() {
        for out in [
            "",
            "not json",
            r#"{"type":"init","session_id":"s","model":"m"}"#,
            r#"{"type":"message","role":"assistant","content":"partial","delta":true}"#,
        ] {
            assert!(
                matches!(parse_output(out), Err(AgentError::Parse(_))),
                "{out}"
            );
        }
    }

    #[test]
    fn parse_output_ignores_noise_lines() {
        let noisy = format!("Loaded cached credentials.\n{{oops\n{SUCCESS}");
        assert_eq!(
            parse_output(&noisy).unwrap().text,
            "It defines `add`, which sums two integers."
        );
    }

    #[test]
    fn limit_messages_are_recognised() {
        for msg in [
            "You have exhausted your daily quota on this model.",
            "You have exhausted your capacity on this model. Your quota will reset after 3h12m.",
            "[API Error: 429 quota exceeded]",
        ] {
            assert!(limit_hit(msg).is_some(), "{msg}");
        }
        assert!(limit_hit("Gemini CLI reported an error").is_none());
        assert!(limit_hit("[API Error: RESOURCE_EXHAUSTED QUOTA_EXHAUSTED]").is_some());
        assert!(limit_hit("status RATE_LIMIT_EXCEEDED").is_some());
    }

    #[test]
    fn retry_in_sets_the_reset_only_past_five_minutes() {
        let hit = limit_hit("Quota exceeded for model. Please retry in 1234.5s.").unwrap();
        let left = hit.unwrap() - time::OffsetDateTime::now_utc();
        assert!((1_230..=1_235).contains(&left.whole_seconds()), "{left}");
        assert_eq!(limit_hit("Please retry in 42.1s."), Some(None));
        assert_eq!(limit_hit("Please retry in 300s."), Some(None));
    }

    #[test]
    fn limit_reset_is_read_when_given() {
        let msg =
            "You have exhausted your capacity on this model. Your quota will reset after 3h12m.";
        let left = limit_hit(msg).unwrap().unwrap() - time::OffsetDateTime::now_utc();
        assert!((11_400..=11_520).contains(&left.whole_seconds()), "{left}");
    }
}
