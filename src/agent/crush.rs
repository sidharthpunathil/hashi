//! Crush backend (Charmbracelet): `crush run`, which prints the answer as
//! plain text on stdout. Opt in with `--enable-crush`.
//!
//! Checked against the Crush source on `main` as of 2026-11-05, the day
//! v0.100.0 was tagged. **Verified versions: 0.100.x only** (see
//! [`check_version`]). Paths below are under `internal/` in that tree.
//!
//! Behaviour this module relies on:
//!
//! - `cmd/run.go`: positional arguments are joined with a space, and a
//!   pipe or regular file on stdin is read and prepended to the prompt. The
//!   runner gives every child a null stdin, which is neither, and the
//!   prompt goes after `--` as one argument. Exit codes come from
//!   `cmd/root.go`: 0, 1 on any error, 130 on an interrupt. Failures print
//!   `agent processing failed: <error>` (`app/app.go:502`) to stderr.
//! - `app/app.go:447-449`: a non-interactive run auto-approves every
//!   permission request of its session. `-y` and `--yolo` are therefore
//!   pointless as well as dangerous, and are never passed; the only control
//!   left is which tools exist. [`workspace_config`] removes every tool that
//!   can write, run code, reach the network or start a sub-agent through
//!   `options.disabled_tools`. Reads outside the working directory also ask
//!   for permission, which is auto-approved, so Crush can read any file the
//!   owner's user can read (see docs/security-model.md).
//! - `config/load.go` `Load`: the workspace config at
//!   `<data-dir>/crush.json` is merged after every other file, so it wins.
//!   Hashi passes `--data-dir` and rewrites that file before every run.
//! - `config/load.go` `lookupConfigs`: `.crushrc`, `crushrc`, `.crush.json`
//!   and `crush.json` are searched upward from the working directory to the
//!   git root, and a crushrc is executed as shell code.
//!   [`crush_config_present`] refuses the run when any of them exists in
//!   the working directory or any parent up to the filesystem root (past
//!   the git root, so the check holds whatever Crush's walk does), as does
//!   a system config at `/etc/crush/crush.json`, which could add MCP
//!   servers or hooks that Hashi cannot turn off. [`execute`] checks before
//!   the version probe and again under the data directory's lock right
//!   before each spawn that could load a config.
//! - `CRUSH_GLOBAL_CONFIG`, `CRUSH_GLOBAL_DATA`, `CRUSH_CACHE_DIR` and
//!   `CRUSH_SKILLS_DIR` (`config/load.go`) move every user-level file, so
//!   each account, including `default`, keeps its config, logins and skills
//!   in a Hashi-owned directory and the owner's `~/.config/crush` (with its
//!   MCP servers and hooks) never loads. `HOME` points there too, because
//!   the Copilot token import reads `$HOME/.config/github-copilot`
//!   (`oauth/copilot/disk.go`).
//! - `config/load.go` `PushPopCrushEnv`: while providers are configured,
//!   any `CRUSH_<VAR>` overrides `<VAR>`, so every inherited `CRUSH_*`
//!   variable is removed.
//! - `main.go` imports `godotenv/autoload`, which loads a `.env` file in the
//!   working directory into the process (without overriding variables that
//!   are already set). That could bring back `CRUSH_*` or provider
//!   variables, so [`check_config`] refuses a workspace with a `.env`.
//! - `cmd/session.go:413-430`: `crush session last --json` prints the most
//!   recently updated session's `meta` with `id` (a hash), `uuid`, `cost`,
//!   `prompt_tokens` and `completion_tokens`. `run` prints no session id, so
//!   this runs right after, under a lock per data directory (see
//!   [`execute`]). `agent/agent.go` `updateSessionTokenCounters` stores the
//!   last step's context size and output, not a running total; `cost` is a
//!   running total, so a resumed run reports the difference.
//! - `cmd/run.go`: `-s <id>` resumes a session by UUID or hash prefix.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{AgentConfig, AgentError, AgentOutput, EnvOverride, Invocation, Usage, runner};

/// The oldest Crush release whose tool list and flags Hashi has checked.
pub const MIN_VERSION: (u64, u64, u64) = (0, 100, 0);

/// The newest Crush release (major and minor) whose tool list Hashi has
/// checked. Patch releases of it are accepted; anything newer, and any
/// pre-release or build (a version with a `-` or `+` suffix), is refused
/// unless the owner passes `--crush-allow-newer`.
pub const MAX_VERIFIED: (u64, u64) = (0, 100);

