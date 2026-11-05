//! The `hashi report` bundle: a redacted Markdown file the user can read and
//! attach to an issue.

use std::fmt::Write as _;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

use super::{LOG_FILE, build_info, neutralize_controls, redact};
use crate::api::AgentInfo;
use crate::store::Store;
use crate::util::{human_age, human_duration, rfc3339, truncate};

/// How many failed requests the report lists.
const FAILED_LIMIT: u32 = 20;
/// How many trailing log lines the report includes.
const LOG_LINES: usize = 200;
/// Longest prompt excerpt with `--include-prompts`, in characters.
const PROMPT_CHARS: usize = 500;
/// Longest error text per request with `--include-prompts`, in characters.
const ERROR_CHARS: usize = 2_000;
/// Only this much of the end of the log is read.
const LOG_TAIL_BYTES: u64 = 512 * 1024;
/// Longest crash file included, in characters.
const CRASH_CHARS: usize = 64 * 1024;

/// Everything a report is built from, so it can be tested without the real
/// home directory.
#[derive(Debug)]
pub struct ReportInput<'a> {
    /// The database, or why it could not be opened.
    pub store: Result<&'a Store, String>,
    /// Directory holding `hashi.log` and crash files.
    pub logs_dir: &'a Path,
    /// Agent detection, as `hashi doctor` shows it.
    pub agents: &'a [AgentInfo],
    /// Where `cloudflared` was found, if anywhere.
    pub cloudflared: Option<&'a Path>,
    /// `HASHI_*` environment variables.
    pub env: &'a [(String, String)],
    /// Include prompt excerpts and the stored error text (which can be agent
    /// output) of failed requests, and agent errors in the log.
    pub include_prompts: bool,
    /// Generation time, Unix ms.
    pub now_ms: i64,
}

/// Builds the redacted Markdown report.
#[must_use]
pub fn build_report(input: &ReportInput<'_>) -> String {
    let mut out = String::new();
    let when = rfc3339(input.now_ms).unwrap_or_default();
    let prompts = if input.include_prompts {
        "Prompt excerpts and agent error output are included because `--include-prompts` \
         was passed."
    } else {
        "Prompts, responses and agent error output are left out."
    };
    let _ = writeln!(
        out,
        "# Hashi diagnostic report\n\nGenerated {when} by `hashi report`. Secrets are redacted \
         and {prompts_lower} Read it before you share it.\n",
        prompts_lower = lowercase_first(prompts)
    );

    section_environment(&mut out);
    section_agents(&mut out, input);
    section_settings(&mut out, input.env);
    match &input.store {
        Ok(store) => {
            section_database(&mut out, store, input.now_ms);
            section_failed(&mut out, store, input.include_prompts);
        }
        Err(why) => {
            let _ = writeln!(out, "## Database\n\nNot available: {why}\n");
        }
    }
    section_log(&mut out, input.logs_dir, input.include_prompts);
    section_crashes(&mut out, input.logs_dir);
    redact(&out)
}

fn lowercase_first(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map_or_else(String::new, |c| {
        c.to_lowercase().chain(chars).collect::<String>()
    })
}

/// `content` in a Markdown code block. Control and bidi characters are
/// neutralised, and the fence is longer than any backtick run inside, so the
/// content can never close the block and add Markdown of its own.
fn fenced(content: &str) -> String {
    let clean = neutralize_controls(content, true);
    let mut longest = 0;
    let mut current = 0;
    for c in clean.chars() {
        if c == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    let fence = "`".repeat((longest + 1).max(4));
    format!("{fence}text\n{clean}\n{fence}")
}

/// A label Hashi generates itself for a stored error. No stored text is
/// copied: the error column can hold agent stdout or stderr, and
/// `AgentError::Reported` is the model's own text with no prefix.
fn error_kind(error: &str) -> &'static str {
    const KINDS: &[(&str, &str)] = &[
        ("agent exited with", "agent exited unsuccessfully"),
        ("agent timed out", "agent timed out"),
        ("agent output exceeded", "agent output too large"),
        (
            "run exceeded its memory limit",
            "run exceeded its memory limit",
        ),
        ("failed to start", "agent could not be started"),
        (
            "could not parse agent output",
            "agent output could not be parsed",
        ),
        ("i/o error", "i/o error"),
    ];
    KINDS
        .iter()
        .find(|(prefix, _)| error.starts_with(prefix))
        .map_or(
            "error reported by the agent or another source",
            |(_, kind)| kind,
        )
}

