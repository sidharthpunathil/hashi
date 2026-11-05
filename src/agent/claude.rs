//! Claude Code backend: `claude -p --output-format stream-json --verbose`.

use std::ffi::OsString;

use serde::Deserialize;
use serde_json::Value;

use super::{
    AgentConfig, AgentError, AgentOutput, Invocation, RateLimit, RateLimitStatus, Usage, has_flag,
    neutralize_slash_command,
};

/// Extra environment for the child. None needed.
#[must_use]
pub fn env(_workdir: &std::path::Path) -> Vec<(String, String)> {
    Vec::new()
}

/// Builds the argument vector for a Claude Code print-mode run.
///
/// - `--setting-sources user`: only the owner's user settings load, not
///   project or local settings from the per-token workspace. A token that can
///   write files (under an owner-chosen permission mode) could otherwise plant
///   `.claude/settings.json` with hooks or allow rules for its next run.
/// - Without an owner permission mode, the network and MCP posture is pinned:
///   `--disallowedTools WebFetch`, so shared users cannot make the owner's
///   machine fetch URLs (Bash stays under Claude Code's own default rules),
///   and `--strict-mcp-config`, so none of the owner's MCP servers load. The
///   owner opts in through `extra_args`: naming `WebFetch` in any extra
///   argument (for example `--allowedTools WebFetch`) drops the WebFetch deny,
///   and `--mcp-config <file>` loads exactly the servers in that file.
/// - A prompt starting with `/` gets a leading space so it is never run as a
///   slash command or skill from the owner's config.
///
/// `--disallowedTools` takes a variable number of values, so it is always
/// followed by another option and never by text from the request.
#[must_use]
pub fn build_args(cfg: &AgentConfig, inv: Invocation<'_>) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "-p".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--max-turns".into(),
        cfg.max_turns.to_string().into(),
    ];
    if !has_flag(&cfg.extra_args, &["--setting-sources"]) {
        args.extend(["--setting-sources".into(), "user".into()]);
    }
    if cfg.permission_mode.is_none() {
        if !cfg.extra_args.iter().any(|a| a.contains("WebFetch")) {
            args.extend(["--disallowedTools".into(), "WebFetch".into()]);
        }
        if !has_flag(&cfg.extra_args, &["--strict-mcp-config"]) {
            args.push("--strict-mcp-config".into());
        }
    }
    if let Some(model) = inv.model {
        args.extend(["--model".into(), model.into()]);
    }
    if let Some(session) = inv.resume {
        args.extend(["--resume".into(), session.into()]);
    }
    if let Some(mode) = &cfg.permission_mode {
        args.extend(["--permission-mode".into(), mode.into()]);
    }
    args.extend(cfg.extra_args.iter().map(OsString::from));
    args.push("--".into());
    args.push(neutralize_slash_command(inv.prompt).into());
    args
}

#[derive(Debug, Deserialize)]
struct ResultEnvelope {
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default)]
    total_cost_usd: Option<f64>,
    #[serde(default)]
    usage: Option<RawUsage>,
}

#[derive(Debug, Deserialize)]
struct RawUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

/// `rate_limit_event.rate_limit_info`, as the Agent SDK types declare it
/// (`SDKRateLimitInfo`). Only the fields Hashi reads.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRateLimit {
    status: Option<String>,
    /// Unix seconds.
    resets_at: Option<f64>,
    rate_limit_type: Option<String>,
    utilization: Option<f64>,
}

/// A `rate_limit_event`'s figures. Claude Code builds the event from the
/// `anthropic-ratelimit-unified-*-utilization` response headers, which are
/// fractions from 0 to 1, so `utilization` is scaled to a percent. (The
/// separate usage endpoint reports 0 to 100; see `usage::claude`.)
fn rate_limit(info: &Value) -> Option<RateLimit> {
    let raw: RawRateLimit = serde_json::from_value(info.clone()).ok()?;
    let status = match raw.status.as_deref()? {
        "allowed" => RateLimitStatus::Allowed,
        "allowed_warning" => RateLimitStatus::Warning,
        "rejected" => RateLimitStatus::Rejected,
        _ => return None,
    };
    let used_percent = raw
        .utilization
        .filter(|u| u.is_finite() && *u >= 0.0)
        .map(|u| (u * 100.0).min(100.0));
    #[allow(clippy::cast_possible_truncation)]
    let resets_at = raw
        .resets_at
        .filter(|s| s.is_finite() && *s > 0.0)
        .map(|s| (s * 1_000.0) as i64);
    Some(RateLimit {
        kind: raw.rate_limit_type.filter(|k| super::is_safe_arg(k)),
        status,
        used_percent,
        resets_at,
    })
}

