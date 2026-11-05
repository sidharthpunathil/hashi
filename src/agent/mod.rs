//! Coding agent backends.
//!
//! Each backend is split into pure halves, argument construction, extra
//! environment and output parsing, plus the shared [`runner`] that owns the
//! child process. Keeping them pure makes them easy to test against fixtures.
//!
//! Adding a backend: add a variant to [`AgentKind`] (and its entries in the
//! `impl` below), create `src/agent/<name>.rs` with `build_args`, `env` and
//! `parse_output`, and wire it into [`execute`].

pub mod claude;
pub mod codex;
pub mod cursor;
pub mod gemini;
pub mod opencode;
pub mod qwen;
pub mod runner;
pub mod tree;

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub use runner::Limits;

/// The coding agents Hashi can drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    /// Anthropic's Claude Code CLI (`claude`).
    Claude,
    /// OpenAI's Codex CLI (`codex`).
    Codex,
    /// Google's Gemini CLI (`gemini`).
    Gemini,
    /// opencode (`opencode`).
    Opencode,
    /// Alibaba's Qwen Code (`qwen`).
    Qwen,
    /// Cursor's CLI agent (`cursor-agent`, also shipped as `agent`).
    Cursor,
}

impl AgentKind {
    /// Every supported agent, in display order.
    pub const ALL: [Self; 6] = [
        Self::Claude,
        Self::Codex,
        Self::Gemini,
        Self::Opencode,
        Self::Qwen,
        Self::Cursor,
    ];

    /// Stable lowercase identifier used in the API and database.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Opencode => "opencode",
            Self::Qwen => "qwen",
            Self::Cursor => "cursor",
        }
    }

    /// Name shown to humans.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Gemini => "Gemini CLI",
            Self::Opencode => "opencode",
            Self::Qwen => "Qwen Code",
            Self::Cursor => "Cursor CLI",
        }
    }

    /// Executable names looked up on `PATH`, most specific first.
    #[must_use]
    pub const fn binary_names(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &["claude"],
            Self::Codex => &["codex"],
            Self::Gemini => &["gemini"],
            Self::Opencode => &["opencode"],
            Self::Qwen => &["qwen"],
            // `agent` is Cursor's newer name, but it is generic enough that the
            // specific legacy name is tried first.
            Self::Cursor => &["cursor-agent", "agent"],
        }
    }

    /// The primary executable name.
    #[must_use]
    pub const fn binary_name(self) -> &'static str {
        self.binary_names()[0]
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when parsing an unknown agent name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown agent `{0}` (expected claude, codex, gemini, opencode, qwen or cursor)")]
pub struct UnknownAgent(pub String);

impl FromStr for AgentKind {
    type Err = UnknownAgent;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude-code" | "claude_code" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            "gemini" | "gemini-cli" | "gemini_cli" => Ok(Self::Gemini),
            "opencode" | "open-code" => Ok(Self::Opencode),
            "qwen" | "qwen-code" | "qwen_code" => Ok(Self::Qwen),
            "cursor" | "cursor-agent" | "cursor_agent" | "cursor-cli" => Ok(Self::Cursor),
            other => Err(UnknownAgent(other.to_owned())),
        }
    }
}

/// Static configuration for one agent backend.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Path to the agent executable.
    pub bin: PathBuf,
    /// Maximum agentic turns per request (Claude only).
    pub max_turns: u32,
    /// Claude `--permission-mode`, when set.
    pub permission_mode: Option<String>,
    /// Codex sandbox policy (`read-only`, `workspace-write`, `danger-full-access`).
    pub sandbox: String,
    /// Extra arguments appended before the prompt.
    pub extra_args: Vec<String>,
}

impl AgentConfig {
    /// Sensible defaults for `kind`, with `bin` resolved from `PATH` when possible.
    #[must_use]
    pub fn default_for(kind: AgentKind) -> Self {
        Self {
            bin: kind
                .binary_names()
                .iter()
                .find_map(|name| locate(name))
                .unwrap_or_else(|| PathBuf::from(kind.binary_name())),
            max_turns: 10,
            permission_mode: None,
            sandbox: "read-only".to_owned(),
            extra_args: Vec::new(),
        }
    }
}

/// One prompt to run.
#[derive(Debug, Clone, Copy)]
pub struct Invocation<'a> {
    /// The user's prompt. Always passed after `--` so it is never parsed as a flag.
    pub prompt: &'a str,
    /// Agent session to resume, for stateful tokens.
    pub resume: Option<&'a str>,
    /// Model override.
    pub model: Option<&'a str>,
}