fn section_environment(out: &mut String) {
    let _ = writeln!(
        out,
        "## Environment\n\n{}\n",
        fenced(build_info().trim_end())
    );
}

fn section_agents(out: &mut String, input: &ReportInput<'_>) {
    let _ = writeln!(out, "## Agents\n");
    for a in input.agents {
        match &a.version {
            Some(v) => {
                let _ = writeln!(out, "- {}: {v} (`{}`)", a.agent.display_name(), a.path);
            }
            None => {
                let _ = writeln!(
                    out,
                    "- {}: not found at `{}`",
                    a.agent.display_name(),
                    a.path
                );
            }
        }
    }
    match input.cloudflared {
        Some(p) => {
            let _ = writeln!(out, "- cloudflared: `{}`\n", p.display());
        }
        None => {
            let _ = writeln!(out, "- cloudflared: not installed\n");
        }
    }
}

/// Environment variables whose values are secrets, not settings.
fn is_secret_env(key: &str) -> bool {
    let k = key.to_ascii_uppercase();
    ["TOKEN", "SECRET", "PASSWORD", "API_KEY"]
        .iter()
        .any(|word| k.contains(word))
}

fn section_settings(out: &mut String, env: &[(String, String)]) {
    let _ = writeln!(out, "## Settings\n");
    if env.is_empty() {
        let _ = writeln!(out, "No `HASHI_*` environment variables are set.\n");
        return;
    }
    let mut env: Vec<&(String, String)> = env.iter().collect();
    env.sort();
    for (key, value) in env {
        if is_secret_env(key) {
            let _ = writeln!(out, "- `{key}`: set, value hidden");
        } else {
            let _ = writeln!(out, "- `{key}` = `{}`", value.replace('`', "'"));
        }
    }
    out.push('\n');
}

fn section_database(out: &mut String, store: &Store, now_ms: i64) {
    let _ = writeln!(out, "## Database\n");
    match store.schema_version() {
        Ok(v) => {
            let _ = writeln!(out, "- Schema version: {v}");
        }
        Err(e) => {
            let _ = writeln!(out, "- Schema version: unknown ({e})");
        }
    }
    match store.stats() {
        Ok(s) => {
            let _ = writeln!(
                out,
                "- Tokens: {}\n- Requests: {} total, {} queued, {} running, {} completed, {} failed",
                s.tokens, s.total, s.queued, s.running, s.completed, s.failed
            );
        }
        Err(e) => {
            let _ = writeln!(out, "- Counts unavailable: {e}");
        }
    }
    match store.list_invites() {
        Ok(invites) => {
            let count = |status: &str| {
                invites
                    .iter()
                    .filter(|i| i.status(now_ms) == status)
                    .count()
            };
            let _ = writeln!(
                out,
                "- Invites: {} pending, {} claimed, {} expired",
                count("pending"),
                count("claimed"),
                count("expired")
            );
        }
        Err(e) => {
            let _ = writeln!(out, "- Invites unavailable: {e}");
        }
    }
    out.push('\n');
}