/// Every built-in tool of the verified releases, from `allToolNames` in
/// `config/config.go:1060-1096`. Each is in exactly one of
/// [`ALLOWED_TOOLS`] and [`DENIED_TOOLS`].
pub const KNOWN_TOOLS: [&str; 33] = [
    "agent",
    "agent_dispatch",
    "agent_send",
    "agent_status",
    "agent_stop",
    "bash",
    "crush_info",
    "crush_logs",
    "job_output",
    "job_kill",
    "download",
    "edit",
    "multiedit",
    "lsp_diagnostics",
    "lsp_references",
    "lsp_restart",
    "lsp_symbols",
    "lsp_definition",
    "lsp_call_hierarchy",
    "lsp_rename",
    "lsp_replace_symbol",
    "fetch",
    "agentic_fetch",
    "glob",
    "grep",
    "ls",
    "question",
    "sourcegraph",
    "todos",
    "view",
    "write",
    "list_mcp_resources",
    "read_mcp_resource",
];

/// The tools a Hashi run keeps: reading files and listing, searching, the
/// read-only language-server queries and the model's own to-do list.
pub const ALLOWED_TOOLS: [&str; 10] = [
    "view",
    "ls",
    "glob",
    "grep",
    "lsp_diagnostics",
    "lsp_references",
    "lsp_symbols",
    "lsp_definition",
    "lsp_call_hierarchy",
    "todos",
];

/// The tools a Hashi run removes: shell and background jobs, file writes,
/// renames through the language server, network access, sub-agents,
/// Crush's own introspection, MCP resources and questions to a user who is
/// not there.
pub const DENIED_TOOLS: [&str; 23] = [
    "bash",
    "job_output",
    "job_kill",
    "download",
    "edit",
    "multiedit",
    "write",
    "lsp_rename",
    "lsp_replace_symbol",
    "lsp_restart",
    "fetch",
    "agentic_fetch",
    "sourcegraph",
    "agent",
    "agent_dispatch",
    "agent_send",
    "agent_status",
    "agent_stop",
    "crush_info",
    "crush_logs",
    "list_mcp_resources",
    "read_mcp_resource",
    "question",
];

/// Project config files Crush loads from the working directory and its
/// parents. A crushrc is run as shell code.
pub const PROJECT_CONFIG_NAMES: [&str; 4] = [".crushrc", "crushrc", ".crush.json", "crush.json"];

/// The system-wide config Crush loads on Unix (`config/config_unix.go`).
pub const SYSTEM_CONFIG: &str = "/etc/crush/crush.json";

/// Inherited variables removed from every Crush run, on any account. A
/// trailing `*` matches a prefix (see [`runner::env_name_matches`]).
pub const ALWAYS_REMOVE: [&str; 5] = [
    "CRUSH_*",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
];

/// Provider variables from the Crush README (`README.md:208-236`), removed
/// on non-default accounts so the owner's keys cannot outrank the account's
/// own login. The `CRUSH_` forms are covered by [`ALWAYS_REMOVE`].
pub const PROVIDER_VARS: [&str; 30] = [
    "HYPER_API_KEY",
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "VERCEL_API_KEY",
    "GEMINI_API_KEY",
    "ZAI_API_KEY",
    "MINIMAX_API_KEY",
    "SYNTHETIC_API_KEY",
    "HF_TOKEN",
    "CEREBRAS_API_KEY",
    "OPENROUTER_API_KEY",
    "IONET_API_KEY",
    "ALIBABA_SINGAPORE_API_KEY",
    "ALIBABA_US_API_KEY",
    "GROQ_API_KEY",
    "AVIAN_API_KEY",
    "OPENCODE_API_KEY",
    "VERTEXAI_PROJECT",
    "VERTEXAI_LOCATION",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_REGION",
    "AWS_PROFILE",
    "AWS_BEARER_TOKEN_BEDROCK",
    "AZURE_OPENAI_API_ENDPOINT",
    "AZURE_OPENAI_API_KEY",
    "AZURE_OPENAI_API_VERSION",
    "MOONSHOT_API_KEY",
    "XAI_API_KEY",
    "AWS_SESSION_TOKEN",
];

