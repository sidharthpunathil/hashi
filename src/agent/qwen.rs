//! Qwen Code backend: `qwen --output-format stream-json --prompt=<text>`.
//!
//! Verified against the Qwen Code 0.26.7 source (`github:QwenLM/qwen-code`);
//! see `docs/research/agent-clis.md` for the wider notes.
//!
//! - **Prompt form.** The prompt is one argv element, `--prompt=<text>`.
//!   yargs-parser splits `--key=value` on the first `=` and keeps the rest,
//!   newlines and leading dashes included, as the value. The positional form is
//!   unsafe here: yargs (17.x) moves everything after `--` out of the way
//!   before positionals are filled, so `-- <text>` leaves the `[query..]`
//!   positional empty, and a positional that is filled is re-parsed as
//!   `--query <value>`, which reads a value starting with `-` as a flag.
//!   `--prompt` is marked deprecated, which only changes the help text.
//! - **Prompt neutralising.** A prompt starting with `/` runs a slash command
//!   in headless mode, and several are allowed there (`/config` can rewrite any
//!   setting, including tool allowlists; `/update` installs updates). A prompt
//!   wrapped in matching quotes also loses them in yargs-parser. Both are
//!   defused by prefixing a single space, which Qwen Code does not trim
//!   before the slash check (`isSlashCommand` tests the raw input).
//! - **Permissions.** `--approval-mode default` is passed explicitly so a
//!   user's `tools.approvalMode` setting (which could be `yolo`) cannot widen
//!   it. In non-interactive `default` mode Qwen Code adds shell, monitor, edit
//!   and write_file to the deny list unless they are explicitly allowlisted
//!   (`packages/cli/src/config/config.ts`).
//! - **Output.** `stream-json` prints one JSON object per line and ends with a
//!   `result` object shaped like Claude Code's.

use std::ffi::OsString;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use super::{AgentConfig, AgentError, AgentOutput, Invocation, Usage, has_flag};

/// Approval mode passed on every run. Hashi's `AgentConfig` has no Qwen field,
/// so this is fixed: headless `default` denies shell and edits unless
/// allowlisted, and never auto-approves.
pub const APPROVAL_MODE: &str = "default";

/// Extra environment for the child.
///
/// `QWEN_CODE_NO_RELAUNCH` stops Qwen Code from respawning itself as a child
/// node process (for a larger heap, or on platforms without `execve`), so the
/// process Hashi spawns is the one doing the work and `kill_on_drop` really
/// stops it (`packages/cli/src/utils/relaunch.ts`). Update checks run only in
/// the interactive TUI and folder trust is off by default (and only narrows the
/// approval mode when on), so neither needs an override.
///
/// `QWEN_CODE_MEMORY_BASE_DIR` moves Qwen Code's auto-memory store
/// (`packages/core/src/memory/paths.ts`, `getMemoryBaseDir`) from the owner's
/// global Qwen directory into the run's own workspace, so memories written
/// during one token's runs are never read back in another token's runs.
#[must_use]
pub fn env(workdir: &Path) -> Vec<(String, String)> {
    let dir = std::path::absolute(workdir).unwrap_or_else(|_| workdir.to_path_buf());
    vec![
        ("QWEN_CODE_NO_RELAUNCH".to_owned(), "1".to_owned()),
        (
            "QWEN_CODE_MEMORY_BASE_DIR".to_owned(),
            dir.join(MEMORY_DIR).to_string_lossy().into_owned(),
        ),
    ]
}

/// Directory, inside the workspace, that holds Qwen Code's memory store.
pub const MEMORY_DIR: &str = ".qwen-memory";

/// Restricts which configured MCP servers Qwen Code connects to.
const MCP_ALLOWLIST_FLAG: &str = "--allowed-mcp-server-names";