/// Token accounting reported by the agent, when available.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    /// Prompt tokens, including cached input.
    pub input_tokens: u64,
    /// Completion tokens.
    pub output_tokens: u64,
    /// Estimated cost in USD, if the agent reports one.
    pub cost_usd: Option<f64>,
}

/// The useful result of one agent run.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentOutput {
    /// Final assistant message.
    pub text: String,
    /// Session id that can be resumed later.
    pub session_id: Option<String>,
    /// Token accounting.
    pub usage: Option<Usage>,
}

/// Everything that can go wrong while running an agent.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// The executable could not be started.
    #[error("failed to start `{bin}`: {source}")]
    Spawn {
        /// Executable that failed.
        bin: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The run exceeded its time budget and was killed.
    #[error("agent timed out after {0:?}")]
    Timeout(Duration),
    /// The run produced more output than allowed and was killed.
    #[error("agent output exceeded {0} bytes")]
    OutputTooLarge(usize),
    /// The run's process tree used more memory than allowed and was killed.
    #[error(
        "run exceeded its memory limit ({} of {})",
        crate::util::human_bytes(*.used),
        crate::util::human_bytes(*.limit)
    )]
    MemoryLimit {
        /// Measured resident memory of the whole tree, in bytes.
        used: u64,
        /// Allowed bytes.
        limit: u64,
    },
    /// The agent exited unsuccessfully.
    #[error("agent exited with {status}: {detail}")]
    Exit {
        /// Exit status description.
        status: String,
        /// Trimmed stderr or stdout.
        detail: String,
    },
    /// The agent ran but reported a failure of its own.
    #[error("{0}")]
    Reported(String),
    /// The output could not be understood.
    #[error("could not parse agent output: {0}")]
    Parse(String),
    /// An I/O error while talking to the child.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

/// Runs one prompt against `kind` in `workdir`.
///
/// # Errors
///
/// Returns an [`AgentError`] if the process fails, times out, overflows its
/// output budget, or reports an error.
pub async fn execute(
    kind: AgentKind,
    cfg: &AgentConfig,
    inv: Invocation<'_>,
    workdir: &Path,
    limits: Limits,
) -> Result<AgentOutput, AgentError> {
    let (args, env): (Vec<OsString>, Vec<(String, String)>) = match kind {
        AgentKind::Claude => (claude::build_args(cfg, inv), claude::env(workdir)),
        AgentKind::Codex => (codex::build_args(cfg, inv), codex::env(workdir)),
        AgentKind::Gemini => (gemini::build_args(cfg, inv), gemini::env(workdir)),
        AgentKind::Opencode => (opencode::build_args(cfg, inv), opencode::env(workdir)),
        AgentKind::Qwen => (qwen::build_args(cfg, inv), qwen::env(workdir)),
        AgentKind::Cursor => (cursor::build_args(cfg, inv), cursor::env(workdir)),
    };
    let raw = runner::run(&cfg.bin, &args, &env, workdir, limits).await?;
    let parsed = match kind {
        AgentKind::Claude => claude::parse_output(&raw.stdout),
        AgentKind::Codex => codex::parse_output(&raw.stdout),
        AgentKind::Gemini => gemini::parse_output(&raw.stdout),
        AgentKind::Opencode => opencode::parse_output(&raw.stdout),
        AgentKind::Qwen => qwen::parse_output(&raw.stdout),
        AgentKind::Cursor => cursor::parse_output(&raw.stdout),
    };
    match parsed {
        Ok(out) => Ok(out),
        // A non-zero exit with unparseable output is best explained by stderr.
        // A failure the agent reported itself is already the clearest message.
        Err(err @ AgentError::Parse(_)) if !raw.success => Err(AgentError::Exit {
            status: raw.status,
            detail: runner::summarize(&raw.stderr, &raw.stdout).unwrap_or_else(|| err.to_string()),
        }),
        Err(err) => Err(err),
    }
}

/// Returns `true` if `s` is safe to pass as the value of a CLI option.
///
/// Rejects anything starting with `-` (which a parser could read as a flag)
/// and restricts the alphabet to what model names and session ids use.
#[must_use]
pub fn is_safe_arg(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/' | '@' | '[' | ']')
        })
}