/// Flags that must never reach Crush from extra arguments: auto-approval
/// (`-y`, `--yolo`, which `-yy` widens to drop the command block list),
/// and anything that moves the working directory, data directory or server.
const UNSAFE_FLAGS: [&str; 9] = [
    "-y",
    "-yy",
    "--yolo",
    "-c",
    "--cwd",
    "-D",
    "--data-dir",
    "-H",
    "--host",
];

/// How long the version probe and the session lookup may take.
const SIDE_TIMEOUT: Duration = Duration::from_secs(30);

/// Environment for every Crush child, before the account's own.
///
/// - `CRUSH_DISABLE_METRICS` and `DO_NOT_TRACK` (`cmd/root.go:948-951`):
///   no telemetry.
/// - `CRUSH_DISABLE_PROVIDER_AUTO_UPDATE` (`config/load.go:787`): use the
///   bundled provider list instead of fetching one.
#[must_use]
pub fn env() -> Vec<(String, String)> {
    [
        ("CRUSH_DISABLE_METRICS", "1"),
        ("DO_NOT_TRACK", "1"),
        ("CRUSH_DISABLE_PROVIDER_AUTO_UPDATE", "1"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect()
}

/// The variables that point Crush at the account directory `dir`. `HOME`
/// is always one of them, and is how [`execute`] finds the directory again.
#[must_use]
pub fn account_env(dir: &Path) -> Vec<(String, String)> {
    let sub = |name: &str| dir.join(name).to_string_lossy().into_owned();
    let mut env = vec![
        ("HOME".to_owned(), dir.to_string_lossy().into_owned()),
        ("CRUSH_GLOBAL_CONFIG".to_owned(), sub("config")),
        ("CRUSH_GLOBAL_DATA".to_owned(), sub("global-data")),
        ("CRUSH_CACHE_DIR".to_owned(), sub("cache")),
        ("CRUSH_SKILLS_DIR".to_owned(), sub("skills")),
    ];
    if cfg!(windows) {
        // The Copilot import reads %LOCALAPPDATA%\github-copilot on Windows.
        env.push(("LOCALAPPDATA".to_owned(), sub("local")));
    }
    env
}

/// The subdirectories [`account_env`] names, created before every run.
const ACCOUNT_SUBDIRS: [&str; 6] = ["config", "global-data", "cache", "skills", "local", "data"];

/// The account directory a run's environment points at: the `HOME` that
/// [`account_env`] sets.
#[must_use]
pub fn account_dir_of(account: &EnvOverride) -> Option<PathBuf> {
    account
        .set
        .iter()
        .rev()
        .find(|(k, _)| k == "HOME")
        .map(|(_, v)| PathBuf::from(v))
        .filter(|p| p.is_absolute())
}

/// The data directory for `workdir` under account `dir`: one per working
/// directory, named by the SHA-256 of its absolute path, so each token's
/// sessions stay apart.
#[must_use]
pub fn data_dir(dir: &Path, workdir: &Path) -> PathBuf {
    let abs = std::path::absolute(workdir).unwrap_or_else(|_| workdir.to_path_buf());
    let digest = Sha256::digest(abs.to_string_lossy().as_bytes());
    dir.join("data").join(hex::encode(digest))
}

/// The workspace config Hashi writes to `<data-dir>/crush.json`.
#[must_use]
pub fn workspace_config() -> Value {
    json!({
        "options": {
            "disabled_tools": DENIED_TOOLS,
            "auto_lsp": false,
            "disable_metrics": true,
            "disable_provider_auto_update": true,
            "notifications": "disabled",
        }
    })
}

/// Builds the argument vector for a one-shot `crush run`.
///
/// `-q` hides the spinner. `--data-dir` keeps the sessions and the
/// workspace config in the account. The prompt is one argument after `--`.
#[must_use]
pub fn build_args(cfg: &AgentConfig, inv: Invocation<'_>, data: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "--data-dir".into(),
        data.as_os_str().to_owned(),
        "run".into(),
        "-q".into(),
    ];
    if let Some(model) = inv.model {
        args.extend(["-m".into(), model.into()]);
    }
    if let Some(session) = inv.resume {
        args.extend(["-s".into(), session.into()]);
    }
    args.extend(
        cfg.extra_args
            .iter()
            .filter(|a| !is_unsafe_flag(a))
            .map(OsString::from),
    );
    args.push("--".into());
    args.push(inv.prompt.into());
    args
}

fn is_unsafe_flag(arg: &str) -> bool {
    let name = arg.split_once('=').map_or(arg, |(name, _)| name);
    // `-y` repeated, or bundled with other short flags, still counts.
    UNSAFE_FLAGS.contains(&name)
        || (name.starts_with('-') && !name.starts_with("--") && name.contains('y'))
}

/// The arguments of `crush session <cmd> --json` against `data`.
fn session_args(data: &Path, cmd: &[&str]) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["--data-dir".into(), data.as_os_str().to_owned()];
    args.push("session".into());
    args.extend(cmd.iter().map(OsString::from));
    args.push("--json".into());
    args
}