/// Builds the argument vector for a headless Qwen Code run.
///
/// `--allowed-mcp-server-names ""` connects none of the owner's MCP servers:
/// the empty name is filtered out, leaving an empty allowlist that no server
/// matches (`packages/cli/src/config/config.ts`, and the "allow no MCP servers
/// if the flag is provided but empty" test). The option takes any number of
/// values, so it goes first, followed by another option. The owner opts in by
/// passing the flag with server names in `extra_args`, which replaces it.
#[must_use]
pub fn build_args(cfg: &AgentConfig, inv: Invocation<'_>) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    if !has_flag(&cfg.extra_args, &[MCP_ALLOWLIST_FLAG]) {
        args.extend([MCP_ALLOWLIST_FLAG.into(), "".into()]);
    }
    args.extend([
        "--output-format".into(),
        "stream-json".into(),
        "--approval-mode".into(),
        APPROVAL_MODE.into(),
    ]);
    if let Some(model) = inv.model {
        args.extend(["--model".into(), model.into()]);
    }
    if let Some(session) = inv.resume {
        args.extend(["--resume".into(), session.into()]);
    }
    args.extend(cfg.extra_args.iter().map(OsString::from));
    let mut prompt = OsString::from("--prompt=");
    prompt.push(neutralise(inv.prompt));
    args.push(prompt);
    args
}

/// Prefixes a space when the prompt would otherwise be read as a slash
/// command or have its surrounding quotes stripped.
fn neutralise(prompt: &str) -> String {
    let quoted = prompt.len() >= 2
        && ['"', '\'']
            .iter()
            .any(|&q| prompt.starts_with(q) && prompt.ends_with(q));
    if prompt.starts_with('/') || quoted {
        format!(" {prompt}")
    } else {
        prompt.to_owned()
    }
}

#[derive(Debug, Deserialize)]
struct ResultMessage {
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default)]
    usage: Option<RawUsage>,
    #[serde(default)]
    error: Option<RawError>,
}

#[derive(Debug, Deserialize)]
struct RawUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

#[derive(Debug, Deserialize)]
struct RawError {
    #[serde(default)]
    message: Option<String>,
}

/// Parses Qwen Code's `stream-json` output (one object per line). A single
/// JSON array of the same messages, as `--output-format json` prints, is
/// accepted too.
///
/// Token counts are taken as reported: Qwen Code's `input_tokens` is the
/// provider's prompt token count, which already includes cached input, so
/// `cache_read_input_tokens` is not added on top (unlike Claude Code).
///
/// # Errors
///
/// Returns [`AgentError::Reported`] when the result is an error and
/// [`AgentError::Parse`] when there is no result message.
pub fn parse_output(stdout: &str) -> Result<AgentOutput, AgentError> {
    let messages = messages(stdout);
    let session_hint = messages
        .iter()
        .find_map(|m| m.get("session_id").and_then(Value::as_str))
        .map(str::to_owned);

    let Some(raw) = messages.iter().rev().find(|m| is_top_level_result(m)) else {
        // Early fatal errors in JSON mode are a bare `{"error":{...}}` object.
        let reported = messages
            .iter()
            .rev()
            .find_map(|m| m.pointer("/error/message").and_then(Value::as_str));
        return Err(match reported {
            Some(msg) => AgentError::Reported(msg.to_owned()),
            None => AgentError::Parse("no result message in Qwen Code output".to_owned()),
        });
    };
    let msg: ResultMessage =
        serde_json::from_value(raw.clone()).map_err(|e| AgentError::Parse(e.to_string()))?;

    if msg.is_error {
        let reason = msg
            .error
            .and_then(|e| e.message)
            .or(msg.result)
            .filter(|r| !r.trim().is_empty())
            .or(msg.subtype)
            .unwrap_or_else(|| "Qwen Code reported an error".to_owned());
        return Err(AgentError::Reported(reason));
    }

    Ok(AgentOutput {
        text: msg.result.unwrap_or_default(),
        session_id: msg.session_id.or(session_hint),
        usage: msg.usage.map(|u| Usage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cost_usd: None,
        }),
    })
}