fn section_failed(out: &mut String, store: &Store, include_prompts: bool) {
    let _ = writeln!(out, "## Recent failed requests\n");
    let failed = match store.failed_requests(FAILED_LIMIT) {
        Ok(f) => f,
        Err(e) => {
            let _ = writeln!(out, "Unavailable: {e}\n");
            return;
        }
    };
    if failed.is_empty() {
        let _ = writeln!(out, "None.\n");
        return;
    }
    for req in failed {
        let ran = req
            .duration_ms()
            .and_then(|ms| u64::try_from(ms).ok())
            .map_or_else(
                || "unknown".to_owned(),
                |ms| human_duration(Duration::from_millis(ms)),
            );
        let _ = writeln!(
            out,
            "### `{}`\n\n- Agent: {}\n- Age: {} ago\n- Ran for: {ran}\n",
            req.id,
            req.agent,
            human_age(req.created_at)
        );
        let error = req.error.as_deref().unwrap_or("no error text");
        if include_prompts {
            let _ = writeln!(out, "{}\n", fenced(&truncate(error, ERROR_CHARS)));
            let _ = writeln!(
                out,
                "Prompt:\n\n{}\n",
                fenced(&truncate(&req.message, PROMPT_CHARS))
            );
        } else {
            let _ = writeln!(
                out,
                "- Error: {} ({} chars, text left out; pass `--include-prompts` to include it)\n",
                error_kind(error),
                error.chars().count()
            );
        }
    }
}

/// The last `n` lines of `path`, reading at most [`LOG_TAIL_BYTES`].
fn tail_lines(path: &Path, n: usize) -> std::io::Result<Vec<String>> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(LOG_TAIL_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<&str> = text.lines().collect();
    if start > 0 && !lines.is_empty() {
        // The first line is probably cut in half.
        lines.remove(0);
    }
    let skip = lines.len().saturating_sub(n);
    Ok(lines[skip..].iter().map(|l| (*l).to_owned()).collect())
}

/// The queue logs an excerpt of each failed run's error, which can be agent
/// output. Without `--include-prompts` that field is cut from the line.
fn strip_agent_error(line: &str) -> String {
    const FIELD: &str = " error=";
    match line.find("request failed") {
        Some(at) => match line[at..].find(FIELD) {
            Some(rel) => format!("{}{FIELD}[left out]", &line[..at + rel]),
            None => line.to_owned(),
        },
        None => line.to_owned(),
    }
}

fn section_log(out: &mut String, logs_dir: &Path, include_agent_errors: bool) {
    let _ = writeln!(out, "## Log\n");
    match tail_lines(&logs_dir.join(LOG_FILE), LOG_LINES) {
        Ok(lines) if lines.is_empty() => {
            let _ = writeln!(out, "`{LOG_FILE}` is empty.\n");
        }
        Ok(lines) => {
            let lines: Vec<String> = if include_agent_errors {
                lines
            } else {
                lines.iter().map(|l| strip_agent_error(l)).collect()
            };
            let _ = writeln!(
                out,
                "Last {} lines of `{LOG_FILE}`:\n\n{}\n",
                lines.len(),
                fenced(&lines.join("\n"))
            );
        }
        Err(e) => {
            let _ = writeln!(out, "`{LOG_FILE}` could not be read: {e}\n");
        }
    }
}

fn crash_files(logs_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(logs_dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with("crash-") && n.ends_with(".txt"))
        .collect();
    // Timestamps in the names sort chronologically.
    names.sort();
    names
}

fn section_crashes(out: &mut String, logs_dir: &Path) {
    let _ = writeln!(out, "## Crash reports\n");
    let names = crash_files(logs_dir);
    let Some(latest) = names.last() else {
        let _ = writeln!(out, "None.\n");
        return;
    };
    for name in &names {
        let _ = writeln!(out, "- `{name}`");
    }
    match std::fs::read_to_string(logs_dir.join(latest)) {
        Ok(text) => {
            let _ = writeln!(
                out,
                "\nMost recent, `{latest}`:\n\n{}\n",
                fenced(&truncate(text.trim_end(), CRASH_CHARS))
            );
        }
        Err(e) => {
            let _ = writeln!(out, "\n`{latest}` could not be read: {e}\n");
        }
    }
}