/// The first project config file found in `workdir` or any parent, all the
/// way to the filesystem root. Crush itself stops at the git root
/// (`config/load.go` `lookupConfigs`), but where that is depends on Crush's
/// version and on `.git` files Hashi does not control, so the check walks
/// further and is safe whatever Crush does. A symlink by one of the names
/// counts, even a dangling one.
#[must_use]
pub fn crush_config_present(workdir: &Path) -> Option<PathBuf> {
    let start = std::path::absolute(workdir).unwrap_or_else(|_| workdir.to_path_buf());
    start.ancestors().find_map(|d| {
        PROJECT_CONFIG_NAMES
            .iter()
            .map(|name| d.join(name))
            .find(|path| std::fs::symlink_metadata(path).is_ok())
    })
}

/// Refuses a run whose config Hashi does not control: a project config in
/// `workdir` or a parent, a `.env` in `workdir`, or a system config at
/// `system`.
///
/// # Errors
///
/// Returns the reason, naming the file.
pub fn check_config(workdir: &Path, system: Option<&Path>) -> Result<(), String> {
    if let Some(found) = crush_config_present(workdir) {
        return Err(format!(
            "refusing to run Crush: {} is a Crush project config, which Crush would load \
             (a crushrc runs as shell code); remove it",
            found.display()
        ));
    }
    let dotenv = std::path::absolute(workdir)
        .unwrap_or_else(|_| workdir.to_path_buf())
        .join(".env");
    if std::fs::symlink_metadata(&dotenv).is_ok() {
        return Err(format!(
            "refusing to run Crush: {} would be loaded into Crush's environment; remove it",
            dotenv.display()
        ));
    }
    if let Some(sys) = system.filter(|p| std::fs::symlink_metadata(p).is_ok()) {
        return Err(format!(
            "refusing to run Crush: {} is a system-wide Crush config, which can add MCP \
             servers and hooks that Hashi cannot turn off; remove it",
            sys.display()
        ));
    }
    Ok(())
}

/// Reads `x.y.z` out of `crush --version` output such as
/// `crush version v0.100.0`: the first word that starts with a digit
/// (after an optional `v`). A release with a pre-release or build suffix
/// (`v0.100.1-0.20261105...`, `v0.100.0-devel+dirty`) is not a release
/// whose tools Hashi checked, so it gives `None`, which
/// [`check_version`] refuses unless `--crush-allow-newer` is set.
#[must_use]
pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let word = text
        .split_whitespace()
        .map(|w| w.trim_start_matches('v').trim_end_matches(','))
        .find(|w| w.starts_with(|c: char| c.is_ascii_digit()))?;
    let mut parts = word.split('.');
    let mut num = || -> Option<u64> {
        let p = parts.next()?;
        (!p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
            .then(|| p.parse().ok())
            .flatten()
    };
    let v = (num()?, num()?, num()?);
    parts.next().is_none().then_some(v)
}

/// The tool gate. Crush has no command that lists its tools, so Hashi
/// trusts only the releases whose tool list it has checked
/// ([`KNOWN_TOOLS`]): a newer release could add a tool that is neither
/// allowed nor denied, and it would run without approval.
///
/// # Errors
///
/// Returns the reason the version is refused.
pub fn check_version(output: Option<&str>, allow_newer: bool) -> Result<(), String> {
    let max = format!("{}.{}.x", MAX_VERIFIED.0, MAX_VERIFIED.1);
    let Some(text) = output.map(str::trim).filter(|t| !t.is_empty()) else {
        return Err("refusing to run Crush: `crush --version` printed nothing".to_owned());
    };
    let Some(v) = parse_version(text) else {
        if allow_newer {
            return Ok(());
        }
        return Err(format!(
            "refusing to run Crush: cannot read a version from `{}`; Hashi has checked the \
             tools of Crush {max} only. Pass --crush-allow-newer to run it anyway",
            crate::util::truncate(text, 60)
        ));
    };
    let shown = format!("{}.{}.{}", v.0, v.1, v.2);
    if v < MIN_VERSION {
        return Err(format!(
            "refusing to run Crush {shown}: Hashi has checked Crush {max} only; update Crush"
        ));
    }
    if (v.0, v.1) > MAX_VERIFIED && !allow_newer {
        return Err(format!(
            "refusing to run Crush {shown}: Hashi has checked the tools of Crush {max} only, \
             and a newer release may add a tool that Hashi neither allows nor denies, which \
             Crush would run without asking. Pass --crush-allow-newer \
             (HASHI_CRUSH_ALLOW_NEWER=1) to run it anyway"
        ));
    }
    Ok(())
}

