//! Claude Code backend: `claude -p --output-format json`.

use std::ffi::OsString;

use serde::Deserialize;

use super::{
    AgentConfig, AgentError, AgentOutput, Invocation, Usage, has_flag, neutralize_slash_command,
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
        "json".into(),
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

/// Parses the single JSON result object Claude prints in `json` output mode.
///
/// # Errors
///
/// Returns [`AgentError::Parse`] for malformed output and
/// [`AgentError::Reported`] when Claude flags the run as an error.
pub fn parse_output(stdout: &str) -> Result<AgentOutput, AgentError> {
    // Some versions print diagnostics before the result; the result object is
    // always the last JSON line.
    let line = stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with('{'))
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
        return Err(AgentError::Reported(reason));
    }

    Ok(AgentOutput {
        text: env.result.unwrap_or_default(),
        session_id: env.session_id,
        usage,
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
                "json",
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