/// The report for this machine: `store` (or why there is none), the agents
/// found, cloudflared, the `HASHI_*` variables and the logs under `home`.
/// `hashi report` and the dashboard both build it here.
#[must_use]
pub fn report_text(
    home: &Path,
    store: Result<&Store, String>,
    agents: &[AgentInfo],
    include_prompts: bool,
) -> String {
    let cloudflared = crate::tunnel::locate();
    let env: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| k.starts_with("HASHI_"))
        .collect();
    build_report(&ReportInput {
        store,
        logs_dir: &home.join("logs"),
        agents,
        cloudflared: cloudflared.as_deref(),
        env: &env,
        include_prompts,
        now_ms: crate::util::now_ms(),
    })
}

/// Writes a report owner-only to `out` (a file, or a directory to put
/// `hashi-report-<time>.md` in), or to the current directory when `out` is
/// `None`. Returns the path written, made absolute.
///
/// # Errors
///
/// Fails if the file cannot be written.
pub fn save_report(out: Option<&Path>, text: &str) -> std::io::Result<std::path::PathBuf> {
    let name = format!(
        "hashi-report-{}.md",
        super::file_timestamp(time::OffsetDateTime::now_utc())
    );
    let path = match out {
        Some(p) if p.is_dir() => p.join(name),
        Some(p) => p.to_path_buf(),
        None => std::path::PathBuf::from(name),
    };
    super::write_private(&path, text)?;
    Ok(std::path::absolute(&path).unwrap_or(path))
}

