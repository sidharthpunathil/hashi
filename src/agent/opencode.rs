//! opencode backend: `opencode run --standalone --format json`, which streams
//! JSONL events.
//!
//! Targets opencode V2 (`@opencode/cli`), as read from the 2.2.5 source.
//! **Minimum version: 2.2.0**, the release that accepts `--standalone` after
//! the subcommand and reports server connection failures as JSON. The 1.x
//! builds (`opencode-ai`) have a different CLI and are not supported.
//!
//! Behaviour this module relies on, with the source it was checked against
//! (paths under `packages/cli/src` unless noted):
//!
//! - `run/run.ts`: the project directory is `process.env.PWD ?? process.cwd()`,
//!   so [`env`] points `PWD` at the workspace. Positional message parts are
//!   joined with a space, and any part that contains a space is first wrapped
//!   in literal double quotes. [`build_args`] therefore splits the prompt on
//!   single spaces so that the join rebuilds the original text exactly.
//! - effect/cli's lexer splits argv at the first `--` and keeps everything
//!   after it as operands, so no piece of the prompt is read as a flag, even
//!   a piece that is itself `--help` or `--`.
//! - `run/noninteractive.ts`: without `--auto` every permission ask is
//!   answered "reject" and the model is told to carry on, and questions are
//!   cancelled, so an unattended run never waits for a human. Events are
//!   `{"type", "timestamp", "sessionID", ...}` lines on stdout: `step_start`,
//!   `text`, `reasoning`, `tool_use`, `step_finish` (per step `cost` in USD
//!   and `tokens`) and `error`. There is no final result event.
//! - `services/server-connection.ts`: without `--standalone`, `run` attaches
//!   to an already running background service. `--standalone` gives every
//!   run a private server.

use std::ffi::OsString;
use std::path::Path;

use serde_json::Value;

use super::{AgentConfig, AgentError, AgentOutput, Invocation, Usage};

/// Extra environment for the child.
///
/// - `PWD`: opencode takes its project directory from `PWD` before the real
///   working directory, and Hashi's own `PWD` would otherwise leak through.
/// - `OPENCODE_DISABLE_AUTOUPDATE`: no update check or self upgrade during an
///   unattended run (`services/updater.ts`).
/// - `OPENCODE_DISABLE_PROJECT_CONFIG`: ignore `opencode.json` and `.opencode`
///   directories found in the workspace or its parents
///   (`server-process.ts`, `core/src/config/discovery.ts`). The workspace is
///   writable by whoever holds the token, and a project config could
///   otherwise grant itself permissions or plugins. The owner's global config
///   still applies.
#[must_use]
pub fn env(workdir: &Path) -> Vec<(String, String)> {
    let dir = std::path::absolute(workdir).unwrap_or_else(|_| workdir.to_path_buf());
    vec![
        ("PWD".to_owned(), dir.to_string_lossy().into_owned()),
        ("OPENCODE_DISABLE_AUTOUPDATE".to_owned(), "1".to_owned()),
        ("OPENCODE_DISABLE_PROJECT_CONFIG".to_owned(), "1".to_owned()),
    ]
}

/// Builds the argument vector for a headless `opencode run`.
///
/// `--auto` (and its aliases) is never added, so permission asks are
/// rejected rather than granted. The prompt goes after `--`, split on single
/// spaces: opencode wraps any argument containing a space in quotes before
/// joining the arguments with spaces, so one piece per word is the only form
/// that reaches the model unchanged. Newlines, tabs, quotes and leading
/// dashes stay inside their piece and are passed through as is.
#[must_use]
pub fn build_args(cfg: &AgentConfig, inv: Invocation<'_>) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "run".into(),
        "--standalone".into(),
        "--format".into(),
        "json".into(),
    ];
    if let Some(model) = inv.model {
        args.extend(["-m".into(), model.into()]);
    }
    if let Some(session) = inv.resume {
        args.extend(["-s".into(), session.into()]);
    }
    args.extend(cfg.extra_args.iter().map(OsString::from));
    args.push("--".into());
    args.extend(prompt_words(inv.prompt).map(OsString::from));
    args
}