/// Parses Claude Code's `stream-json` output: one JSON message per line.
///
/// The answer, session and usage come from the `result` message, exactly
/// as they did from the single object of `json` output (which still
/// parses: a line without a `type` counts as the result). Each
/// `rate_limit_event` becomes one of [`AgentOutput::rate_limits`]. A failed
/// run whose stream has a `rejected` rate limit, or an assistant message
/// with `error: "rate_limit"`, is [`AgentError::Limited`] with the reset
/// time.
///
/// # Errors
///
/// Returns [`AgentError::Parse`] for output without a result,
/// [`AgentError::Limited`] for a run refused at the plan limit and
/// [`AgentError::Reported`] when Claude flags the run as an error.
pub fn parse_output(stdout: &str) -> Result<AgentOutput, AgentError> {
    let mut result: Option<&str> = None;
    let mut untyped: Option<&str> = None;
    let mut rate_limits = Vec::new();
    let mut assistant_limited = false;
    // Diagnostics may come before or between the messages; only JSON
    // objects count.
    for line in stdout.lines().map(str::trim).filter(|l| l.starts_with('{')) {
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            untyped = Some(line);
            continue;
        };
        match msg.get("type").and_then(Value::as_str) {
            Some("result") => result = Some(line),
            Some("rate_limit_event") => {
                if let Some(l) = msg.get("rate_limit_info").and_then(rate_limit) {
                    rate_limits.push(l);
                }
            }
            Some("assistant") => {
                if msg.get("error").and_then(Value::as_str) == Some("rate_limit") {
                    assistant_limited = true;
                }
            }
            Some(_) => {}
            None => untyped = Some(line),
        }
    }
    let line = result
        .or(untyped)
        .ok_or_else(|| AgentError::Parse("no JSON result in output".to_owned()))?;
    let env: ResultEnvelope =
        serde_json::from_str(line).map_err(|e| AgentError::Parse(e.to_string()))?;

    let usage = env.usage.map(|u| Usage {
        input_tokens: u.input_tokens + u.cache_creation_input_tokens + u.cache_read_input_tokens,
        output_tokens: u.output_tokens,
        cost_usd: env.total_cost_usd,
    });

    if env.is_error {
        let reason = env
            .result
            .filter(|r| !r.trim().is_empty())
            .or(env.subtype)
            .unwrap_or_else(|| "Claude reported an error".to_owned());
        let rejected = rate_limits
            .iter()
            .filter(|l| l.status == RateLimitStatus::Rejected);
        let limited_reset = rejected.clone().filter_map(|l| l.resets_at).max();
        if assistant_limited || rejected.count() > 0 {
            let resets_at =
                limited_reset.or_else(|| rate_limits.iter().filter_map(|l| l.resets_at).max());
            return Err(AgentError::Limited {
                message: reason,
                resets_at,
                rate_limits,
            });
        }
        return Err(AgentError::Reported(reason));
    }

    Ok(AgentOutput {
        text: env.result.unwrap_or_default(),
        session_id: env.session_id,
        usage,
        rate_limits,
    })
}

/// Claude Code's limit messages: the plan's session, weekly and Opus
/// limits, an API key out of credit, and a rejected 429.
const LIMIT_MESSAGES: [&str; 5] = [
    "hit your session limit",
    "weekly limit",
    "opus limit",
    "credit balance is too low",
    "request rejected (429)",
];

