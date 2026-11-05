//! Coding agent backends.
//!
//! Each backend is split into two pure halves, argument construction and output
//! parsing, plus the shared [`runner`] that owns the child process. Keeping the
//! halves pure makes them trivial to test against recorded fixtures.

pub mod claude;
pub mod codex;
pub mod runner;

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
}

impl AgentKind {
    /// Every supported agent, in display order.
    pub const ALL: [Self; 2] = [Self::Claude, Self::Codex];

    /// Stable lowercase identifier used in the API and database.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    /// Name shown to humans.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
        }
    }

    /// Executable name looked up on `PATH`.
    #[must_use]
    pub const fn binary_name(self) -> &'static str {
        self.as_str()
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when parsing an unknown agent name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown agent `{0}` (expected `claude` or `codex`)")]
pub struct UnknownAgent(pub String);

impl FromStr for AgentKind {
    type Err = UnknownAgent;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude-code" | "claude_code" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
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
            bin: locate(kind.binary_name()).unwrap_or_else(|| PathBuf::from(kind.binary_name())),
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
    let args: Vec<OsString> = match kind {
        AgentKind::Claude => claude::build_args(cfg, inv),
        AgentKind::Codex => codex::build_args(cfg, inv),
    };
    let raw = runner::run(&cfg.bin, &args, workdir, limits).await?;
    let parsed = match kind {
        AgentKind::Claude => claude::parse_output(&raw.stdout),
        AgentKind::Codex => codex::parse_output(&raw.stdout),
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

/// Finds an executable on `PATH` or in the usual per-user install locations.
#[must_use]
pub fn locate(name: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".claude/local"));
        dirs.push(home.join(".cargo/bin"));
        dirs.push(home.join(".npm-global/bin"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));

    dirs.into_iter()
        .map(|d| d.join(name))
        .find(|p| is_executable(p))
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
    fn agent_kind_rejects_unknown() {
        assert!("gemini".parse::<AgentKind>().is_err());
    }

    #[test]
    fn safe_arg_accepts_model_names() {
        assert!(is_safe_arg("claude-opus-5-5"));
        assert!(is_safe_arg("gpt-5.4"));
        assert!(is_safe_arg("7a9ad98b-c4ef-44fe-ad92-7fb361223d15"));
    }

    #[test]
    fn safe_arg_rejects_flag_injection() {
        assert!(!is_safe_arg("--dangerously-skip-permissions"));
        assert!(!is_safe_arg("-x"));
        assert!(!is_safe_arg("a b"));
        assert!(!is_safe_arg(""));
    }
}