/// Every JSON object in `stdout`, in order.
fn messages(stdout: &str) -> Vec<Value> {
    if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(stdout.trim()) {
        return items.into_iter().filter(Value::is_object).collect();
    }
    stdout
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(Value::is_object)
        .collect()
}

/// A `result` message for the main agent, not a subagent.
fn is_top_level_result(m: &Value) -> bool {
    m.get("type").and_then(Value::as_str) == Some("result")
        && m.get("parent_tool_use_id").is_none_or(Value::is_null)
}

/// Qwen Code's quota messages (`packages/core/src/utils/quotaErrorDetection.ts`
/// and `retry.ts`): an exhausted token plan, an exceeded free allocation or
/// `insufficient_quota`, and the rewrite for the discontinued OAuth free tier.
const LIMIT_MESSAGES: [&str; 5] = [
    "quota exhausted",
    "quota has been exhausted",
    "free allocated quota exceeded",
    "insufficient_quota",
    "qwen oauth free tier has been discontinued",
];

/// Whether a failed run's error says this account is at its limit, and when
/// it resets if the message says: "The quota will reset at MM-DD HH:MM:SS
/// UTC." has no year, so it is the next such time. See
/// [`super::limit_hit`].
#[must_use]
pub fn limit_hit(error: &str) -> Option<Option<time::OffsetDateTime>> {
    if !super::limit::contains_any(error, &LIMIT_MESSAGES) {
        return None;
    }
    let now = time::OffsetDateTime::now_utc();
    Some(
        super::limit::after_marker(error, "reset at ")
            .and_then(|rest| super::limit::next_month_day_utc(&rest, now))
            .or_else(|| super::limit::reset_time(error, now)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentKind;

    const SUCCESS: &str = include_str!("../../tests/fixtures/qwen_success.jsonl");
    const FAILURE: &str = include_str!("../../tests/fixtures/qwen_failure.jsonl");
    const RESUME: &str = include_str!("../../tests/fixtures/qwen_resume.jsonl");

    const SESSION: &str = "9f1c0b2e-4d3a-4b5c-8e7f-6a5b4c3d2e1f";

    fn cfg() -> AgentConfig {
        AgentConfig {
            bin: "qwen".into(),
            ..AgentConfig::default_for(AgentKind::Qwen)
        }
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn args_for(prompt: &str, resume: Option<&str>, model: Option<&str>) -> Vec<String> {
        strings(&build_args(
            &cfg(),
            Invocation {
                prompt,
                resume,
                model,
            },
        ))
    }

    #[test]
    fn build_args_for_new_run() {
        assert_eq!(
            args_for("hi", None, None),
            [
                "--allowed-mcp-server-names",
                "",
                "--output-format",
                "stream-json",
                "--approval-mode",
                "default",
                "--prompt=hi"
            ]
        );
    }

    #[test]
    fn build_args_never_auto_approves() {
        let args = args_for("hi", Some(SESSION), Some("qwen3-coder-plus"));
        assert!(args.windows(2).any(|w| w == ["--approval-mode", "default"]));
        for banned in ["--yolo", "-y", "yolo", "auto", "auto-edit"] {
            assert!(!args.iter().any(|a| a == banned), "{banned} in {args:?}");
        }
    }

    #[test]
    fn build_args_resumes_by_session_id() {
        let args = args_for("again", Some(SESSION), None);
        assert!(args.windows(2).any(|w| w == ["--resume", SESSION]));
        assert!(
            !args
                .iter()
                .any(|a| a == "--session-id" || a == "--continue")
        );
        assert_eq!(args.last().map(String::as_str), Some("--prompt=again"));
    }

    #[test]
    fn build_args_passes_model_only_when_set() {
        let with = args_for("hi", None, Some("qwen3-coder-plus"));
        assert!(
            with.windows(2)
                .any(|w| w == ["--model", "qwen3-coder-plus"])
        );
        let without = args_for("hi", None, None);
        assert!(!without.iter().any(|a| a == "--model" || a == "-m"));
        assert!(!without.iter().any(|a| a == "--resume" || a == "-r"));
    }

    #[test]
    fn build_args_keeps_flag_looking_prompt_in_one_element() {
        let args = args_for("--yolo --help\nrm -rf /", None, None);
        assert_eq!(
            args.last().map(String::as_str),
            Some("--prompt=--yolo --help\nrm -rf /")
        );
        assert!(
            !args
                .iter()
                .any(|a| a == "--yolo" || a == "--help" || a == "--")
        );
        assert_eq!(args.iter().filter(|a| a.starts_with("--prompt")).count(), 1);
    }

    #[test]
    fn build_args_defuses_slash_commands_and_quotes() {
        let last = |p: &str| args_for(p, None, None).pop().unwrap_or_default();
        assert_eq!(
            last("/config tools.approvalMode=yolo"),
            "--prompt= /config tools.approvalMode=yolo"
        );
        assert_eq!(last("\"quoted\""), "--prompt= \"quoted\"");
        assert_eq!(last("'q'"), "--prompt= 'q'");
        assert_eq!(last("\"half"), "--prompt=\"half");
        assert_eq!(last("a /b"), "--prompt=a /b");
    }

    #[test]
    fn build_args_appends_extra_args_before_prompt() {
        let mut c = cfg();
        c.extra_args = vec!["--max-wall-time".into(), "120".into()];
        let args = strings(&build_args(
            &c,
            Invocation {
                prompt: "hi",
                resume: None,
                model: None,
            },
        ));
        assert_eq!(
            &args[args.len() - 3..],
            ["--max-wall-time", "120", "--prompt=hi"]
        );
    }

    #[test]
    fn env_disables_self_relaunch() {
        let env = env(Path::new("/tmp/ws"));
        assert!(
            env.iter()
                .any(|(k, v)| k == "QWEN_CODE_NO_RELAUNCH" && v == "1")
        );
    }

    #[test]
    fn env_keeps_memory_inside_the_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let env = env(workspace.path());
        let dir = &env
            .iter()
            .find(|(k, _)| k == "QWEN_CODE_MEMORY_BASE_DIR")
            .unwrap()
            .1;
        let expected = std::path::absolute(workspace.path())
            .unwrap()
            .join(MEMORY_DIR);
        assert_eq!(Path::new(dir), expected);
        assert!(Path::new(dir).starts_with(workspace.path()));
        // A relative workspace still yields an absolute path, since Qwen
        // Code resolves the variable against its own working directory.
        let env = super::env(Path::new("ws"));
        let dir = &env
            .iter()
            .find(|(k, _)| k == "QWEN_CODE_MEMORY_BASE_DIR")
            .unwrap()
            .1;
        assert!(Path::new(dir).is_absolute(), "{dir}");
    }

    #[test]
    fn build_args_connects_no_mcp_server_by_default() {
        let args = args_for("hi", Some(SESSION), Some("qwen3-coder-plus"));
        assert_eq!(
            &args[..3],
            ["--allowed-mcp-server-names", "", "--output-format"]
        );
    }

    #[test]
    fn owner_mcp_allowlist_replaces_the_default() {
        let mut c = cfg();
        c.extra_args = vec!["--allowed-mcp-server-names=docs".into()];
        let args = strings(&build_args(
            &c,
            Invocation {
                prompt: "hi",
                resume: None,
                model: None,
            },
        ));
        assert_eq!(
            args.iter()
                .filter(|a| a.starts_with("--allowed-mcp-server-names"))
                .count(),
            1
        );
        assert!(!args.iter().any(String::is_empty), "{args:?}");
    }

    #[test]
    fn parse_output_extracts_result_session_and_usage() {
        let out = parse_output(SUCCESS).unwrap();
        assert_eq!(out.text, "It defines `add`, which sums two integers.");
        assert_eq!(out.session_id.as_deref(), Some(SESSION));
        let usage = out.usage.unwrap();
        // Cached input is already part of `input_tokens`.
        assert_eq!(usage.input_tokens, 2052);
        assert_eq!(usage.output_tokens, 32);
        assert_eq!(usage.cost_usd, None);
    }

    #[test]
    fn parse_output_reads_resumed_session() {
        let out = parse_output(RESUME).unwrap();
        assert_eq!(out.text, "You asked what `add` does.");
        assert_eq!(out.session_id.as_deref(), Some(SESSION));
        assert_eq!(out.usage.map(|u| u.input_tokens), Some(2300));
    }

    #[test]
    fn parse_output_surfaces_reported_errors() {
        let err = parse_output(FAILURE).unwrap_err();
        assert!(matches!(err, AgentError::Reported(_)), "{err:?}");
        assert_eq!(err.to_string(), "401 Incorrect API key provided");
    }

    #[test]
    fn parse_output_falls_back_to_subtype() {
        let err = parse_output(r#"{"type":"result","subtype":"error_max_turns","is_error":true}"#)
            .unwrap_err();
        assert_eq!(err.to_string(), "error_max_turns");
    }

    #[test]
    fn parse_output_reports_bare_fatal_errors() {
        let err = parse_output(r#"{"error":{"type":"Error","message":"no auth","code":41}}"#)
            .unwrap_err();
        assert!(
            matches!(err, AgentError::Reported(ref m) if m == "no auth"),
            "{err:?}"
        );
    }

    #[test]
    fn parse_output_ignores_subagent_results() {
        let stdout = concat!(
            r#"{"type":"result","is_error":false,"result":"main","session_id":"s1"}"#,
            "\n",
            r#"{"type":"result","is_error":true,"parent_tool_use_id":"t1","error":{"message":"sub"}}"#,
        );
        assert_eq!(parse_output(stdout).unwrap().text, "main");
    }

    #[test]
    fn parse_output_accepts_json_array_mode() {
        let lines: Vec<&str> = SUCCESS.lines().collect();
        let array = format!("[{}]", lines.join(","));
        let out = parse_output(&array).unwrap();
        assert_eq!(out.text, "It defines `add`, which sums two integers.");
    }

    #[test]
    fn parse_output_rejects_malformed() {
        for bad in ["not json", "", "{\"type\":\"assistant\"}", "{broken"] {
            assert!(
                matches!(parse_output(bad), Err(AgentError::Parse(_))),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn limit_messages_are_recognised() {
        for msg in [
            "Free allocated quota exceeded.",
            "429 You exceeded your current quota (insufficient_quota)",
            "Qwen OAuth free tier has been discontinued. Switch to an API key.",
        ] {
            assert_eq!(limit_hit(msg), Some(None), "{msg}");
        }
        // The Gemini-era pattern is not Qwen Code's.
        assert!(
            limit_hit("Quota exceeded for quota metric 'Requests' and limit 'Requests per day'")
                .is_none()
        );
        assert!(limit_hit("quota exceeded").is_none());
    }

    #[test]
    fn token_plan_reset_is_the_next_such_time() {
        let now = time::OffsetDateTime::now_utc();
        let soon = now + time::Duration::hours(3);
        let msg = format!(
            "Quota exhausted: Your token-plan monthly quota has been exhausted. The quota will reset at {:02}-{:02} {:02}:{:02}:{:02} UTC.",
            u8::from(soon.month()),
            soon.day(),
            soon.hour(),
            soon.minute(),
            soon.second()
        );
        let at = limit_hit(&msg).unwrap().unwrap();
        assert!((at - soon).whole_seconds().abs() <= 1, "{at} vs {soon}");
        let past = now - time::Duration::hours(3);
        let msg = format!(
            "Quota exhausted: Your token-plan quota has been exhausted. The quota will reset at {:02}-{:02} {:02}:{:02}:{:02} UTC.",
            u8::from(past.month()),
            past.day(),
            past.hour(),
            past.minute(),
            past.second()
        );
        let at = limit_hit(&msg).unwrap().unwrap();
        assert!(at > now && at.year() >= now.year(), "{at}");
    }
}
