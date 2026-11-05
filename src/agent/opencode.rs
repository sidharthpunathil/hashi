//! opencode backend. Being implemented; see `docs/research/agent-clis.md`.

use std::ffi::OsString;
use std::path::Path;

use super::{AgentConfig, AgentError, AgentOutput, Invocation};

/// Extra environment for the child.
#[must_use]
pub fn env(_workdir: &Path) -> Vec<(String, String)> {
    Vec::new()
}

/// Builds the argument vector for a headless run.
#[must_use]
pub fn build_args(_cfg: &AgentConfig, inv: Invocation<'_>) -> Vec<OsString> {
    vec!["--".into(), inv.prompt.into()]
}

/// Parses the CLI's output.
///
/// # Errors
///
/// Always fails until this backend is implemented.
pub fn parse_output(_stdout: &str) -> Result<AgentOutput, AgentError> {
    Err(AgentError::Parse(
        "opencode support is not finished yet".to_owned(),
    ))
}
