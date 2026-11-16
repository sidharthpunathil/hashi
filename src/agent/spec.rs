//! Table-driven backends.
//!
//! The first six agents (Claude Code, Codex, Gemini CLI, opencode, Qwen Code
//! and Cursor CLI) each have a module of their own. Every other agent is
//! described by one [`Spec`] in [`super::specs`]: how to call it for one
//! headless prompt, which flags must never reach it, how to read its output,
//! and how its accounts and limits work. This module turns a spec into the
//! same three pure halves a hand-written backend has: [`build_args`],
//! [`env`] and [`parse_output`].
//!
//! A spec only says what was read in the agent's source or documentation;
//! the research notes for each are in `docs/research/agent-clis.md`.

use std::ffi::OsString;

use serde_json::Value;

use super::{AgentConfig, AgentError, AgentKind, AgentOutput, Invocation, Usage};

/// How the prompt reaches the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    /// As the last argument after `--`, which the agent's parser honours.
    DashDash,
    /// After `--`, one argument per space-separated piece. For opencode 1.x
    /// and its forks, which wrap an argument containing a space in literal
    /// quotes before joining the arguments back with spaces.
    DashDashWords,
    /// As the last argument with no `--`. A prompt that starts with `-`
    /// gets a leading space, so it is never read as a flag.
    Positional,
    /// As one argument `<flag>=<prompt>`, which every common parser reads as
    /// the flag's value even when the prompt starts with `-`.
    FlagEq(&'static str),
}

/// Fields of one JSON object, as RFC 6901 pointers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fields {
    /// The answer text.
    pub text: &'static str,
    /// The session id to resume.
    pub session: Option<&'static str>,
    /// Input (prompt) tokens.
    pub input_tokens: Option<&'static str>,
    /// Output (completion) tokens.
    pub output_tokens: Option<&'static str>,
    /// Cost in USD.
    pub cost: Option<&'static str>,
    /// A failure message: when it is present and not empty (or `true`), the
    /// run failed.
    pub error: Option<&'static str>,
}

/// An event stream, one JSON object per line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Events {
    /// Pointer to each event's type.
    pub kind: &'static str,
    /// The type of the events that carry answer text.
    pub text_kind: &'static str,
    /// Pointer to the text within such an event.
    pub text: &'static str,
    /// Join every text event (`true`) or keep only the last one.
    pub concat: bool,
    /// Pointer to the session id, read from any event.
    pub session: Option<&'static str>,
    /// The type of the events that carry usage, which are summed.
    pub usage_kind: Option<&'static str>,
    /// Pointer to input tokens within a usage event.
    pub input_tokens: Option<&'static str>,
    /// Pointer to output tokens within a usage event.
    pub output_tokens: Option<&'static str>,
    /// Pointer to the cost in USD within a usage event.
    pub cost: Option<&'static str>,
    /// The type of the events that mean the run failed.
    pub error_kind: Option<&'static str>,
    /// Pointer to the message within such an event.
    pub error: Option<&'static str>,
}

/// How to read what the agent printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// Standard output is the answer, with terminal escapes removed.
    Text,
    /// Claude Code's `--output-format json` result object, read by
    /// [`super::claude::parse_output`].
    ClaudeResult,
    /// One JSON object, or the last JSON line.
    Json(Fields),
    /// A JSON event stream.
    Jsonl(Events),
}

/// How to sign a login account in, run on the owner's terminal with the
/// account directory variable set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Login {
    /// Arguments after the executable.
    pub args: &'static [&'static str],
    /// What to do once the CLI is open, when it is not automatic.
    pub hint: Option<&'static str>,
}

/// Where an agent keeps its login, for `hashi switch`: plain files in one
/// folder, which its [`Spec::dir_var`] moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Switch {
    /// The folder the plain command uses, relative to the user's home,
    /// with `/` between parts (such as `.grok`).
    pub live_dir: &'static str,
    /// Where the same folder is under the account directory that
    /// [`Spec::dir_var`] points at: `""` for the directory itself.
    pub sub: &'static str,
    /// The files that hold the login, relative to the folder; the first
    /// must exist for the agent to count as signed in.
    pub files: &'static [&'static str],
    /// Pointer to an expiry (a number, or a timestamp that sorts as text)
    /// in the first file, so a login a run refreshed is never replaced by
    /// an older copy.
    pub expiry: Option<&'static str>,
    /// Where the signed-in user can be read, to show who the plain command
    /// signs in as.
    pub identity: Option<Identity>,
}