/// What `crush session ... --json` says about one session.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionInfo {
    /// The session's UUID, or its hash id when the UUID is missing.
    pub id: String,
    /// Running cost in USD.
    pub cost: Option<f64>,
    /// The last step's context size.
    pub prompt_tokens: u64,
    /// The last step's output.
    pub completion_tokens: u64,
}

/// Parses the JSON printed by `crush session last --json` or `show --json`.
#[must_use]
pub fn parse_session(stdout: &str) -> Option<SessionInfo> {
    let v: Value = serde_json::from_str(stdout.trim()).ok()?;
    let meta = v.get("meta").unwrap_or(&v);
    let id = ["uuid", "id"]
        .iter()
        .find_map(|k| meta.get(*k).and_then(Value::as_str))
        .filter(|s| !s.is_empty())?
        .to_owned();
    let n = |k: &str| meta.get(k).and_then(Value::as_u64).unwrap_or(0);
    Some(SessionInfo {
        id,
        cost: meta
            .get("cost")
            .and_then(Value::as_f64)
            .filter(|c| c.is_finite() && *c >= 0.0),
        prompt_tokens: n("prompt_tokens"),
        completion_tokens: n("completion_tokens"),
    })
}

/// Usage for a run from the session after it, and before it when resumed:
/// tokens as Crush records them, cost as the difference.
#[must_use]
pub fn usage(after: &SessionInfo, before: Option<&SessionInfo>) -> Usage {
    let cost = match (after.cost, before.and_then(|b| b.cost)) {
        (Some(a), Some(b)) => Some((a - b).max(0.0)),
        (a, _) => a,
    };
    Usage {
        input_tokens: after.prompt_tokens,
        output_tokens: after.completion_tokens,
        cost_usd: cost,
    }
}

/// The answer: everything Crush printed on stdout.
///
/// # Errors
///
/// Returns [`AgentError::Parse`] when stdout is empty.
pub fn parse_output(stdout: &str) -> Result<String, AgentError> {
    let text = stdout.trim();
    if text.is_empty() {
        return Err(AgentError::Parse("Crush printed no answer".to_owned()));
    }
    Ok(text.to_owned())
}

/// One lock per data directory, so `session last` after a run finds that
/// run's session and not a neighbour's. In process only: two Hashi
/// processes running Crush in the same directory are not serialised.
fn lock_for(data: &Path) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let mut map = LOCKS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    // Drop locks nobody holds, so the map does not grow without bound.
    map.retain(|_, l| Arc::strong_count(l) > 1);
    Arc::clone(map.entry(data.to_path_buf()).or_default())
}