/// Whether `args` (the owner's `extra_args`) set any of the options `names`,
/// either as a separate element or in `--name=value` form.
#[must_use]
pub fn has_flag(args: &[String], names: &[&str]) -> bool {
    args.iter().any(|a| {
        let name = a.split_once('=').map_or(a.as_str(), |(name, _)| name);
        names.contains(&name)
    })
}

/// Keeps a prompt from being read as a slash command.
///
/// Gemini CLI and Claude Code run a prompt that starts with `/` as one of
/// their own commands (built in, or a custom command or skill from the
/// owner's config), which a shared user must not be able to trigger. Gemini
/// only checks `startsWith('/')` without trimming, so a leading space makes
/// it an ordinary prompt; the same space is used for Claude Code.
#[must_use]
pub fn neutralize_slash_command(prompt: &str) -> String {
    if prompt.starts_with('/') {
        format!(" {prompt}")
    } else {
        prompt.to_owned()
    }
}

/// Finds an executable on `PATH` or in the usual per-user install locations.
///
/// On Windows each directory is tried with `.exe`, `.cmd` and `.bat`, because
/// npm installs CLIs such as Codex as `.cmd` shims.
#[must_use]
pub fn locate(name: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local").join("bin"));
        dirs.push(home.join(".claude").join("local"));
        dirs.push(home.join(".cargo").join("bin"));
        dirs.push(home.join(".npm-global").join("bin"));
    }
    if cfg!(windows) {
        for var in ["APPDATA", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(var).map(PathBuf::from) {
                dirs.push(base.join("npm"));
                dirs.push(base.join("Programs").join("hashi").join("bin"));
            }
        }
    } else {
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
        dirs.push(PathBuf::from("/usr/local/bin"));
    }

    let names = candidate_names(name, cfg!(windows));
    dirs.iter()
        .flat_map(|d| names.iter().map(move |n| d.join(n)))
        .find(|p| is_executable(p))
}

/// File names to try for `name`, most preferred first.
#[must_use]
pub fn candidate_names(name: &str, windows: bool) -> Vec<String> {
    if windows && Path::new(name).extension().is_none() {
        [".exe", ".cmd", ".bat", ""]
            .iter()
            .map(|ext| format!("{name}{ext}"))
            .collect()
    } else {
        vec![name.to_owned()]
    }
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Asks an agent binary for its version string, with a short timeout.
pub async fn probe_version(bin: &Path) -> Option<String> {
    let fut = tokio::process::Command::new(bin)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(Duration::from_secs(10), fut)
        .await
        .ok()?
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_kind_parses_aliases() {
        assert_eq!("Claude-Code".parse::<AgentKind>(), Ok(AgentKind::Claude));
        assert_eq!(" codex ".parse::<AgentKind>(), Ok(AgentKind::Codex));
    }

    #[test]
    fn every_agent_round_trips_through_its_name() {
        for kind in AgentKind::ALL {
            assert_eq!(kind.as_str().parse::<AgentKind>(), Ok(kind));
            assert!(!kind.binary_names().is_empty());
        }
    }

    #[test]
    fn cursor_prefers_its_specific_binary_name() {
        assert_eq!(AgentKind::Cursor.binary_names(), ["cursor-agent", "agent"]);
        assert_eq!("cursor-agent".parse::<AgentKind>(), Ok(AgentKind::Cursor));
    }

    #[test]
    fn agent_kind_rejects_unknown() {
        assert!("copilot".parse::<AgentKind>().is_err());
    }

    #[test]
    fn safe_arg_accepts_model_names() {
        assert!(is_safe_arg("claude-opus-5-5"));
        assert!(is_safe_arg("gpt-5.4"));
        assert!(is_safe_arg("7a9ad98b-c4ef-44fe-ad92-7fb361223d15"));
    }

    #[test]
    fn candidate_names_try_windows_extensions_in_order() {
        assert_eq!(
            candidate_names("codex", true),
            ["codex.exe", "codex.cmd", "codex.bat", "codex"]
        );
        assert_eq!(candidate_names("codex", false), ["codex"]);
        assert_eq!(candidate_names("clip.exe", true), ["clip.exe"]);
    }

    #[test]
    fn safe_arg_rejects_flag_injection() {
        assert!(!is_safe_arg("--dangerously-skip-permissions"));
        assert!(!is_safe_arg("-x"));
        assert!(!is_safe_arg("a b"));
        assert!(!is_safe_arg(""));
    }
}