/// Where a login file names its user: a string at `pointer` in `file`,
/// or, with `jwt_claim`, that claim of the JSON Web Token found there.
/// Only the name is read; nothing secret is kept or shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    /// The file, relative to the login folder.
    pub file: &'static str,
    /// Pointer to the user name, email or token.
    pub pointer: &'static str,
    /// The claim to read when the pointer holds a token.
    pub jwt_claim: Option<&'static str>,
}

/// Everything Hashi knows about one table-driven agent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spec {
    /// The agent.
    pub kind: AgentKind,
    /// Stable lowercase identifier used in the API and database.
    pub id: &'static str,
    /// Name shown to humans.
    pub display: &'static str,
    /// Executable names looked up on `PATH`, most specific first.
    pub binaries: &'static [&'static str],
    /// Other names accepted for the agent.
    pub aliases: &'static [&'static str],
    /// Arguments that always come first: the headless subcommand and the
    /// output format.
    pub base_args: &'static [&'static str],
    /// Flags that keep a shared run safe (no MCP servers, read-only tools,
    /// no update check), each with its value. A group is skipped when the
    /// operator's extra arguments already set its flag.
    pub pinned_args: &'static [&'static [&'static str]],
    /// How the prompt is passed.
    pub prompt: Prompt,
    /// Option that takes the model as its next argument.
    pub model_flag: Option<&'static str>,
    /// Option that takes a session id to resume as its next argument.
    pub resume_flag: Option<&'static str>,
    /// Whether the model may be passed together with a session to resume.
    /// Some agents refuse a model for a session that already has one.
    pub model_on_resume: bool,
    /// Fixed environment for an unattended run.
    pub env: &'static [(&'static str, &'static str)],
    /// Flags that grant tools, shell or writes without asking. They are
    /// removed from the operator's extra arguments.
    pub unsafe_flags: &'static [&'static str],
    /// Whether a headless run can use shell or write tools without asking,
    /// even with [`Self::pinned_args`]. Such an agent is offered only when
    /// the owner turns it on with `--enable-agents`.
    pub auto_approves: bool,
    /// Whether a prompt starting with `/` runs one of the agent's commands
    /// (or `!` a shell command, or `@` attaches a file). Such a prompt then
    /// gets a leading space.
    pub slash_commands: bool,
    /// Whether a one-word prompt can be taken for a subcommand even after
    /// `--`. Such a prompt then gets a trailing space.
    pub word_commands: bool,
    /// How to read the output.
    pub output: Output,
    /// Variable that moves all of the agent's login state to a directory,
    /// for login accounts.
    pub dir_var: Option<&'static str>,
    /// Fixed environment for a run on a non-default account, on top of
    /// [`Self::env`] (for example turning off an OS keyring the account
    /// directory does not cover).
    pub account_env: &'static [(&'static str, &'static str)],
    /// Variable that carries an API key, for key accounts.
    pub key_var: Option<&'static str>,
    /// Variables that pick credentials, a provider or an endpoint. A run on
    /// a non-default account removes the owner's.
    pub credential_vars: &'static [&'static str],
    /// How to sign a login account in.
    pub login: Option<Login>,
    /// Where the login lives, when the plain command can be switched.
    pub switch: Option<Switch>,
    /// Arguments of a command that says whether a login account is signed
    /// in: by a `loggedIn` field when it prints JSON with one, else by its
    /// exit status.
    pub status_args: Option<&'static [&'static str]>,
    /// Lowercase parts of the agent's usage-limit messages.
    pub limit_messages: &'static [&'static str],
    /// Lowercase parts of the messages a plain-text agent prints, still
    /// exiting with 0, when the run failed. A text answer that contains one
    /// (or a limit message) is reported as a failure.
    pub text_errors: &'static [&'static str],
    /// The official install command, shown by `hashi doctor`.
    pub install: Option<&'static str>,
    /// Accent colour in the dashboard.
    pub color: (u8, u8, u8),
    /// Glyph in the dashboard.
    pub glyph: &'static str,
}

impl Spec {
    /// Whether `name` (already lowercased) names this agent.
    #[must_use]
    pub fn named(&self, name: &str) -> bool {
        self.id == name || self.aliases.contains(&name) || self.binaries.contains(&name)
    }
}