/// Splits `prompt` into the argv pieces that opencode joins back with spaces.
fn prompt_words(prompt: &str) -> impl Iterator<Item = &str> {
    prompt.split(' ')
}

/// Folds the JSONL event stream into a single result.
///
/// The answer is the text of the last step that produced any, the session id
/// comes from the `sessionID` on each line, and usage is summed over every
/// `step_finish`. Unknown event types and non-JSON lines are ignored.
///
/// # Errors
///
/// Returns [`AgentError::Reported`] when the stream carries an `error` event
/// and [`AgentError::Parse`] when it holds no assistant text.
pub fn parse_output(stdout: &str) -> Result<AgentOutput, AgentError> {
    let mut session_id: Option<String> = None;
    let mut current: Vec<String> = Vec::new();
    let mut answer: Vec<String> = Vec::new();
    let mut usage: Option<Usage> = None;
    let mut failure: Option<String> = None;

    for line in stdout.lines().map(str::trim).filter(|l| l.starts_with('{')) {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(id) = event
            .get("sessionID")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            session_id = Some(id.to_owned());
        }
        match event.get("type").and_then(Value::as_str) {
            Some("step_start") => {
                if !current.is_empty() {
                    answer = std::mem::take(&mut current);
                }
            }
            Some("text") => {
                if let Some(text) = event
                    .pointer("/part/text")
                    .and_then(Value::as_str)
                    .filter(|t| !t.trim().is_empty())
                {
                    current.push(text.to_owned());
                }
            }
            Some("step_finish") => {
                let part = &event["part"];
                let n = |ptr: &str| count(part.pointer(ptr));
                let step = Usage {
                    input_tokens: n("/tokens/input")
                        + n("/tokens/cache/read")
                        + n("/tokens/cache/write"),
                    output_tokens: n("/tokens/output") + n("/tokens/reasoning"),
                    cost_usd: part
                        .get("cost")
                        .and_then(Value::as_f64)
                        .filter(|c| c.is_finite()),
                };
                usage = Some(match usage {
                    None => step,
                    Some(total) => Usage {
                        input_tokens: total.input_tokens + step.input_tokens,
                        output_tokens: total.output_tokens + step.output_tokens,
                        cost_usd: match (total.cost_usd, step.cost_usd) {
                            (Some(a), Some(b)) => Some(a + b),
                            (a, b) => a.or(b),
                        },
                    },
                });
            }
            Some("error") if failure.is_none() => {
                let message = event
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|m| !m.is_empty())
                    .unwrap_or("opencode reported an error");
                failure = Some(message.to_owned());
            }
            _ => {}
        }
    }
    if !current.is_empty() {
        answer = current;
    }

    // opencode exits non-zero after any error event, even when some text was
    // produced first, so the run counts as failed.
    if let Some(reason) = failure {
        return Err(AgentError::Reported(reason));
    }
    if answer.is_empty() {
        return Err(AgentError::Parse(
            "opencode produced no assistant text".to_owned(),
        ));
    }
    Ok(AgentOutput {
        text: answer.join("\n\n"),
        session_id,
        usage,
    })
}

