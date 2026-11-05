//! Claude Code backend: `claude -p --output-format json`.

use std::ffi::OsString;

use serde::Deserialize;

use super::{AgentConfig, AgentError, AgentOutput, Invocation, Usage};

/// Extra environment for the child. None needed.
#[must_use]
pub fn env(_workdir: &std::path::Path) -> Vec<(String, String)> {
    Vec::new()
}

/// Builds the argument vector for a Claude Code print-mode run.
#[must_use]
pub fn build_args(cfg: &AgentConfig, inv: Invocation<'_>) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "-p".into(),
        "--output-format".into(),
        "json".into(),
        "--max-turns".into(),
        cfg.max_turns.to_string().into(),
    ];
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
    args.push(inv.prompt.into());
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
}