/// Value in [`Spec::env`] replaced with the run's working directory, for
/// agents that read their project root from a variable such as `PWD`.
pub const WORKDIR: &str = "{workdir}";

/// Fixed environment for the child, with [`WORKDIR`] filled in.
#[must_use]
pub fn env(spec: &Spec, workdir: &std::path::Path) -> Vec<(String, String)> {
    spec.env
        .iter()
        .map(|(k, v)| {
            let value = if *v == WORKDIR {
                workdir.to_string_lossy().into_owned()
            } else {
                (*v).to_owned()
            };
            ((*k).to_owned(), value)
        })
        .collect()
}

/// Builds the argument vector for one headless prompt.
#[must_use]
pub fn build_args(spec: &Spec, cfg: &AgentConfig, inv: Invocation<'_>) -> Vec<OsString> {
    let extra = safe_extra(&cfg.extra_args, spec.unsafe_flags);

    let mut args: Vec<OsString> = spec.base_args.iter().map(OsString::from).collect();
    for group in spec.pinned_args {
        let Some(first) = group.first() else { continue };
        let name = flag_name(first);
        // A flag pinned more than once (`--deny-tool=shell`,
        // `--deny-tool=write`) is a list, which the operator's own copy
        // adds to rather than replaces.
        let repeated = spec
            .pinned_args
            .iter()
            .filter(|g| g.first().map(|f| flag_name(f)) == Some(name))
            .count()
            > 1;
        if repeated || !super::has_flag(&extra, &[name]) {
            args.extend(group.iter().map(OsString::from));
        }
    }
    let model = inv
        .model
        .filter(|_| spec.model_on_resume || inv.resume.is_none());
    if let (Some(flag), Some(model)) = (spec.model_flag, model) {
        args.extend([flag.into(), model.into()]);
    }
    if let (Some(flag), Some(session)) = (spec.resume_flag, inv.resume) {
        args.extend([flag.into(), session.into()]);
    }
    args.extend(extra.into_iter().map(OsString::from));

    let mut prompt = inv.prompt.to_owned();
    if spec.slash_commands && prompt.starts_with(['/', '!', '@']) {
        prompt.insert(0, ' ');
    }
    // Even after `--`, some CLIs scan the raw argument list for options
    // such as `--debug-file=<path>` or `--config=<dir>` before parsing, so a
    // prompt never starts with `-`.
    if prompt.starts_with('-') {
        prompt.insert(0, ' ');
    }
    if spec.word_commands && !prompt.trim().contains(char::is_whitespace) {
        prompt.push(' ');
    }
    match spec.prompt {
        Prompt::DashDash => {
            args.push("--".into());
            args.push(prompt.into());
        }
        Prompt::DashDashWords => {
            args.push("--".into());
            // A word that starts with `-` would be an argument of its own
            // that looks like an option, so such a prompt goes as one
            // argument instead (it then reaches the model in quotes).
            if prompt.split(' ').any(|w| w.starts_with('-')) {
                args.push(prompt.into());
            } else {
                args.extend(prompt.split(' ').map(OsString::from));
            }
        }
        Prompt::Positional => args.push(prompt.into()),
        Prompt::FlagEq(flag) => args.push(format!("{flag}={prompt}").into()),
    }
    args
}

fn flag_name(arg: &str) -> &str {
    arg.split_once('=').map_or(arg, |(n, _)| n)
}

/// The operator's extra arguments without the `unsafe` flags. A flag given
/// alone also loses the value after it, when that value does not look like
/// a flag, so `--permission-mode bypassPermissions` goes as a whole.
fn safe_extra(extra: &[String], unsafe_flags: &[&str]) -> Vec<String> {
    let mut out = Vec::with_capacity(extra.len());
    let mut iter = extra.iter().peekable();
    while let Some(arg) = iter.next() {
        match arg.split_once('=') {
            Some((name, _)) if unsafe_flags.contains(&name) => {}
            None if unsafe_flags.contains(&arg.as_str()) => {
                iter.next_if(|next| !next.starts_with('-'));
            }
            _ => out.push(arg.clone()),
        }
    }
    out
}