/// Creates `dir` owner-only.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// Writes [`workspace_config`] into `data`, replacing whatever is there
/// (Crush itself writes model choices into the same file).
///
/// # Errors
///
/// Fails if the directory or file cannot be written.
pub fn write_workspace_config(data: &Path) -> std::io::Result<()> {
    private_dir(data)?;
    let path = data.join("crush.json");
    if std::fs::symlink_metadata(&path).is_ok_and(|m| !m.is_file()) {
        return Err(std::io::Error::other(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    let text = serde_json::to_string_pretty(&workspace_config()).map_err(std::io::Error::other)?;
    crate::diagnostics::write_private(&path, &text)
}

/// The system config to check on this platform.
fn system_config() -> Option<&'static Path> {
    cfg!(unix).then(|| Path::new(SYSTEM_CONFIG))
}

/// Runs one prompt on Crush in `workdir`, on the account `account` points
/// at (always set: the default account has a Hashi-owned directory too).
///
/// Before the run: the project and system config check, the version gate,
/// and a fresh workspace config. The run and the `session last` lookup
/// after it hold the data directory's lock.
///
/// # Errors
///
/// [`AgentError::Reported`] for a refused run, otherwise as for
/// [`super::execute`].
pub async fn execute(
    cfg: &AgentConfig,
    inv: Invocation<'_>,
    workdir: &Path,
    limits: super::Limits,
    account: Option<&EnvOverride>,
) -> Result<AgentOutput, AgentError> {
    let Some(account) = account else {
        return Err(AgentError::Reported(
            "Crush runs only on a Hashi-owned account directory".to_owned(),
        ));
    };
    let Some(dir) = account_dir_of(account) else {
        return Err(AgentError::Reported(
            "the Crush account has no directory".to_owned(),
        ));
    };
    let workdir = std::path::absolute(workdir).unwrap_or_else(|_| workdir.to_path_buf());
    check_config(&workdir, system_config()).map_err(AgentError::Reported)?;

    let mut env = env();
    env.extend(account.set.iter().cloned());
    let remove: &[&str] = &account.remove;
    let side = super::Limits {
        timeout: SIDE_TIMEOUT.min(limits.timeout),
        ..limits.clone()
    };

    let version = runner::run_with(
        &cfg.bin,
        &["--version".into()],
        &env,
        remove,
        &workdir,
        side.clone(),
    )
    .await?;
    let shown = version.success.then_some(version.stdout.as_str());
    check_version(shown, cfg.allow_newer).map_err(AgentError::Reported)?;

    for sub in ACCOUNT_SUBDIRS {
        private_dir(&dir.join(sub))?;
    }
    let data = data_dir(&dir, &workdir);
    write_workspace_config(&data)?;

    let lock = lock_for(&data);
    let _held = lock.lock().await;
    // Again under the lock: a config planted while the version probe ran, or
    // while this run waited for the lock, would load in the next spawn.
    check_config(&workdir, system_config()).map_err(AgentError::Reported)?;

    let before = match inv.resume {
        Some(id) => {
            let raw = runner::run_with(
                &cfg.bin,
                &session_args(&data, &["show", id]),
                &env,
                remove,
                &workdir,
                side.clone(),
            )
            .await?;
            raw.success.then(|| parse_session(&raw.stdout)).flatten()
        }
        None => None,
    };

    // And once more right before the run itself, after the `session show`.
    check_config(&workdir, system_config()).map_err(AgentError::Reported)?;
    let args = build_args(cfg, inv, &data);
    let raw = runner::run_with(&cfg.bin, &args, &env, remove, &workdir, limits).await?;
    if !raw.success {
        return Err(AgentError::Exit {
            status: raw.status,
            detail: runner::summarize(&raw.stderr, &raw.stdout)
                .unwrap_or_else(|| "Crush failed without a message".to_owned()),
        });
    }
    let text = parse_output(&raw.stdout)?;

    // A project config planted during the run would load in the lookup.
    let after = if check_config(&workdir, system_config()).is_ok() {
        match runner::run_with(
            &cfg.bin,
            &session_args(&data, &["last"]),
            &env,
            remove,
            &workdir,
            side,
        )
        .await
        {
            Ok(raw) if raw.success => parse_session(&raw.stdout),
            Ok(raw) => {
                tracing::warn!(status = %raw.status, "crush session last failed");
                None
            }
            Err(e) => {
                tracing::warn!(error = %e, "crush session last failed");
                None
            }
        }
    } else {
        None
    };
    Ok(AgentOutput {
        text,
        usage: after.as_ref().map(|a| usage(a, before.as_ref())),
        session_id: after.map(|a| a.id),
        rate_limits: Vec::new(),
    })
}

/// Limit messages: Crush retries provider errors itself and, when it gives
/// up, reports the provider's `<Title>: <Message>`, so only the generic
/// wording of each provider is left to match. Crush reports no reset time,
/// but a provider message may name one.
const LIMIT_MESSAGES: [&str; 9] = [
    "rate limit",
    "rate-limit",
    "rate_limit",
    "ratelimit",
    "too many requests",
    "quota",
    "usage limit",
    "resource_exhausted",
    "resource exhausted",
];

/// Whether a failed run's error says the account is at its limit. See
/// [`super::limit_hit`].
#[must_use]
pub fn limit_hit(error: &str) -> Option<Option<time::OffsetDateTime>> {
    (super::limit::contains_any(error, &LIMIT_MESSAGES) || super::limit::has_status(error, "429"))
        .then(|| super::limit::hit(error))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentKind;

    fn cfg() -> AgentConfig {
        AgentConfig {
            bin: "crush".into(),
            ..AgentConfig::default_for(AgentKind::Crush)
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

    #[test]
    fn tool_lists_partition_the_known_tools() {
        for tool in KNOWN_TOOLS {
            let allowed = ALLOWED_TOOLS.contains(&tool);
            let denied = DENIED_TOOLS.contains(&tool);
            assert!(allowed ^ denied, "{tool} must be in exactly one list");
        }
        for tool in ALLOWED_TOOLS.iter().chain(&DENIED_TOOLS) {
            assert!(KNOWN_TOOLS.contains(tool), "{tool} is not a known tool");
        }
        for tool in ["bash", "edit", "write", "fetch", "agent", "download"] {
            assert!(DENIED_TOOLS.contains(&tool));
        }
    }

    #[test]
    fn build_args_for_new_and_resumed_runs() {
        let data = Path::new("/acct/data/abc");
        let args = strings(&build_args(&cfg(), inv("fix it"), data));
        assert_eq!(
            args,
            ["--data-dir", "/acct/data/abc", "run", "-q", "--", "fix it"]
        );
        let args = strings(&build_args(
            &cfg(),
            Invocation {
                prompt: "-s evil --yolo",
                resume: Some("7a9ad98b-c4ef"),
                model: Some("anthropic/claude-sonnet-4-5"),
            },
            data,
        ));
        assert_eq!(
            &args[4..],
            [
                "-m",
                "anthropic/claude-sonnet-4-5",
                "-s",
                "7a9ad98b-c4ef",
                "--",
                "-s evil --yolo"
            ]
        );
    }

    #[test]
    fn auto_approval_never_comes_from_extra_args() {
        let mut c = cfg();
        c.extra_args = [
            "-y",
            "-yy",
            "--yolo",
            "--yolo=2",
            "-qy",
            "--cwd=/",
            "-D",
            "--small-model",
        ]
        .map(String::from)
        .to_vec();
        let args = strings(&build_args(&c, inv("hi"), Path::new("/d")));
        assert_eq!(
            args,
            ["--data-dir", "/d", "run", "-q", "--small-model", "--", "hi"]
        );
    }

    #[test]
    fn workspace_config_denies_every_unsafe_tool() {
        let v = workspace_config();
        let denied: Vec<&str> = v["options"]["disabled_tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap())
            .collect();
        assert_eq!(denied, DENIED_TOOLS);
        assert_eq!(v["options"]["auto_lsp"], false);
        assert_eq!(v["options"]["disable_metrics"], true);
    }

    #[test]
    fn versions_are_gated() {
        assert_eq!(parse_version("crush version v0.100.0"), Some((0, 100, 0)));
        assert_eq!(parse_version("crush version 0.100.3,"), Some((0, 100, 3)));
        assert_eq!(parse_version("crush version devel"), None);
        assert!(check_version(Some("crush version v0.100.0"), false).is_ok());
        assert!(check_version(Some("crush version v0.100.7"), false).is_ok());
        let err = check_version(Some("crush version v0.101.0"), false).unwrap_err();
        assert!(err.contains("--crush-allow-newer"), "{err}");
        assert!(check_version(Some("crush version v0.101.0"), true).is_ok());
        assert!(check_version(Some("crush version v1.0.0"), false).is_err());
        assert!(check_version(Some("crush version v0.99.0"), true).is_err());
        assert!(check_version(Some("crush version devel"), false).is_err());
        assert!(check_version(Some("crush version devel"), true).is_ok());
        assert!(check_version(None, true).is_err());
    }

    #[test]
    fn suffixed_versions_are_refused_unless_newer_is_allowed() {
        for v in [
            "crush version v0.100.1-0.20261105120000-abcdef123456",
            "crush version v0.100.0-devel+dirty",
            "v0.100.2-nightly+abc",
            "crush version v0.100.0+build.7",
            "crush version v0.100.0.1",
            "crush version v0.100.x",
        ] {
            assert_eq!(parse_version(v), None, "{v}");
            let err = check_version(Some(v), false).unwrap_err();
            assert!(err.contains("--crush-allow-newer"), "{err}");
            assert!(check_version(Some(v), true).is_ok(), "{v}");
        }
    }

    #[test]
    fn project_config_is_found_in_any_parent() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        let work = repo.join("a").join("b");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir(repo.join(".git")).unwrap();
        assert_eq!(crush_config_present(&work), None);
        // Above the git root counts too, whatever Crush's own walk does.
        let above = root.path().join("crush.json");
        std::fs::write(&above, "{}").unwrap();
        assert_eq!(crush_config_present(&work), Some(above.clone()));
        std::fs::remove_file(&above).unwrap();
        #[cfg(unix)]
        {
            let link = root.path().join(".crushrc");
            std::os::unix::fs::symlink(root.path().join("missing"), &link).unwrap();
            assert_eq!(crush_config_present(&work), Some(link.clone()));
            std::fs::remove_file(&link).unwrap();
        }
        for name in PROJECT_CONFIG_NAMES {
            let path = repo.join("a").join(name);
            std::fs::write(&path, "").unwrap();
            assert_eq!(crush_config_present(&work), Some(path.clone()), "{name}");
            assert!(check_config(&work, None).is_err());
            std::fs::remove_file(&path).unwrap();
        }
        std::fs::write(work.join(".env"), "CRUSH_HOME=/").unwrap();
        assert!(check_config(&work, None).unwrap_err().contains(".env"));
        std::fs::remove_file(work.join(".env")).unwrap();
        // Only the working directory's own `.env` is loaded.
        std::fs::write(repo.join(".env"), "X=1").unwrap();
        assert!(check_config(&work, None).is_ok());
        std::fs::write(work.join(".crushrc"), "echo hi").unwrap();
        assert!(check_config(&work, None).unwrap_err().contains(".crushrc"));
    }

    #[test]
    fn system_config_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let etc = root.path().join("etc");
        std::fs::create_dir(&etc).unwrap();
        let sys = etc.join("crush.json");
        let work = root.path().join("w");
        std::fs::create_dir(&work).unwrap();
        assert!(check_config(&work, Some(&sys)).is_ok());
        std::fs::write(&sys, "{}").unwrap();
        assert!(
            check_config(&work, Some(&sys))
                .unwrap_err()
                .contains("system-wide")
        );
    }

    #[test]
    fn session_json_gives_id_and_usage() {
        let after = parse_session(
            r#"{"meta":{"id":"9f1c","uuid":"0b6d-uuid","cost":0.75,"prompt_tokens":1200,"completion_tokens":80,"total_tokens":1280},"messages":[]}"#,
        )
        .unwrap();
        assert_eq!(after.id, "0b6d-uuid");
        let u = usage(&after, None);
        assert_eq!((u.input_tokens, u.output_tokens), (1200, 80));
        assert_eq!(u.cost_usd, Some(0.75));
        let before = SessionInfo {
            cost: Some(0.5),
            ..after.clone()
        };
        let u = usage(&after, Some(&before));
        assert!((u.cost_usd.unwrap() - 0.25).abs() < 1e-12);
        assert!(parse_session("no sessions found").is_none());
        assert_eq!(parse_session(r#"{"meta":{"id":"abc"}}"#).unwrap().id, "abc");
    }

    #[test]
    fn data_dirs_differ_per_workdir() {
        let a = data_dir(Path::new("/acct"), Path::new("/w/one"));
        let b = data_dir(Path::new("/acct"), Path::new("/w/two"));
        assert_ne!(a, b);
        assert!(a.starts_with("/acct/data"));
        assert_eq!(a.file_name().unwrap().len(), 64);
    }

    #[test]
    fn limits_are_recognised() {
        for msg in [
            "agent exited with exit status: 1: agent processing failed: Rate limit exceeded: slow down",
            "Too Many Requests: try later",
            "HTTP 429: busy",
            "Insufficient quota: check your plan",
            "Usage limit reached for this month",
            "RESOURCE_EXHAUSTED: quota",
        ] {
            assert!(limit_hit(msg).is_some(), "{msg}");
        }
        for msg in ["Invalid API key", "model not found", "error 14290"] {
            assert!(limit_hit(msg).is_none(), "{msg}");
        }
    }

    #[test]
    fn account_dir_comes_from_home() {
        // An absolute path on every platform (`/h` is not one on Windows).
        let dir = std::env::temp_dir()
            .join("h")
            .join("accounts")
            .join("crush")
            .join("default");
        let env = EnvOverride {
            remove: vec![],
            set: account_env(&dir),
        };
        assert_eq!(account_dir_of(&env), Some(dir));
        assert_eq!(account_dir_of(&EnvOverride::default()), None);
    }
}