/// Reads a token count, which the schema types as a finite number.
fn count(value: Option<&Value>) -> u64 {
    match value {
        Some(v) => v.as_u64().unwrap_or_else(|| {
            let f = v.as_f64().unwrap_or(0.0);
            if f.is_finite() && f > 0.0 {
                // Saturating float to integer conversion.
                f.round() as u64
            } else {
                0
            }
        }),
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentKind;

    const SUCCESS: &str = include_str!("../../tests/fixtures/opencode_success.jsonl");
    const FAILURE: &str = include_str!("../../tests/fixtures/opencode_failure.jsonl");
    const STARTUP_ERROR: &str = include_str!("../../tests/fixtures/opencode_startup_error.jsonl");

    fn cfg() -> AgentConfig {
        AgentConfig {
            bin: "opencode".into(),
            ..AgentConfig::default_for(AgentKind::Opencode)
        }
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn inv(prompt: &str) -> Invocation<'_> {
        Invocation {
            prompt,
            resume: None,
            model: None,
        }
    }

    /// Mirrors opencode's `formatMessage` in `run/run.ts`.
    fn opencode_join(parts: &[String]) -> String {
        parts
            .iter()
            .map(|p| {
                if p.contains(' ') {
                    format!("\"{}\"", p.replace('"', "\\\""))
                } else {
                    p.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn after_separator(args: &[String]) -> Vec<String> {
        let at = args.iter().position(|a| a == "--").unwrap();
        args[at + 1..].to_vec()
    }

    #[test]
    fn build_args_for_new_run() {
        let args = strings(&build_args(&cfg(), inv("fix the failing test")));
        assert_eq!(
            args,
            [
                "run",
                "--standalone",
                "--format",
                "json",
                "--",
                "fix",
                "the",
                "failing",
                "test"
            ]
        );
    }

    #[test]
    fn build_args_for_resume() {
        let args = strings(&build_args(
            &cfg(),
            Invocation {
                prompt: "now add a comment",
                resume: Some("ses_hashi7f3a9c21"),
                model: None,
            },
        ));
        assert_eq!(
            args,
            [
                "run",
                "--standalone",
                "--format",
                "json",
                "-s",
                "ses_hashi7f3a9c21",
                "--",
                "now",
                "add",
                "a",
                "comment"
            ]
        );
    }

    #[test]
    fn build_args_passes_model_only_when_set() {
        let with = strings(&build_args(
            &cfg(),
            Invocation {
                prompt: "hi",
                resume: None,
                model: Some("anthropic/claude-sonnet-4-5"),
            },
        ));
        assert_eq!(&with[4..6], ["-m", "anthropic/claude-sonnet-4-5"]);
        let without = strings(&build_args(&cfg(), inv("hi")));
        assert!(!without.contains(&"-m".to_owned()));
        assert!(!without.contains(&"-s".to_owned()));
    }

    #[test]
    fn build_args_never_auto_approves() {
        let args = strings(&build_args(&cfg(), inv("hi")));
        for flag in ["--auto", "--yolo", "--dangerously-skip-permissions"] {
            assert!(!args.contains(&flag.to_owned()));
        }
    }

    #[test]
    fn prompt_survives_opencode_join_exactly() {
        let prompts = [
            "-- --help please",
            "--auto",
            "line one\nline two\n\n  indented \"quoted\" text",
            "  leading and trailing  ",
            "tabs\tstay\tinside",
            "-s ses_evil -m x/y",
            "single",
        ];
        for prompt in prompts {
            let args = strings(&build_args(&cfg(), inv(prompt)));
            let words = after_separator(&args);
            assert!(words.iter().all(|w| !w.contains(' ')), "{words:?}");
            assert_eq!(opencode_join(&words), prompt);
        }
    }

    #[test]
    fn prompt_with_leading_separator_stays_after_the_real_one() {
        let args = strings(&build_args(&cfg(), inv("-- rm -rf")));
        assert_eq!(&args[4..], ["--", "--", "rm", "-rf"]);
    }

    #[test]
    fn env_sets_pwd_to_the_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let dir = workspace.path();
        let env = env(dir);
        let get = |k: &str| {
            env.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
        };
        // Compare with the platform's own absolute form (a drive letter on Windows).
        let expected = std::path::absolute(dir).unwrap();
        assert_eq!(get("PWD"), Some(expected.to_string_lossy().as_ref()));
        assert_eq!(get("OPENCODE_DISABLE_AUTOUPDATE"), Some("1"));
        assert_eq!(get("OPENCODE_DISABLE_PROJECT_CONFIG"), Some("1"));
    }

    #[test]
    fn env_makes_a_relative_workspace_absolute() {
        let env = env(Path::new("ws"));
        let pwd = &env.iter().find(|(k, _)| k == "PWD").unwrap().1;
        assert!(Path::new(pwd).is_absolute(), "{pwd}");
        assert!(pwd.ends_with("ws"));
    }

    #[test]
    fn parse_output_returns_last_step_text_session_and_summed_usage() {
        let out = parse_output(SUCCESS).unwrap();
        assert_eq!(out.text, "`add` returns the sum of its two arguments.");
        assert_eq!(out.session_id.as_deref(), Some("ses_hashi7f3a9c21"));
        let usage = out.usage.unwrap();
        assert_eq!(usage.input_tokens, 5123 + 100 + 5210 + 4096);
        assert_eq!(usage.output_tokens, 40 + 8 + 12);
        assert!((usage.cost_usd.unwrap() - 0.00421).abs() < 1e-12);
    }

    #[test]
    fn parse_output_reports_error_event() {
        let err = parse_output(FAILURE).unwrap_err();
        assert!(
            matches!(&err, AgentError::Reported(m) if m == "Invalid API key"),
            "{err}"
        );
    }

    #[test]
    fn parse_output_reports_startup_error_without_session() {
        let err = parse_output(STARTUP_ERROR).unwrap_err();
        assert!(matches!(&err, AgentError::Reported(m) if m == "Invalid model reference"));
    }

    #[test]
    fn parse_output_error_after_text_still_fails() {
        let stream = format!(
            "{}\n{}",
            SUCCESS.trim(),
            r#"{"type":"error","timestamp":1,"sessionID":"ses_x","error":{"type":"aborted","message":"Session interrupted: shutdown"}}"#
        );
        assert!(matches!(
            parse_output(&stream),
            Err(AgentError::Reported(_))
        ));
    }

    #[test]
    fn parse_output_keeps_earlier_text_when_last_step_is_silent() {
        let stream = [
            r#"{"type":"step_start","sessionID":"ses_a","part":{}}"#,
            r#"{"type":"text","sessionID":"ses_a","part":{"type":"text","text":"done"}}"#,
            r#"{"type":"step_finish","sessionID":"ses_a","part":{"cost":0,"tokens":{"input":1.0,"output":2,"reasoning":0,"cache":{"read":0,"write":0}}}}"#,
            r#"{"type":"step_start","sessionID":"ses_a","part":{}}"#,
            r#"{"type":"step_finish","sessionID":"ses_a","part":{"tokens":{"input":3,"output":0}}}"#,
        ]
        .join("\n");
        let out = parse_output(&stream).unwrap();
        assert_eq!(out.text, "done");
        let usage = out.usage.unwrap();
        assert_eq!((usage.input_tokens, usage.output_tokens), (4, 2));
        assert_eq!(usage.cost_usd, Some(0.0));
    }

    #[test]
    fn parse_output_ignores_noise_and_unknown_events() {
        let stream = "warming up\n{not json\n{\"type\":\"brand_new\",\"sessionID\":\"ses_b\"}\n\
            {\"type\":\"text\",\"sessionID\":\"ses_b\",\"part\":{\"text\":\"ok\"}}\n";
        let out = parse_output(stream).unwrap();
        assert_eq!(out.text, "ok");
        assert_eq!(out.session_id.as_deref(), Some("ses_b"));
        assert_eq!(out.usage, None);
    }

    #[test]
    fn parse_output_without_text_is_parse_error() {
        for stream in [
            "",
            "garbage",
            r#"{"type":"step_start","sessionID":"ses_c"}"#,
        ] {
            assert!(matches!(parse_output(stream), Err(AgentError::Parse(_))));
        }
    }
}