/// Reads the agent's output. `success` is whether it exited with 0: plain
/// text from a failed run is an error, not an answer.
///
/// # Errors
///
/// [`AgentError::Parse`] when the output has no answer,
/// [`AgentError::Reported`] when it says the run failed.
pub fn parse_output(spec: &Spec, stdout: &str, success: bool) -> Result<AgentOutput, AgentError> {
    match spec.output {
        Output::Text => parse_text(spec, stdout, success),
        Output::ClaudeResult => {
            super::claude::parse_output(stdout).map_err(|e| better_claude_error(e, stdout))
        }
        Output::Json(fields) => after_exit(parse_json(stdout, &fields), success),
        Output::Jsonl(events) => after_exit(parse_events(stdout, &events), success),
    }
}

/// Claude Code clones (Qoder, CodeBuddy) put an error result's message in
/// `errors[0]` rather than `result`, which leaves only the subtype; this
/// swaps in that message when there is one.
fn better_claude_error(err: AgentError, stdout: &str) -> AgentError {
    let AgentError::Reported(_) = err else {
        return err;
    };
    let message = stdout
        .lines()
        .rev()
        .map(str::trim)
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v.get("type").and_then(Value::as_str) == Some("result"))
        .and_then(|v| {
            // OpenClaude puts a diagnostic line first.
            v.get("errors")?
                .as_array()?
                .iter()
                .filter_map(|e| e.as_str().or_else(|| e.get("message")?.as_str()))
                .find(|m| !m.starts_with("[ede_diagnostic]"))
                .map(str::to_owned)
        });
    match message {
        Some(m) if !m.trim().is_empty() => AgentError::Reported(m),
        _ => err,
    }
}

/// A run that exited unsuccessfully failed even when its output parsed,
/// unless the output already said why: the error then comes from stderr.
fn after_exit(
    parsed: Result<AgentOutput, AgentError>,
    success: bool,
) -> Result<AgentOutput, AgentError> {
    match parsed {
        Ok(_) if !success => Err(AgentError::Parse(
            "the agent exited unsuccessfully".to_owned(),
        )),
        other => other,
    }
}

fn parse_text(spec: &Spec, stdout: &str, success: bool) -> Result<AgentOutput, AgentError> {
    let text = strip_ansi(stdout).trim().to_owned();
    if !success || text.is_empty() {
        return Err(AgentError::Parse("no answer in output".to_owned()));
    }
    if super::limit::contains_any(&text, spec.text_errors)
        || super::limit::contains_any(&text, spec.limit_messages)
    {
        return Err(AgentError::Reported(text));
    }
    Ok(AgentOutput {
        text,
        ..AgentOutput::default()
    })
}

fn parse_json(stdout: &str, f: &Fields) -> Result<AgentOutput, AgentError> {
    let trimmed = stdout.trim();
    // A pretty-printed object spans many lines; otherwise take the last
    // line that is an object, since diagnostics may come before it.
    let value: Value = serde_json::from_str(trimmed)
        .ok()
        .filter(Value::is_object)
        .or_else(|| {
            trimmed
                .lines()
                .rev()
                .map(str::trim)
                .filter(|l| l.starts_with('{'))
                .find_map(|l| serde_json::from_str(l).ok())
        })
        .ok_or_else(|| AgentError::Parse("no JSON result in output".to_owned()))?;

    if let Some(msg) = f.error.and_then(|p| failure(&value, p)) {
        return Err(AgentError::Reported(msg));
    }
    let text = value
        .pointer(f.text)
        .and_then(text_of)
        .ok_or_else(|| AgentError::Parse(format!("result has no `{}`", f.text)))?;
    let usage = usage_of(&value, f.input_tokens, f.output_tokens, f.cost);
    Ok(AgentOutput {
        text,
        session_id: f.session.and_then(|p| string_at(&value, p)),
        usage,
        rate_limits: Vec::new(),
    })
}