/// A prefilled GitHub new-issue link. Nothing is sent until the user opens it
/// and submits the form.
#[must_use]
pub fn issue_url() -> String {
    let title = format!(
        "Bug: (describe the problem) [hashi {} on {}]",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS
    );
    format!(
        "https://github.com/sidharthpunathil/hashi/issues/new?template=bug_report.md&title={}",
        percent_encode(&title)
    )
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentKind;
    use crate::store::{NewRequest, NewToken};
    use crate::token::{Limits, Role, SessionMode};

    const PROMPT: &str = "please refactor the secret-sauce module";
    /// Stands in for agent output stored as error text.
    const AGENT_TEXT: &str = "here is the answer so far: MARKER-STDOUT-9137";

    struct Fixture {
        store: Store,
        secret: String,
        logs: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        let store = Store::open_in_memory().unwrap();
        let (token, secret) = store
            .create_token(&NewToken {
                name: "alice-laptop".into(),
                role: Role::User,
                session_mode: SessionMode::Stateless,
                agents: vec![AgentKind::Claude],
                expires_in_days: None,
                note: None,
                limits: Limits::default(),
                accounts: Default::default(),
                project: None,
            })
            .unwrap();
        store
            .insert_request(NewRequest {
                id: "req-1",
                token: &token,
                agent: AgentKind::Claude,
                model: None,
                message: PROMPT,
            })
            .unwrap();
        store.claim_next(AgentKind::Claude).unwrap();
        store
            .fail_request(
                "req-1",
                &format!(
                    "agent exited with exit status: 1: used Bearer {} {AGENT_TEXT}",
                    secret.raw
                ),
            )
            .unwrap();

        let logs = tempfile::tempdir().unwrap();
        std::fs::write(
            logs.path().join(LOG_FILE),
            format!(
                "line one\nauth with {}\n2026-11-05T09:35:31Z  WARN hashi::queue: request failed \
                 id=req-1 agent=claude error_id=E-1a2b3c4d error=agent exited with 1: MARKER-LOG-5512\n\
                 line three\n",
                secret.raw
            ),
        )
        .unwrap();
        std::fs::write(logs.path().join("crash-20261101T000000Z.txt"), "old crash").unwrap();
        std::fs::write(logs.path().join("crash-20261105T000000Z.txt"), "new crash").unwrap();
        Fixture {
            store,
            secret: secret.raw,
            logs,
        }
    }

    fn report(f: &Fixture, include_prompts: bool) -> String {
        let env = vec![
            ("HASHI_PORT".to_owned(), "9000".to_owned()),
            (
                "HASHI_TUNNEL_TOKEN".to_owned(),
                "cf-secret-value".to_owned(),
            ),
        ];
        build_report(&ReportInput {
            store: Ok(&f.store),
            logs_dir: f.logs.path(),
            agents: &[AgentInfo {
                agent: AgentKind::Claude,
                available: true,
                version: Some("2.0.1".into()),
                path: "claude".into(),
            }],
            cloudflared: None,
            env: &env,
            include_prompts,
            now_ms: crate::util::now_ms(),
        })
    }

    #[test]
    fn report_never_contains_secrets_or_prompts() {
        let f = fixture();
        let text = report(&f, false);
        assert!(!text.contains(&f.secret), "token secret leaked");
        assert!(!text.contains(&f.secret[10..20]), "token secret leaked");
        assert!(!text.contains(PROMPT), "prompt leaked");
        assert!(!text.contains("secret-sauce"), "prompt leaked");
        assert!(!text.contains("cf-secret-value"), "tunnel token leaked");
        assert!(!text.contains("alice-laptop"), "token name leaked");
        // The useful parts are there.
        assert!(text.contains("`req-1`"));
        // Failed requests are described by Hashi's own label, never by
        // stored text, which can be agent output.
        assert!(!text.contains("used Bearer"), "error text leaked");
        assert!(!text.contains("MARKER-STDOUT-9137"), "agent output leaked");
        assert!(
            !text.contains("MARKER-LOG-5512"),
            "agent output leaked from the log"
        );
        assert!(text.contains("- Error: agent exited unsuccessfully ("));
        assert!(text.contains("request failed id=req-1 agent=claude error_id=E-1a2b3c4d"));
        assert!(text.contains("- Tokens: 1"));
        assert!(text.contains("1 failed"));
        assert!(text.contains("`HASHI_PORT` = `9000`"));
        assert!(text.contains("`HASHI_TUNNEL_TOKEN`: set, value hidden"));
        assert!(text.contains("line three"));
        assert!(text.contains("crash-20261101T000000Z.txt"));
        assert!(text.contains("new crash"));
        assert!(!text.contains("old crash"));
        assert!(text.contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn include_prompts_adds_them() {
        let f = fixture();
        let text = report(&f, true);
        assert!(text.contains(PROMPT));
        assert!(text.contains("used Bearer hashi_"));
        assert!(text.contains(AGENT_TEXT));
        assert!(text.contains("MARKER-LOG-5512"));
        assert!(!text.contains(&f.secret));
    }

    fn fail(f: &Fixture, id: &str, error: &str) {
        let token = f.store.list_tokens().unwrap().remove(0);
        f.store
            .insert_request(NewRequest {
                id,
                token: &token,
                agent: AgentKind::Claude,
                model: None,
                message: "hello",
            })
            .unwrap();
        f.store.claim_next(AgentKind::Claude).unwrap();
        f.store.fail_request(id, error).unwrap();
    }

    #[test]
    fn default_report_labels_errors_without_copying_them() {
        let f = fixture();
        // AgentError::Reported is the model's own text, with no prefix.
        fail(&f, "req-r", "model said MARKER-RESULT-4471");
        fail(&f, "req-t", "agent timed out after 300s");
        fail(
            &f,
            "req-s",
            "failed to start `/Users/alice/bin/claude`: No such file",
        );
        let text = report(&f, false);
        assert!(!text.contains("MARKER-RESULT-4471"));
        assert!(!text.contains("model said"));
        assert!(!text.contains("/bin/claude"));
        assert!(text.contains("- Error: error reported by the agent or another source (29 chars"));
        assert!(text.contains("- Error: agent timed out ("));
        assert!(text.contains("- Error: agent could not be started ("));
        assert!(text.contains("pass `--include-prompts` to include it"));
    }

    #[test]
    fn error_kinds_are_fixed_labels() {
        for (error, want) in [
            (
                "agent exited with exit status: 1: x",
                "agent exited unsuccessfully",
            ),
            ("agent timed out after 5s", "agent timed out"),
            ("agent output exceeded 10 bytes", "agent output too large"),
            ("failed to start `x`: y", "agent could not be started"),
            (
                "could not parse agent output: x",
                "agent output could not be parsed",
            ),
            ("i/o error: broken pipe", "i/o error"),
            (
                "run exceeded its memory limit (1 of 2)",
                "run exceeded its memory limit",
            ),
            (
                "anything else",
                "error reported by the agent or another source",
            ),
        ] {
            assert_eq!(error_kind(error), want, "{error}");
        }
    }

    #[test]
    fn included_errors_cannot_escape_their_fence_or_the_terminal() {
        let f = fixture();
        fail(
            &f,
            "req-x",
            "bad\n``````\n# Injected heading\n\u{1b}]0;owned\u{7}\u{1b}[2J\u{202e}end",
        );
        let text = report(&f, true);
        assert!(
            !text
                .chars()
                .any(|c| c == '\u{1b}' || c == '\u{7}' || c == '\u{202e}')
        );
        // The block opens with a fence longer than the six backticks inside.
        let at = text.find("# Injected heading").unwrap();
        let open = text[..at].rfind("text\nbad\n").unwrap();
        let fence: String = text[..open]
            .chars()
            .rev()
            .take_while(|c| *c == '`')
            .collect();
        assert!(fence.len() > 6, "fence {fence:?}");
        assert!(text[at..].contains(&format!("\n{fence}\n")));
    }

    #[test]
    fn fences_outgrow_backtick_runs() {
        assert_eq!(fenced("plain"), "````text\nplain\n````");
        assert_eq!(fenced("a `````` b"), "```````text\na `````` b\n```````");
        assert_eq!(fenced("x\u{1b}[2Jy"), "````text\nx?[2Jy\n````");
    }

    #[test]
    fn prompts_are_truncated() {
        let f = fixture();
        let token = f.store.list_tokens().unwrap().remove(0);
        let long = "y".repeat(2_000);
        f.store
            .insert_request(NewRequest {
                id: "req-2",
                token: &token,
                agent: AgentKind::Claude,
                model: None,
                message: &long,
            })
            .unwrap();
        f.store.claim_next(AgentKind::Claude).unwrap();
        f.store.fail_request("req-2", "boom").unwrap();
        let text = report(&f, true);
        assert!(text.contains(&format!("{}…", "y".repeat(PROMPT_CHARS - 1))));
        assert!(!text.contains(&"y".repeat(PROMPT_CHARS)));
    }

    #[test]
    fn report_without_database_still_renders() {
        let logs = tempfile::tempdir().unwrap();
        let text = build_report(&ReportInput {
            store: Err("no database yet".into()),
            logs_dir: logs.path(),
            agents: &[],
            cloudflared: None,
            env: &[],
            include_prompts: false,
            now_ms: 0,
        });
        assert!(text.contains("Not available: no database yet"));
        assert!(text.contains("could not be read"));
        assert!(text.contains("## Crash reports\n\nNone."));
    }

    #[test]
    fn log_tail_keeps_the_last_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOG_FILE);
        let body: String = (0..500).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&path, body).unwrap();
        let lines = tail_lines(&path, LOG_LINES).unwrap();
        assert_eq!(lines.len(), LOG_LINES);
        assert_eq!(lines[0], "line 300");
        assert_eq!(lines[LOG_LINES - 1], "line 499");
    }

    #[test]
    fn issue_url_is_encoded() {
        let url = issue_url();
        assert!(url.starts_with(
            "https://github.com/sidharthpunathil/hashi/issues/new?template=bug_report.md&title=Bug%3A%20"
        ));
        assert!(!url[8..].contains(' '));
    }
}