/// Whether a failed run's error says this account is at its limit, and when
/// it resets if the message says. See [`super::limit_hit`].
#[must_use]
pub fn limit_hit(error: &str) -> Option<Option<time::OffsetDateTime>> {
    super::limit::contains_any(error, &LIMIT_MESSAGES)
        .then(|| super::limit::hit(error))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentKind;

    const SUCCESS: &str = include_str!("../../tests/fixtures/claude_success.json");

    fn cfg() -> AgentConfig {
        AgentConfig {
            bin: "claude".into(),
            ..AgentConfig::default_for(AgentKind::Claude)
        }
    }

    #[test]
    fn build_args_places_prompt_after_separator() {
        let inv = Invocation {
            prompt: "--help",
            resume: None,
            model: None,
        };
        let args = build_args(&cfg(), inv);
        let n = args.len();
        assert_eq!(args[n - 2], "--");
        assert_eq!(args[n - 1], "--help");
    }

    #[test]
    fn build_args_includes_resume_and_model() {
        let inv = Invocation {
            prompt: "hi",
            resume: Some("abc"),
            model: Some("opus"),
        };
        let args = build_args(&cfg(), inv);
        let joined: Vec<_> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(joined.windows(2).any(|w| w == ["--resume", "abc"]));
        assert!(joined.windows(2).any(|w| w == ["--model", "opus"]));
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn hi() -> Invocation<'static> {
        Invocation {
            prompt: "hi",
            resume: None,
            model: None,
        }
    }

    #[test]
    fn default_run_pins_settings_network_and_mcp() {
        let args = strings(&build_args(&cfg(), hi()));
        assert_eq!(
            args,
            [
                "-p",
                "--output-format",
                "stream-json",
                "--verbose",
                "--max-turns",
                "10",
                "--setting-sources",
                "user",
                "--disallowedTools",
                "WebFetch",
                "--strict-mcp-config",
                "--",
                "hi"
            ]
        );
    }

    #[test]
    fn variadic_tool_list_is_followed_by_an_option() {
        let inv = Invocation {
            prompt: "WebFetch http://169.254.169.254",
            resume: Some("abc"),
            model: Some("opus"),
        };
        let args = strings(&build_args(&cfg(), inv));
        let at = args.iter().position(|a| a == "--disallowedTools").unwrap();
        assert_eq!(args[at + 1], "WebFetch");
        assert!(args[at + 2].starts_with('-'), "{args:?}");
    }

    #[test]
    fn owner_permission_mode_leaves_tools_and_mcp_alone() {
        let mut c = cfg();
        c.permission_mode = Some("acceptEdits".to_owned());
        let args = strings(&build_args(&c, hi()));
        assert!(
            args.windows(2)
                .any(|w| w == ["--permission-mode", "acceptEdits"])
        );
        assert!(!args.iter().any(|a| a == "--disallowedTools"));
        assert!(!args.iter().any(|a| a == "--strict-mcp-config"));
        assert!(args.windows(2).any(|w| w == ["--setting-sources", "user"]));
    }

    #[test]
    fn owner_extra_args_override_the_pinned_defaults() {
        let mut c = cfg();
        c.extra_args = [
            "--allowedTools",
            "WebFetch",
            "--mcp-config",
            "/srv/hashi/mcp.json",
            "--setting-sources=user,project",
        ]
        .map(str::to_owned)
        .to_vec();
        let args = strings(&build_args(&c, hi()));
        assert!(!args.iter().any(|a| a == "--disallowedTools"), "{args:?}");
        assert!(!args.iter().any(|a| a == "--setting-sources"), "{args:?}");
        // Strict mode stays, so only the servers in the owner's file load.
        assert_eq!(
            args.iter().filter(|a| *a == "--strict-mcp-config").count(),
            1
        );
        let n = args.len();
        assert_eq!(&args[n - 7..n - 2], c.extra_args.as_slice());

        let mut c = cfg();
        c.extra_args = vec!["--strict-mcp-config".to_owned()];
        let args = strings(&build_args(&c, hi()));
        assert_eq!(
            args.iter().filter(|a| *a == "--strict-mcp-config").count(),
            1
        );
    }

    #[test]
    fn leading_slash_is_not_a_slash_command() {
        let inv = Invocation {
            prompt: "/owner-deploy now",
            resume: None,
            model: None,
        };
        let args = strings(&build_args(&cfg(), inv));
        assert_eq!(args.last().map(String::as_str), Some(" /owner-deploy now"));
        let args = strings(&build_args(&cfg(), hi()));
        assert_eq!(args.last().map(String::as_str), Some("hi"));
    }

    #[test]
    fn parse_output_extracts_result_session_and_usage() {
        let out = parse_output(SUCCESS).unwrap();
        assert_eq!(out.text, "pong");
        assert_eq!(
            out.session_id.as_deref(),
            Some("7a9ad98b-c4ef-44fe-ad92-7fb361223d15")
        );
        let usage = out.usage.unwrap();
        assert_eq!(usage.output_tokens, 4);
        assert_eq!(usage.input_tokens, 2 + 21_083 + 11_873);
    }

    const STREAM: &str = include_str!("../../tests/fixtures/claude_stream_success.jsonl");
    const REJECTED: &str = include_str!("../../tests/fixtures/claude_stream_rejected.jsonl");

    #[test]
    fn a_stream_gives_the_same_answer_and_usage_as_json_output() {
        let json = parse_output(SUCCESS).unwrap();
        let stream = parse_output(STREAM).unwrap();
        assert_eq!(stream.text, json.text);
        assert_eq!(stream.session_id, json.session_id);
        assert_eq!(stream.usage, json.usage);
        assert!(json.rate_limits.is_empty());
        // A message after the result (session state) does not hide it.
        assert_eq!(stream.text, "pong");
    }

    #[test]
    fn rate_limit_events_are_read() {
        let out = parse_output(STREAM).unwrap();
        assert_eq!(out.rate_limits.len(), 2);
        let five = &out.rate_limits[0];
        assert_eq!(five.kind.as_deref(), Some("five_hour"));
        assert_eq!(five.status, RateLimitStatus::Warning);
        assert!((five.used_percent.unwrap() - 82.0).abs() < 1e-9);
        assert_eq!(five.resets_at, Some(1_800_018_000_000));
        assert_eq!(out.rate_limits[1].kind.as_deref(), Some("seven_day"));
        assert_eq!(out.rate_limits[1].status, RateLimitStatus::Allowed);
    }

    #[test]
    fn a_rejected_stream_is_a_limit_with_its_reset() {
        match parse_output(REJECTED).unwrap_err() {
            AgentError::Limited {
                message,
                resets_at,
                rate_limits,
            } => {
                assert!(message.contains("hit your session limit"), "{message}");
                assert_eq!(resets_at, Some(1_800_018_000_000));
                assert_eq!(rate_limits[0].status, RateLimitStatus::Rejected);
                assert_eq!(rate_limits[0].used_percent, Some(100.0));
            }
            other => panic!("{other:?}"),
        }
        // The text still reads as a limit message for the fallback path.
        assert!(limit_hit(&parse_output(REJECTED).unwrap_err().to_string()).is_some());
    }

    #[test]
    fn an_assistant_rate_limit_error_alone_is_a_limit() {
        let stream = concat!(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"x"}]},"error":"rate_limit","session_id":"s"}"#,
            "\n",
            r#"{"type":"result","subtype":"success","is_error":true,"result":"API Error: Request rejected (429)","session_id":"s"}"#,
        );
        assert!(matches!(
            parse_output(stream),
            Err(AgentError::Limited {
                resets_at: None,
                ..
            })
        ));
        // Other assistant errors stay ordinary failures.
        let other = stream.replace("rate_limit", "authentication_failed");
        assert!(matches!(parse_output(&other), Err(AgentError::Reported(_))));
    }

    #[test]
    fn utilization_fraction_becomes_a_percent_and_bogus_events_are_skipped() {
        let stream = concat!(
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","rateLimitType":"seven_day_opus","utilization":0.645}}"#,
            "\n",
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"bogus"}}"#,
            "\n",
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","rateLimitType":"five_hour","utilization":3.0}}"#,
            "\n",
            r#"{"type":"result","is_error":false,"result":"ok","session_id":"s"}"#,
        );
        let out = parse_output(stream).unwrap();
        assert_eq!(out.rate_limits.len(), 2);
        let first = out.rate_limits[0].used_percent.unwrap();
        assert!((first - 64.5).abs() < 1e-9, "{first}");
        assert_eq!(out.rate_limits[0].resets_at, None);
        // Out-of-range values are clamped rather than trusted.
        assert_eq!(out.rate_limits[1].used_percent, Some(100.0));
    }

    #[test]
    fn a_stream_without_a_result_is_a_parse_error() {
        let stream = r#"{"type":"system","subtype":"init","session_id":"s"}"#;
        assert!(matches!(parse_output(stream), Err(AgentError::Parse(_))));
    }

    #[test]
    fn parse_output_surfaces_reported_errors() {
        let err = parse_output(r#"{"is_error":true,"subtype":"error_max_turns","result":""}"#)
            .unwrap_err();
        assert_eq!(err.to_string(), "error_max_turns");
    }

    #[test]
    fn parse_output_rejects_non_json() {
        assert!(matches!(
            parse_output("not json"),
            Err(AgentError::Parse(_))
        ));
    }

    #[test]
    fn limit_messages_are_recognised() {
        for msg in [
            "Claude AI usage limit reached: You've hit your session limit \u{b7} resets 3pm",
            "You've reached your weekly limit",
            "Opus limit reached, try again later",
            "Credit balance is too low",
            "API Error: Request rejected (429) \u{b7} rate limited",
        ] {
            assert!(limit_hit(msg).is_some(), "{msg}");
        }
        assert!(limit_hit("simulated failure").is_none());
        assert!(limit_hit("Invalid API key").is_none());
    }

    #[test]
    fn limit_reset_is_read_when_given() {
        let hit = limit_hit("You've hit your session limit, try again in 2 hours").unwrap();
        let left = hit.unwrap() - time::OffsetDateTime::now_utc();
        assert!((7_100..=7_200).contains(&left.whole_seconds()), "{left}");
        assert_eq!(limit_hit("weekly limit"), Some(None));
    }
}