fn parse_events(stdout: &str, e: &Events) -> Result<AgentOutput, AgentError> {
    let mut texts: Vec<String> = Vec::new();
    let mut session = None;
    let mut usage: Option<Usage> = None;
    let mut seen = false;
    // A failure is held rather than returned, because agents that retry
    // inside one run report the failed attempt and then answer.
    let mut failed: Option<String> = None;
    // An event type shared with text or usage events is a failure only
    // when its error field says so.
    let shared = e.error_kind.is_some()
        && (e.error_kind == Some(e.text_kind) || e.error_kind == e.usage_kind);
    for line in stdout.lines().map(str::trim).filter(|l| l.starts_with('{')) {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        seen = true;
        if let Some(id) = e.session.and_then(|p| string_at(&event, p)) {
            session = Some(id);
        }
        let kind = event.pointer(e.kind).and_then(tag);
        if kind.is_some() && kind == e.error_kind {
            let msg = e.error.and_then(|p| failure(&event, p));
            if msg.is_some() || !shared {
                failed = Some(msg.unwrap_or_else(|| "the agent reported an error".to_owned()));
                continue;
            }
        }
        if kind == Some(e.text_kind)
            && let Some(t) = event.pointer(e.text).and_then(text_of)
        {
            if !t.trim().is_empty() {
                failed = None;
            }
            texts.push(t);
        }
        if kind.is_some()
            && kind == e.usage_kind
            && let Some(u) = usage_of(&event, e.input_tokens, e.output_tokens, e.cost)
        {
            usage = Some(add(usage, u));
        }
    }
    if !seen {
        return Err(AgentError::Parse("no JSON events in output".to_owned()));
    }
    if let Some(msg) = failed {
        return Err(AgentError::Reported(msg));
    }
    let text = if e.concat {
        texts.join("")
    } else {
        texts.pop().unwrap_or_default()
    };
    if text.trim().is_empty() {
        return Err(AgentError::Parse("no answer in output".to_owned()));
    }
    Ok(AgentOutput {
        text,
        session_id: session,
        usage,
        rate_limits: Vec::new(),
    })
}

/// An event's type: a string, or the only key of an object (an externally
/// tagged enum such as `{"event":{"AgentMessage":"OK"}}`).
fn tag(v: &Value) -> Option<&str> {
    match v {
        Value::String(s) => Some(s),
        Value::Object(o) if o.len() == 1 => o.keys().next().map(String::as_str),
        _ => None,
    }
}

/// The failure message at `pointer`, if the value there says the run failed:
/// a non-empty string, `true`, or an object with a `message`.
fn failure(value: &Value, pointer: &str) -> Option<String> {
    match value.pointer(pointer)? {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Bool(true) => Some(
            value
                .pointer("/result")
                .and_then(text_of)
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "the agent reported an error".to_owned()),
        ),
        Value::Object(o) => Some(
            o.get("message")
                .and_then(Value::as_str)
                .unwrap_or("the agent reported an error")
                .to_owned(),
        ),
        _ => None,
    }
}

/// Text at a pointer: a string, or the joined `text` of an array of parts.
fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => {
            let joined: String = parts
                .iter()
                .filter_map(|p| p.as_str().or_else(|| p.get("text").and_then(Value::as_str)))
                .collect();
            Some(joined)
        }
        _ => None,
    }
}

fn string_at(v: &Value, pointer: &str) -> Option<String> {
    match v.pointer(pointer)? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn usage_of(
    v: &Value,
    input: Option<&str>,
    output: Option<&str>,
    cost: Option<&str>,
) -> Option<Usage> {
    let num = |p: Option<&str>| p.and_then(|p| v.pointer(p)).and_then(Value::as_u64);
    let (i, o) = (num(input), num(output));
    let c = cost.and_then(|p| v.pointer(p)).and_then(Value::as_f64);
    (i.is_some() || o.is_some() || c.is_some()).then(|| Usage {
        input_tokens: i.unwrap_or(0),
        output_tokens: o.unwrap_or(0),
        cost_usd: c,
    })
}

fn add(sum: Option<Usage>, u: Usage) -> Usage {
    let Some(s) = sum else { return u };
    Usage {
        input_tokens: s.input_tokens + u.input_tokens,
        output_tokens: s.output_tokens + u.output_tokens,
        cost_usd: match (s.cost_usd, u.cost_usd) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        },
    }
}

/// Removes terminal escape sequences (CSI and OSC) from `s`.
#[must_use]
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                // CSI: parameters, then one final byte in `@`..=`~`.
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                // OSC: ends with BEL or ESC `\`.
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' {
                        chars.next_if_eq(&'\\');
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Whether a failed run's error says this account is at its limit, and when
/// it resets if the message says. See [`super::limit_hit`].
#[must_use]
pub fn limit_hit(spec: &Spec, error: &str) -> Option<Option<time::OffsetDateTime>> {
    if spec.output == Output::ClaudeResult
        && let Some(hit) = super::claude::limit_hit(error)
    {
        return Some(hit);
    }
    super::limit::contains_any(error, spec.limit_messages)
        .then(|| super::limit::hit(error))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn base() -> Spec {
        Spec {
            kind: AgentKind::Claude,
            id: "x",
            display: "X",
            binaries: &["x"],
            aliases: &[],
            base_args: &["run"],
            pinned_args: &[&["--no-mcp"], &["--mode", "safe"]],
            prompt: Prompt::DashDash,
            model_flag: Some("--model"),
            resume_flag: Some("--resume"),
            model_on_resume: true,
            env: &[("X_NO_UPDATE", "1")],
            unsafe_flags: &["--yolo", "-y"],
            auto_approves: false,
            slash_commands: true,
            word_commands: false,
            output: Output::Text,
            dir_var: None,
            account_env: &[],
            key_var: None,
            credential_vars: &[],
            login: None,
            switch: None,
            status_args: None,
            limit_messages: &["rate limit"],
            text_errors: &["litellm."],
            install: None,
            color: (1, 2, 3),
            glyph: "x",
        }
    }

    fn cfg(extra: &[&str]) -> AgentConfig {
        AgentConfig {
            bin: "x".into(),
            max_turns: 1,
            permission_mode: None,
            sandbox: "read-only".into(),
            extra_args: extra.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn inv(prompt: &str) -> Invocation<'_> {
        Invocation {
            prompt,
            resume: Some("s1"),
            model: Some("m1"),
        }
    }

    #[test]
    fn args_put_prompt_after_dashdash_and_strip_unsafe_flags() {
        let args = build_args(
            &base(),
            &cfg(&["--yolo", "on", "-y", "--yolo=true", "--keep", "--mode", "x"]),
            inv("/help"),
        );
        assert_eq!(
            args,
            [
                "run", "--no-mcp", "--model", "m1", "--resume", "s1", "--keep", "--mode", "x",
                "--", " /help"
            ]
            .map(OsString::from)
        );
    }

    #[test]
    fn pinned_flag_is_skipped_when_operator_sets_it() {
        let args = build_args(&base(), &cfg(&["--no-mcp=false"]), inv("hi"));
        assert_eq!(
            args.iter()
                .filter(|a| a.to_string_lossy().starts_with("--no-mcp"))
                .count(),
            1
        );
    }

    #[test]
    fn one_word_prompt_gets_a_trailing_space_when_asked() {
        let spec = Spec {
            word_commands: true,
            ..base()
        };
        assert_eq!(
            build_args(&spec, &cfg(&[]), inv("update")).last().unwrap(),
            "update "
        );
        assert_eq!(
            build_args(&spec, &cfg(&[]), inv("a b")).last().unwrap(),
            "a b"
        );
    }

    #[test]
    fn json_from_failed_exit_is_an_error() {
        let err = parse_output(&JSON, r#"{"response":"partial"}"#, false);
        assert!(matches!(err, Err(AgentError::Parse(_))));
    }

    #[test]
    fn dashdash_prompt_never_starts_with_a_dash() {
        let args = build_args(&base(), &cfg(&[]), inv("--debug-file=/home/me/.zshrc"));
        assert_eq!(args.last().unwrap(), " --debug-file=/home/me/.zshrc");
        let spec = Spec {
            prompt: Prompt::DashDashWords,
            ..base()
        };
        let args = build_args(&spec, &cfg(&[]), inv("hi --config=/x"));
        assert_eq!(args.last().unwrap(), "hi --config=/x");
        assert_eq!(args[args.len() - 2], "--");
    }

    #[test]
    fn model_is_dropped_on_resume_when_the_agent_refuses_both() {
        let spec = Spec {
            model_on_resume: false,
            ..base()
        };
        let args = build_args(&spec, &cfg(&[]), inv("hi"));
        assert!(!args.iter().any(|a| a == "--model"));
        assert!(args.iter().any(|a| a == "--resume"));
    }

    #[test]
    fn repeated_pinned_flags_are_kept_beside_the_operators() {
        let spec = Spec {
            pinned_args: &[&["--deny=a"], &["--deny=b"]],
            ..base()
        };
        let args = build_args(&spec, &cfg(&["--deny=c"]), inv("hi"));
        for want in ["--deny=a", "--deny=b", "--deny=c"] {
            assert!(args.iter().any(|a| a == want), "{want}");
        }
    }

    #[test]
    fn text_that_says_it_failed_is_an_error() {
        let err = parse_output(&base(), "litellm.APIError: boom", true);
        assert!(matches!(err, Err(AgentError::Reported(_))));
        let err = parse_output(&base(), "Rate limit reached for model", true);
        assert!(matches!(err, Err(AgentError::Reported(_))));
    }

    #[test]
    fn openclaude_diagnostic_line_is_skipped() {
        let spec = Spec {
            output: Output::ClaudeResult,
            ..base()
        };
        let out = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["[ede_diagnostic] x","429 rate limited"]}"#;
        assert!(
            matches!(parse_output(&spec, out, false), Err(AgentError::Reported(m)) if m == "429 rate limited")
        );
    }

    #[test]
    fn positional_prompt_starting_with_dash_is_not_a_flag() {
        let spec = Spec {
            prompt: Prompt::Positional,
            ..base()
        };
        let args = build_args(&spec, &cfg(&[]), inv("--version"));
        assert_eq!(args.last().unwrap(), " --version");
    }

    #[test]
    fn flag_eq_prompt_is_one_argument() {
        let spec = Spec {
            prompt: Prompt::FlagEq("--text"),
            slash_commands: false,
            ..base()
        };
        let args = build_args(&spec, &cfg(&[]), inv("-rf /"));
        assert_eq!(args.last().unwrap(), "--text= -rf /");
    }

    #[test]
    fn text_output_strips_escapes_and_fails_on_bad_exit() {
        let out = parse_output(&base(), "\u{1b}[1mOK\u{1b}[0m\n", true).unwrap();
        assert_eq!(out.text, "OK");
        assert!(matches!(
            parse_output(&base(), "OK", false),
            Err(AgentError::Parse(_))
        ));
        assert!(matches!(
            parse_output(&base(), "  \n", true),
            Err(AgentError::Parse(_))
        ));
    }

    #[test]
    fn strip_ansi_handles_osc() {
        assert_eq!(
            strip_ansi("a\u{1b}]0;title\u{7}b\u{1b}]8;;u\u{1b}\\c"),
            "abc"
        );
    }

    const JSON: Spec = Spec {
        output: Output::Json(Fields {
            text: "/response",
            session: Some("/session_id"),
            input_tokens: Some("/stats/in"),
            output_tokens: Some("/stats/out"),
            cost: Some("/cost"),
            error: Some("/error"),
        }),
        ..base()
    };

    #[test]
    fn json_output_reads_pointers() {
        let out = parse_output(
            &JSON,
            "warming up\n{\"response\":\"OK\",\"session_id\":\"abc\",\"stats\":{\"in\":10,\"out\":2},\"cost\":0.5}\n",
            true,
        )
        .unwrap();
        assert_eq!(out.text, "OK");
        assert_eq!(out.session_id.as_deref(), Some("abc"));
        assert_eq!(
            out.usage,
            Some(Usage {
                input_tokens: 10,
                output_tokens: 2,
                cost_usd: Some(0.5)
            })
        );
    }

    #[test]
    fn json_output_reads_pretty_printed_object() {
        let out = parse_output(&JSON, "{\n  \"response\": \"OK\"\n}\n", true).unwrap();
        assert_eq!(out.text, "OK");
        assert_eq!(out.usage, None);
    }

    #[test]
    fn json_error_field_is_reported() {
        let err = parse_output(
            &JSON,
            r#"{"response":"","error":{"message":"quota"}}"#,
            true,
        );
        assert!(matches!(err, Err(AgentError::Reported(m)) if m == "quota"));
        let ok = parse_output(&JSON, r#"{"response":"OK","error":null}"#, true);
        assert!(ok.is_ok());
    }

    const EVENTS: Spec = Spec {
        output: Output::Jsonl(Events {
            kind: "/type",
            text_kind: "text",
            text: "/part/text",
            concat: false,
            session: Some("/sessionID"),
            usage_kind: Some("step_finish"),
            input_tokens: Some("/part/tokens/input"),
            output_tokens: Some("/part/tokens/output"),
            cost: Some("/part/cost"),
            error_kind: Some("error"),
            error: Some("/error/message"),
        }),
        ..base()
    };

    #[test]
    fn event_stream_keeps_last_text_and_sums_usage() {
        let out = parse_output(
            &EVENTS,
            concat!(
                "{\"type\":\"text\",\"sessionID\":\"s\",\"part\":{\"text\":\"thinking\"}}\n",
                "{\"type\":\"step_finish\",\"part\":{\"tokens\":{\"input\":5,\"output\":1},\"cost\":0.25}}\n",
                "not json\n",
                "{\"type\":\"text\",\"part\":{\"text\":\"OK\"}}\n",
                "{\"type\":\"step_finish\",\"part\":{\"tokens\":{\"input\":7,\"output\":2},\"cost\":0.25}}\n",
            ),
            true,
        )
        .unwrap();
        assert_eq!(out.text, "OK");
        assert_eq!(out.session_id.as_deref(), Some("s"));
        assert_eq!(
            out.usage,
            Some(Usage {
                input_tokens: 12,
                output_tokens: 3,
                cost_usd: Some(0.5)
            })
        );
    }

    #[test]
    fn event_stream_error_followed_by_an_answer_is_a_retry() {
        let out = parse_output(
            &EVENTS,
            "{\"type\":\"error\",\"error\":{\"message\":\"overloaded\"}}\n{\"type\":\"text\",\"part\":{\"text\":\"OK\"}}",
            true,
        )
        .unwrap();
        assert_eq!(out.text, "OK");
    }

    #[test]
    fn shared_event_type_fails_only_with_an_error_field() {
        let spec = Spec {
            output: Output::Jsonl(Events {
                kind: "/type",
                text_kind: "message_end",
                text: "/message/content",
                concat: false,
                session: None,
                usage_kind: None,
                input_tokens: None,
                output_tokens: None,
                cost: None,
                error_kind: Some("message_end"),
                error: Some("/message/errorMessage"),
            }),
            ..base()
        };
        let ok = concat!(
            "{\"type\":\"message_end\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}\n",
            "{\"type\":\"message_end\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"O\"},{\"type\":\"text\",\"text\":\"K\"}]}}\n",
        );
        assert_eq!(parse_output(&spec, ok, true).unwrap().text, "OK");
        let bad = "{\"type\":\"message_end\",\"message\":{\"content\":[],\"errorMessage\":\"429 rate limit\"}}";
        assert!(
            matches!(parse_output(&spec, bad, true), Err(AgentError::Reported(m)) if m == "429 rate limit")
        );
    }

    #[test]
    fn event_stream_error_is_reported() {
        let err = parse_output(
            &EVENTS,
            "{\"type\":\"error\",\"error\":{\"message\":\"rate limit hit\"}}",
            true,
        );
        assert!(matches!(err, Err(AgentError::Reported(m)) if m == "rate limit hit"));
        assert!(matches!(
            parse_output(&EVENTS, "plain", true),
            Err(AgentError::Parse(_))
        ));
    }

    #[test]
    fn words_prompt_is_split_on_single_spaces() {
        let spec = Spec {
            prompt: Prompt::DashDashWords,
            ..base()
        };
        let args = build_args(&spec, &cfg(&[]), inv("fix  the x"));
        let tail: Vec<_> = args.iter().skip_while(|a| *a != "--").collect();
        assert_eq!(tail, ["--", "fix", "", "the", "x"]);
    }

    #[test]
    fn externally_tagged_events_are_read_by_their_key() {
        let spec = Spec {
            output: Output::Jsonl(Events {
                kind: "/event",
                text_kind: "AgentMessage",
                text: "/event/AgentMessage",
                concat: false,
                session: Some("/event/SessionStart/session_id"),
                usage_kind: None,
                input_tokens: None,
                output_tokens: None,
                cost: None,
                error_kind: Some("Error"),
                error: Some("/event/Error"),
            }),
            ..base()
        };
        let out = parse_output(
            &spec,
            "{\"event\":{\"SessionStart\":{\"session_id\":\"s1\"}}}\n{\"event\":\"Goodbye\"}\n{\"event\":{\"AgentMessage\":\"OK\"}}",
            true,
        )
        .unwrap();
        assert_eq!(
            (out.text.as_str(), out.session_id.as_deref()),
            ("OK", Some("s1"))
        );
    }

    #[test]
    fn claude_clone_error_uses_errors_array() {
        let spec = Spec {
            output: Output::ClaudeResult,
            ..base()
        };
        let out = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["Quota exhausted"]}"#;
        assert!(
            matches!(parse_output(&spec, out, false), Err(AgentError::Reported(m)) if m == "Quota exhausted")
        );
    }

    #[test]
    fn limit_messages_match() {
        assert!(limit_hit(&base(), "Error: Rate limit reached").is_some());
        assert!(limit_hit(&base(), "file not found").is_none());
    }
}
