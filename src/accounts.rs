//! Several accounts per agent.
//!
//! An account is `(agent, name)`, in one of three kinds:
//!
//! - **default**: implicit and always there, the owner's normal login. A run
//!   on it changes nothing.
//! - **login**: a directory `<home>/accounts/<agent>/<name>/` (mode 0700).
//!   Each run points the agent's account variable (for example
//!   `CLAUDE_CONFIG_DIR`) at its absolute path, and the CLI keeps its own
//!   login and token refreshes in there. Hashi never reads, copies or
//!   refreshes those credentials.
//! - **key**: an API key in `<home>/accounts/<agent>/<name>/key` (mode
//!   0600), passed in the agent's API-key variable. The account variable
//!   points at the same directory, so the owner's own config for the agent
//!   (which could pick another sign-in or supply another key) never loads.
//! - **remote**: access a friend shared with the owner. A run sends the
//!   prompt to the saved remote's bridge (see [`crate::remote`] and
//!   [`crate::forward`]) instead of starting a process. The remote's token
//!   stays in `remotes.json`; the database keeps only the remote's name.
//!
//! Every run on a non-default account first removes the agent's credential
//! variables inherited from the owner ([`credential_vars`]), so an
//! `ANTHROPIC_API_KEY` in the owner's shell cannot outrank the account.
//!
//! Crush is the exception to "a run on the default account changes
//! nothing": its default account also has a Hashi-owned directory,
//! `<home>/accounts/crush/default/`, so the owner's own Crush config (with
//! its MCP servers and hooks) never loads. See [`run_env`].
//!
//! Tokens and invites carry an [`AccountMap`]: for each agent, the ordered
//! list of accounts it may use. An agent with no entry uses `default`.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::agent::{AgentKind, EnvOverride};

pub mod ops;

/// Name of the implicit account that is the owner's own login.
pub const DEFAULT: &str = "default";

/// Directory under the Hashi home that holds the accounts.
pub const DIR: &str = "accounts";

/// File holding a key account's key.
pub const KEY_FILE: &str = "key";

/// Most accounts one token may list for one agent.
pub const MAX_PER_AGENT: usize = 8;

/// Ordered accounts per agent, as stored on a token or invite.
pub type AccountMap = BTreeMap<AgentKind, Vec<String>>;

/// How an account signs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AccountKind {
    /// The owner's normal login.
    Default,
    /// A login directory owned by Hashi.
    Login,
    /// An API key.
    ApiKey,
    /// A Claude Code OAuth token, from `claude setup-token`.
    OauthToken,
    /// A friend's bridge, through a saved remote.
    Remote,
}

impl AccountKind {
    /// Stable identifier used in the database and on the command line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Login => "login",
            Self::ApiKey => "api-key",
            Self::OauthToken => "oauth-token",
            Self::Remote => "remote",
        }
    }

    /// `true` for the kinds that store a key.
    #[must_use]
    pub const fn is_key(self) -> bool {
        matches!(self, Self::ApiKey | Self::OauthToken)
    }
}

impl fmt::Display for AccountKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AccountKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "default" => Ok(Self::Default),
            "login" => Ok(Self::Login),
            "api-key" | "key" => Ok(Self::ApiKey),
            "oauth-token" => Ok(Self::OauthToken),
            "remote" => Ok(Self::Remote),
            other => Err(format!(
                "unknown account kind `{other}` (expected login, api-key, oauth-token or remote)"
            )),
        }
    }
}

/// Checks an account name: 1 to 32 characters from `[a-z0-9-]`, not
/// starting with `-`. `default` is reserved unless `allow_default`.
///
/// # Errors
///
/// Returns a message saying what is wrong.
pub fn check_name(name: &str, allow_default: bool) -> Result<(), String> {
    let ok = (1..=32).contains(&name.len())
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !ok {
        return Err(format!(
            "account name `{}` must be 1 to 32 characters from a-z, 0-9 and -",
            crate::util::truncate(name, 40)
        ));
    }
    if name == DEFAULT && !allow_default {
        return Err("`default` is the owner's own login and cannot be added".to_owned());
    }
    Ok(())
}

/// Whether `agent` supports accounts of `kind`.
#[must_use]
pub const fn supports(agent: AgentKind, kind: AccountKind) -> bool {
    match kind {
        AccountKind::Default => true,
        AccountKind::Login => dir_var(agent).is_some(),
        AccountKind::ApiKey | AccountKind::OauthToken => key_var(agent, kind).is_some(),
        AccountKind::Remote => true,
    }
}

/// The variable that points `agent` at an account directory.
#[must_use]
pub const fn dir_var(agent: AgentKind) -> Option<&'static str> {
    match agent {
        AgentKind::Claude => Some("CLAUDE_CONFIG_DIR"),
        AgentKind::Codex => Some("CODEX_HOME"),
        AgentKind::Gemini => Some("GEMINI_CLI_HOME"),
        AgentKind::Opencode => Some("XDG_DATA_HOME"),
        AgentKind::Qwen => Some("QWEN_HOME"),
        AgentKind::Cursor => None,
        // With the CRUSH_* directories beside it; see `account_vars`.
        AgentKind::Crush => Some("HOME"),
    }
}

/// The variables that point `agent` at the account directory `dir`: the
/// [`dir_var`] alone, or for Crush its config, data, cache and skills
/// directories and `HOME`.
#[must_use]
pub fn account_vars(agent: AgentKind, dir: &Path) -> Vec<(String, String)> {
    match (agent, dir_var(agent)) {
        (AgentKind::Crush, _) => crate::agent::crush::account_env(dir),
        (_, Some(var)) => vec![(var.to_owned(), dir.to_string_lossy().into_owned())],
        (_, None) => Vec::new(),
    }
}

/// Inherited variables removed from every run of `agent`, on any account,
/// default included. Only Crush has any: every `CRUSH_*` variable (each
/// overrides the variable without the prefix) and the XDG directories.
#[must_use]
pub const fn always_removed(agent: AgentKind) -> &'static [&'static str] {
    match agent {
        AgentKind::Crush => &crate::agent::crush::ALWAYS_REMOVE,
        _ => &[],
    }
}

/// The variable that passes a key account's key to `agent`.
#[must_use]
pub const fn key_var(agent: AgentKind, kind: AccountKind) -> Option<&'static str> {
    match (agent, kind) {
        (AgentKind::Claude, AccountKind::ApiKey) => Some("ANTHROPIC_API_KEY"),
        (AgentKind::Claude, AccountKind::OauthToken) => Some("CLAUDE_CODE_OAUTH_TOKEN"),
        (AgentKind::Codex, AccountKind::ApiKey) => Some("CODEX_API_KEY"),
        (AgentKind::Gemini, AccountKind::ApiKey) => Some("GEMINI_API_KEY"),
        (AgentKind::Cursor, AccountKind::ApiKey) => Some("CURSOR_API_KEY"),
        _ => None,
    }
}

/// The credential variables of `agent` that a run on a non-default account
/// removes from the environment inherited from the owner. Besides keys and
/// tokens this covers everything that picks another provider or endpoint
/// (a cloud provider switch, a base URL, a cloud project), since any of
/// them would send the account's run to the owner's cloud account. A
/// trailing `*` matches a prefix (see [`crate::agent::runner::env_name_matches`]).
#[must_use]
pub const fn credential_vars(agent: AgentKind) -> &'static [&'static str] {
    match agent {
        // Claude Code's provider settings: "Amazon Bedrock", "Google Vertex
        // AI" and "Microsoft Foundry" in its third-party integration docs,
        // and the AWS SDK and Google Cloud variables they read.
        AgentKind::Claude => &[
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_CUSTOM_HEADERS",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
            "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
            "CLAUDE_CONFIG_DIR",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
            "CLAUDE_CODE_SKIP_BEDROCK_AUTH",
            "CLAUDE_CODE_SKIP_VERTEX_AUTH",
            "CLAUDE_CODE_SKIP_FOUNDRY_AUTH",
            "ANTHROPIC_BEDROCK_*",
            "ANTHROPIC_VERTEX_*",
            "ANTHROPIC_FOUNDRY_*",
            "AWS_*",
            "GOOGLE_CLOUD_*",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "GCLOUD_PROJECT",
            "CLOUD_ML_REGION",
            "VERTEX_REGION_*",
        ],
        // codex-rs: `login/src/auth/manager.rs` (OPENAI_API_KEY,
        // CODEX_API_KEY, CODEX_ACCESS_TOKEN), `model-provider-info/src/lib.rs`
        // (OPENAI_BASE_URL, and OPENAI_ORGANIZATION and OPENAI_PROJECT sent
        // as headers), `protocol/src/shell_environment.rs` (workload
        // identity) and the auth and refresh URL overrides.
        AgentKind::Codex => &[
            "OPENAI_API_KEY",
            "CODEX_API_KEY",
            "CODEX_ACCESS_TOKEN",
            "CODEX_HOME",
            "CODEX_SQLITE_HOME",
            "OPENAI_BASE_URL",
            "OPENAI_ORGANIZATION",
            "OPENAI_ORG_ID",
            "OPENAI_PROJECT",
            "OPENAI_IDENTITY_TOKEN_FILE",
            "OPENAI_WORKLOAD_IDENTITY_CONTEXT",
            "OPENAI_FEDERATION_RULE_ID",
            "CODEX_AUTHAPI_BASE_URL",
            "CODEX_REFRESH_TOKEN_URL_OVERRIDE",
            "CODEX_APP_SERVER_MANAGED_CONFIG_PATH",
        ],
        // gemini-cli: `core/src/core/contentGenerator.ts` (auth type from
        // the environment), `cli/src/config/settings.ts` (system settings
        // paths, Cloud Shell) and `core/src/utils/trust.ts` (workspace trust,
        // which also decides whether `.gemini/.env` files load).
        AgentKind::Gemini => &[
            "GEMINI_API_KEY",
            "GOOGLE_API_KEY",
            "GOOGLE_GENAI_USE_GCA",
            "GOOGLE_GENAI_USE_VERTEXAI",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "GOOGLE_CLOUD_*",
            "GOOGLE_GEMINI_BASE_URL",
            "GOOGLE_VERTEX_BASE_URL",
            "GEMINI_CLI_HOME",
            "GEMINI_FORCE_ENCRYPTED_FILE_STORAGE",
            "GEMINI_CLI_USE_COMPUTE_ADC",
            "GEMINI_DEFAULT_AUTH_TYPE",
            "GEMINI_CLI_AUTH_OVERRIDE",
            "GEMINI_CLI_TRUST_WORKSPACE",
            "GEMINI_CLI_TRUSTED_FOLDERS_PATH",
            "GEMINI_CLI_SYSTEM_SETTINGS_PATH",
            "GEMINI_CLI_SYSTEM_DEFAULTS_PATH",
            "GEMINI_CLI_IDE_*",
            "CLOUD_SHELL",
        ],
        AgentKind::Cursor => &["CURSOR_API_KEY"],
        // qwen-code: `core/src/models/constants.ts` `AUTH_ENV_MAPPINGS` and
        // the `envKey` of each provider in `core/src/providers/presets/`.
        AgentKind::Qwen => &[
            "QWEN_HOME",
            "OPENAI_API_KEY",
            "OPENAI_BASE_URL",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_BASE_URL",
            "GEMINI_API_KEY",
            "GOOGLE_API_KEY",
            "GOOGLE_GENAI_USE_VERTEXAI",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "GOOGLE_CLOUD_*",
            "DASHSCOPE_API_KEY",
            "BAILIAN_CODING_PLAN_API_KEY",
            "BAILIAN_TOKEN_PLAN_API_KEY",
            "QWENCLOUD_API_KEY",
            "QWENCLOUD_TOKEN_PLAN_API_KEY",
            "DEEPSEEK_API_KEY",
            "IDEALAB_API_KEY",
            "ZAI_API_KEY",
            "REQUESTY_API_KEY",
            "MODELSCOPE_API_KEY",
            "MOONSHOT_API_KEY",
            "MINIMAX_API_KEY",
            "XAI_API_KEY",
            "OPENROUTER_API_KEY",
            "OPENCODE_GO_API_KEY",
        ],
        AgentKind::Opencode => &["XDG_DATA_HOME", "OPENCODE_DB"],
        AgentKind::Crush => &crate::agent::crush::PROVIDER_VARS,
    }
}

/// How to sign a login account in: run the agent with these arguments and
/// extra variables, with the account variable set, on the owner's terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignIn {
    /// Arguments after the executable.
    pub args: &'static [&'static str],
    /// Extra variables besides the account variable.
    pub env: &'static [(&'static str, &'static str)],
    /// What to do once the CLI is open, when it is not automatic.
    pub hint: Option<&'static str>,
}

/// The sign-in command for a login account of `agent`.
#[must_use]
pub const fn sign_in(agent: AgentKind) -> Option<SignIn> {
    Some(match agent {
        AgentKind::Claude => SignIn {
            args: &["auth", "login"],
            env: &[],
            hint: None,
        },
        AgentKind::Codex => SignIn {
            args: &["login"],
            env: &[],
            hint: Some("on a machine without a browser, run `codex login --device-auth` instead"),
        },
        AgentKind::Gemini => SignIn {
            args: &[],
            env: &[("NO_BROWSER", "true")],
            hint: Some("choose Sign in with Google, then quit Gemini CLI"),
        },
        AgentKind::Opencode => SignIn {
            args: &["auth", "login", "--standalone"],
            env: &[],
            hint: None,
        },
        AgentKind::Qwen => SignIn {
            args: &[],
            env: &[],
            hint: Some("type /auth and set up the API-key plan, then quit Qwen Code"),
        },
        AgentKind::Crush => SignIn {
            args: &["login"],
            env: &[],
            hint: Some(
                "this signs in to Charm Hyper; for Copilot, ChatGPT or Grok run the same command \
                 with `login copilot`, `login openai` or `login grok`",
            ),
        },
        AgentKind::Cursor => return None,
    })
}

/// Arguments of a command that reports whether a login account is signed
/// in, where the CLI has one. Only Claude Code's is known.
#[must_use]
pub const fn status_args(agent: AgentKind) -> Option<&'static [&'static str]> {
    match agent {
        AgentKind::Claude => Some(&["auth", "status", "--json"]),
        _ => None,
    }
}

/// `<home>/accounts`, absolute. The home is resolved once through
/// `canonicalize` when it exists, so every run passes the same string:
/// Claude Code on macOS ties its Keychain entry to the exact path.
#[must_use]
pub fn root(home: &Path) -> PathBuf {
    let home = std::fs::canonicalize(home)
        .or_else(|_| std::path::absolute(home))
        .unwrap_or_else(|_| home.to_path_buf());
    home.join(DIR)
}

/// The directory of account `name` of `agent`.
#[must_use]
pub fn account_dir(home: &Path, agent: AgentKind, name: &str) -> PathBuf {
    root(home).join(agent.as_str()).join(name)
}

/// Creates `dir` and any missing parents up to it, each owner-only (0700).
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
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

/// Creates the Crush default account's directory,
/// `<home>/accounts/crush/default/`, owner-only, and returns it.
///
/// # Errors
///
/// Fails if a directory cannot be created or a non-directory is in the way.
pub fn crush_default_dir(home: &Path) -> std::io::Result<PathBuf> {
    let root = root(home);
    create_private_dir(&root)?;
    let agent_dir = root.join(AgentKind::Crush.as_str());
    create_private_dir(&agent_dir)?;
    let dir = agent_dir.join(DEFAULT);
    if std::fs::symlink_metadata(&dir).is_ok_and(|m| !m.is_dir()) {
        return Err(std::io::Error::other(format!(
            "{} exists and is not a directory",
            dir.display()
        )));
    }
    create_private_dir(&dir)?;
    Ok(dir)
}

/// Creates the account's directory (and `accounts/<agent>/`), owner-only.
///
/// # Errors
///
/// Fails if the name is invalid or a directory cannot be created.
pub fn create_dir(home: &Path, agent: AgentKind, name: &str) -> std::io::Result<PathBuf> {
    check_name(name, false).map_err(std::io::Error::other)?;
    let root = root(home);
    create_private_dir(&root)?;
    create_private_dir(&root.join(agent.as_str()))?;
    let dir = root.join(agent.as_str()).join(name);
    // A link in place of the account directory would point the CLI's
    // credentials somewhere else.
    if std::fs::symlink_metadata(&dir).is_ok_and(|m| !m.is_dir()) {
        return Err(std::io::Error::other(format!(
            "{} exists and is not a directory",
            dir.display()
        )));
    }
    create_private_dir(&dir)?;
    Ok(dir)
}

/// Checks a key: no whitespace inside, no control characters, at most 4096
/// characters. Surrounding whitespace (a trailing newline) is dropped.
///
/// # Errors
///
/// Returns a message that never contains the key.
pub fn clean_key(key: &str) -> Result<String, String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("the key is empty".to_owned());
    }
    if key.len() > 4_096 || key.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("the key must be one line of at most 4096 characters".to_owned());
    }
    Ok(key.to_owned())
}

/// Writes a key account's key, mode 0600, creating its directory.
///
/// # Errors
///
/// Fails if the directory or file cannot be written.
pub fn write_key(home: &Path, agent: AgentKind, name: &str, key: &str) -> std::io::Result<()> {
    let dir = create_dir(home, agent, name)?;
    let path = dir.join(KEY_FILE);
    if std::fs::symlink_metadata(&path).is_ok_and(|m| !m.is_file()) {
        return Err(std::io::Error::other("the key path is not a regular file"));
    }
    crate::diagnostics::write_private(&path, key)
}

/// Reads a key account's key.
///
/// # Errors
///
/// Fails if the file is missing, unreadable or not a valid key.
pub fn read_key(home: &Path, agent: AgentKind, name: &str) -> std::io::Result<String> {
    let path = account_dir(home, agent, name).join(KEY_FILE);
    if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "the account has no key file",
        ));
    }
    let text = std::fs::read_to_string(&path)?;
    clean_key(&text).map_err(std::io::Error::other)
}

/// Whether `dir` is a directory with anything in it.
#[must_use]
pub fn has_files(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir())
        && std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_some())
}

/// Deletes an account's directory without following symlinks. Returns
/// `false` when there was nothing to delete.
///
/// # Errors
///
/// Fails if the name is invalid or deleting fails.
pub fn remove_dir(home: &Path, agent: AgentKind, name: &str) -> std::io::Result<bool> {
    check_name(name, false).map_err(std::io::Error::other)?;
    crate::server::remove_token_workspace(&root(home).join(agent.as_str()), name)
}

/// What a run on account `name` of `kind` changes in the child's
/// environment, or `None` for the default account. Crush's default account
/// is `Some`: its own directory (created here when missing) and only the
/// [`always_removed`] variables, so the owner's provider keys still work.
///
/// # Errors
///
/// Fails if a key account's key cannot be read, or the kind is not
/// supported for `agent`.
pub fn run_env(
    home: &Path,
    agent: AgentKind,
    name: &str,
    kind: AccountKind,
) -> std::io::Result<Option<EnvOverride>> {
    let set = match kind {
        AccountKind::Default if agent == AgentKind::Crush => {
            return Ok(Some(EnvOverride {
                remove: always_removed(agent).to_vec(),
                set: account_vars(agent, &crush_default_dir(home)?),
            }));
        }
        AccountKind::Default => return Ok(None),
        AccountKind::Login => {
            dir_var(agent).ok_or_else(|| unsupported(agent, kind))?;
            let dir = account_dir(home, agent, name);
            if !std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "the account directory is missing",
                ));
            }
            if agent == AgentKind::Gemini {
                crate::agent::gemini::prepare_account_home(&dir, false)?;
            }
            account_vars(agent, &dir)
        }
        AccountKind::ApiKey | AccountKind::OauthToken => {
            let var = key_var(agent, kind).ok_or_else(|| unsupported(agent, kind))?;
            let key = read_key(home, agent, name)?;
            // The account's own directory too, so the owner's config (Claude
            // Code's `apiKeyHelper` or `env`, Gemini CLI's chosen sign-in)
            // cannot outrank the key.
            let mut set = if dir_var(agent).is_some() {
                let dir = create_dir(home, agent, name)?;
                if agent == AgentKind::Gemini {
                    crate::agent::gemini::prepare_account_home(&dir, true)?;
                }
                account_vars(agent, &dir)
            } else {
                Vec::new()
            };
            set.push((var.to_owned(), key));
            set
        }
        // A remote account never starts a process.
        AccountKind::Remote => return Err(unsupported(agent, kind)),
    };
    let mut remove = credential_vars(agent).to_vec();
    remove.extend_from_slice(always_removed(agent));
    Ok(Some(EnvOverride { remove, set }))
}

fn unsupported(agent: AgentKind, kind: AccountKind) -> std::io::Error {
    std::io::Error::other(format!(
        "{} does not support {kind} accounts",
        agent.display_name()
    ))
}

/// Parses one `--account` value, `claude=work,personal`.
///
/// # Errors
///
/// Returns a message for an unknown agent or an invalid name.
pub fn parse_binding(s: &str) -> Result<(AgentKind, Vec<String>), String> {
    let (agent, names) = s
        .split_once('=')
        .ok_or_else(|| format!("expected <agent>=<account>[,<account>...], got `{s}`"))?;
    let agent: AgentKind = agent
        .parse()
        .map_err(|e: crate::agent::UnknownAgent| e.to_string())?;
    let names: Vec<String> = names
        .split(',')
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_owned)
        .collect();
    if names.is_empty() {
        return Err(format!("name at least one account for {agent}"));
    }
    for n in &names {
        check_name(n, true)?;
    }
    Ok((agent, names))
}

/// Parses several bindings separated by spaces or `;`, as typed in the
/// dashboard: `claude=work,personal codex=team`. An empty string is an
/// empty map.
///
/// # Errors
///
/// As for [`parse_binding`] and [`normalize`].
pub fn parse_bindings(s: &str) -> Result<AccountMap, String> {
    let mut map = AccountMap::new();
    for part in s.split([' ', ';']).filter(|p| !p.trim().is_empty()) {
        let (agent, names) = parse_binding(part.trim())?;
        map.insert(agent, names);
    }
    normalize(map)
}

/// Checks a map's shape: valid names, no account twice for one agent, at
/// most [`MAX_PER_AGENT`] per agent. Agents with an empty list are dropped.
/// Whether the accounts exist is checked against the store separately.
///
/// # Errors
///
/// Returns a message saying what is wrong.
pub fn normalize(map: AccountMap) -> Result<AccountMap, String> {
    let mut out = AccountMap::new();
    for (agent, names) in map {
        if names.is_empty() {
            continue;
        }
        if names.len() > MAX_PER_AGENT {
            return Err(format!(
                "at most {MAX_PER_AGENT} accounts per agent, {agent} has {}",
                names.len()
            ));
        }
        for (i, n) in names.iter().enumerate() {
            check_name(n, true)?;
            if names[..i].contains(n) {
                return Err(format!("account `{n}` is listed twice for {agent}"));
            }
        }
        out.insert(agent, names);
    }
    Ok(out)
}

/// The accounts `map` lists for `agent`, `["default"]` when it has none.
#[must_use]
pub fn for_agent(map: &AccountMap, agent: AgentKind) -> Vec<String> {
    map.get(&agent)
        .filter(|l| !l.is_empty())
        .cloned()
        .unwrap_or_else(|| vec![DEFAULT.to_owned()])
}

/// A map as `claude=work,personal codex=team`, or `default` when empty.
#[must_use]
pub fn format_bindings(map: &AccountMap) -> String {
    if map.is_empty() {
        return DEFAULT.to_owned();
    }
    map.iter()
        .map(|(a, names)| format!("{a}={}", names.join(",")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Encodes a map for the database.
#[must_use]
pub fn encode(map: &AccountMap) -> String {
    serde_json::to_string(map).unwrap_or_else(|_| "{}".to_owned())
}

/// Decodes a stored map. Anything unreadable is an empty map, which means
/// every agent uses its default account.
#[must_use]
pub fn decode(s: &str) -> AccountMap {
    serde_json::from_str(s).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_rules() {
        assert!(check_name("work", false).is_ok());
        assert!(check_name("team-2", false).is_ok());
        assert!(check_name(&"a".repeat(32), false).is_ok());
        for bad in ["", "Work", "a_b", "a b", "-x", "ü", &"a".repeat(33)] {
            assert!(check_name(bad, false).is_err(), "{bad}");
        }
        assert!(check_name("default", false).is_err());
        assert!(check_name("default", true).is_ok());
    }

    #[test]
    fn kinds_are_supported_where_the_research_says() {
        use AccountKind::{ApiKey, Login, OauthToken};
        for agent in [
            AgentKind::Claude,
            AgentKind::Codex,
            AgentKind::Gemini,
            AgentKind::Opencode,
            AgentKind::Qwen,
        ] {
            assert!(supports(agent, Login), "{agent}");
        }
        assert!(!supports(AgentKind::Cursor, Login));
        for agent in [
            AgentKind::Claude,
            AgentKind::Codex,
            AgentKind::Gemini,
            AgentKind::Cursor,
        ] {
            assert!(supports(agent, ApiKey), "{agent}");
        }
        assert!(!supports(AgentKind::Opencode, ApiKey));
        assert!(!supports(AgentKind::Qwen, ApiKey));
        assert!(supports(AgentKind::Claude, OauthToken));
        assert!(!supports(AgentKind::Codex, OauthToken));
    }

    #[test]
    fn kinds_parse_and_print() {
        for k in [
            AccountKind::Default,
            AccountKind::Login,
            AccountKind::ApiKey,
            AccountKind::OauthToken,
            AccountKind::Remote,
        ] {
            assert_eq!(k.as_str().parse::<AccountKind>(), Ok(k));
        }
        assert!("password".parse::<AccountKind>().is_err());
    }

    #[test]
    fn bindings_parse_and_round_trip() {
        let map = parse_bindings("claude=work,personal codex=team").unwrap();
        assert_eq!(map[&AgentKind::Claude], ["work", "personal"]);
        assert_eq!(decode(&encode(&map)), map);
        assert_eq!(format_bindings(&map), "claude=work,personal codex=team");
        assert_eq!(parse_bindings("").unwrap(), AccountMap::new());
        assert!(parse_bindings("claude=work,work").is_err());
        assert!(parse_bindings("claude=").is_err());
        assert!(parse_bindings("copilot=x").is_err());
        assert!(parse_bindings("claude=Bad").is_err());
        assert_eq!(for_agent(&map, AgentKind::Gemini), ["default"]);
        assert_eq!(decode("not json"), AccountMap::new());
    }

    #[test]
    fn json_maps_use_agent_names() {
        let map: AccountMap = serde_json::from_str(r#"{"claude":["a"],"gemini":["b"]}"#).unwrap();
        assert_eq!(map[&AgentKind::Gemini], ["b"]);
        assert_eq!(
            serde_json::to_string(&map).unwrap(),
            r#"{"claude":["a"],"gemini":["b"]}"#
        );
    }

    #[test]
    fn keys_are_checked_without_echoing_them() {
        assert_eq!(clean_key("  sk-abc\n").unwrap(), "sk-abc");
        let err = clean_key("sk a").unwrap_err();
        assert!(!err.contains("sk"));
        assert!(clean_key("\n").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn directories_and_keys_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let dir = create_dir(home.path(), AgentKind::Claude, "work").unwrap();
        assert!(dir.is_absolute());
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&root(home.path())), 0o700);
        write_key(home.path(), AgentKind::Codex, "ci", "sk-test-1").unwrap();
        let key = account_dir(home.path(), AgentKind::Codex, "ci").join(KEY_FILE);
        assert_eq!(mode(&key), 0o600);
        assert_eq!(
            read_key(home.path(), AgentKind::Codex, "ci").unwrap(),
            "sk-test-1"
        );

        let env = run_env(home.path(), AgentKind::Codex, "ci", AccountKind::ApiKey)
            .unwrap()
            .unwrap();
        let ci = account_dir(home.path(), AgentKind::Codex, "ci");
        assert_eq!(
            env.set,
            [
                ("CODEX_HOME".to_owned(), ci.to_string_lossy().into_owned()),
                ("CODEX_API_KEY".to_owned(), "sk-test-1".to_owned())
            ]
        );
        assert!(env.remove.contains(&"OPENAI_API_KEY"));
        assert!(!format!("{env:?}").contains("sk-test-1"));
        let env = run_env(home.path(), AgentKind::Claude, "work", AccountKind::Login)
            .unwrap()
            .unwrap();
        assert_eq!(env.set[0].0, "CLAUDE_CONFIG_DIR");
        assert_eq!(env.set[0].1, dir.to_string_lossy());
        assert!(
            run_env(home.path(), AgentKind::Claude, "x", AccountKind::Default)
                .unwrap()
                .is_none()
        );
    }

    /// The owner's Claude credential variables, set in a re-run of this
    /// test binary (the environment cannot be changed in process).
    const OWNER_VARS: [(&str, &str); 11] = [
        ("ANTHROPIC_API_KEY", "owner-api-key"),
        ("ANTHROPIC_AUTH_TOKEN", "owner-auth-token"),
        ("CLAUDE_CODE_OAUTH_TOKEN", "owner-oauth"),
        ("CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR", "7"),
        ("CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR", "8"),
        ("CLAUDE_CONFIG_DIR", "/owner/.claude"),
        ("CLAUDE_CODE_USE_BEDROCK", "1"),
        ("AWS_BEARER_TOKEN_BEDROCK", "owner-bedrock"),
        ("ANTHROPIC_BASE_URL", "https://owner.example"),
        ("GOOGLE_CLOUD_PROJECT", "owner-project"),
        ("CLOUD_ML_REGION", "us-east5"),
    ];

    /// Prints the Claude credential variables it sees, as Claude's JSON result.
    const ENV_PROBE: &str = r#"#!/bin/sh
seen=""
for v in ANTHROPIC_API_KEY ANTHROPIC_AUTH_TOKEN CLAUDE_CODE_OAUTH_TOKEN \
         CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR CLAUDE_CONFIG_DIR \
         CLAUDE_CODE_USE_BEDROCK AWS_BEARER_TOKEN_BEDROCK ANTHROPIC_BASE_URL GOOGLE_CLOUD_PROJECT \
         CLOUD_ML_REGION; do
  eval "val=\${$v+set:\$$v}"
  if [ -n "$val" ]; then seen="$seen $v=${val#set:}"; fi
done
printf '{"type":"result","is_error":false,"result":"env:%s","session_id":"s"}\n' "$seen"
"#;

    #[cfg(unix)]
    #[tokio::test]
    async fn a_key_account_run_sees_only_its_key() {
        use std::os::unix::fs::PermissionsExt;
        const MARK: &str = "HASHI_ACCOUNT_ENV_PROBE";
        if std::env::var_os(MARK).is_some() {
            let home = tempfile::tempdir().unwrap();
            let bin = home.path().join("claude");
            std::fs::write(&bin, ENV_PROBE).unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            write_key(home.path(), AgentKind::Claude, "ci", "sk-account-key").unwrap();
            let login = create_dir(home.path(), AgentKind::Claude, "work").unwrap();
            let cfg = crate::agent::AgentConfig {
                bin,
                ..crate::agent::AgentConfig::default_for(AgentKind::Claude)
            };
            let runs = [
                (
                    "key",
                    run_env(home.path(), AgentKind::Claude, "ci", AccountKind::ApiKey),
                ),
                (
                    "login",
                    run_env(home.path(), AgentKind::Claude, "work", AccountKind::Login),
                ),
                (
                    "default",
                    run_env(
                        home.path(),
                        AgentKind::Claude,
                        DEFAULT,
                        AccountKind::Default,
                    ),
                ),
            ];
            for (label, env) in runs {
                let env = env.unwrap();
                let inv = crate::agent::Invocation {
                    prompt: "hi",
                    resume: None,
                    model: None,
                };
                let out = crate::agent::execute_as(
                    AgentKind::Claude,
                    &cfg,
                    inv,
                    home.path(),
                    crate::agent::Limits::default(),
                    env.as_ref(),
                )
                .await
                .unwrap();
                println!("{label}:[{}]", out.text);
            }
            println!("login-dir:[{}]", login.display());
            let key_dir = account_dir(home.path(), AgentKind::Claude, "ci");
            println!("key-dir:[{}]", key_dir.display());
            return;
        }
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "accounts::tests::a_key_account_run_sees_only_its_key",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(MARK, "1");
        for (k, v) in OWNER_VARS {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{stdout}");
        let line = |label: &str| {
            let start = stdout.find(&format!("{label}:[")).unwrap() + label.len() + 2;
            stdout[start..].split(']').next().unwrap().to_owned()
        };
        // The key account has its own config directory too, so the owner's
        // `~/.claude` settings (an `apiKeyHelper`, say) never load.
        assert_eq!(
            line("key"),
            format!(
                "env: ANTHROPIC_API_KEY=sk-account-key CLAUDE_CONFIG_DIR={}",
                line("key-dir")
            )
        );
        assert_eq!(
            line("login"),
            format!("env: CLAUDE_CONFIG_DIR={}", line("login-dir"))
        );
        let default = line("default");
        for (k, v) in OWNER_VARS {
            assert!(default.contains(&format!("{k}={v}")), "{default}");
        }
    }

    /// Whether a run with `env` drops the inherited variable `name`.
    fn strips(env: &EnvOverride, name: &str) -> bool {
        env.remove
            .iter()
            .any(|r| crate::agent::runner::env_name_matches(r, name))
    }

    #[test]
    fn cloud_provider_and_endpoint_variables_never_reach_an_account() {
        let home = tempfile::tempdir().unwrap();
        let cases: [(AgentKind, &[&str]); 4] = [
            (
                AgentKind::Claude,
                &[
                    "CLAUDE_CODE_USE_BEDROCK",
                    "CLAUDE_CODE_USE_VERTEX",
                    "CLAUDE_CODE_USE_FOUNDRY",
                    "AWS_BEARER_TOKEN_BEDROCK",
                    "AWS_ACCESS_KEY_ID",
                    "AWS_SECRET_ACCESS_KEY",
                    "AWS_SESSION_TOKEN",
                    "AWS_PROFILE",
                    "AWS_REGION",
                    "ANTHROPIC_FOUNDRY_API_KEY",
                    "ANTHROPIC_VERTEX_PROJECT_ID",
                    "ANTHROPIC_BASE_URL",
                    "GOOGLE_CLOUD_PROJECT",
                    "GOOGLE_APPLICATION_CREDENTIALS",
                    "CLOUD_ML_REGION",
                ],
            ),
            (
                AgentKind::Codex,
                &[
                    "OPENAI_BASE_URL",
                    "OPENAI_ORGANIZATION",
                    "OPENAI_PROJECT",
                    "CODEX_ACCESS_TOKEN",
                ],
            ),
            (
                AgentKind::Gemini,
                &[
                    "GOOGLE_CLOUD_PROJECT",
                    "GOOGLE_CLOUD_LOCATION",
                    "GOOGLE_GEMINI_BASE_URL",
                    "GEMINI_CLI_TRUST_WORKSPACE",
                    "GEMINI_CLI_SYSTEM_SETTINGS_PATH",
                ],
            ),
            (
                AgentKind::Qwen,
                &[
                    "ANTHROPIC_BASE_URL",
                    "DASHSCOPE_API_KEY",
                    "GOOGLE_CLOUD_PROJECT",
                    "OPENROUTER_API_KEY",
                ],
            ),
        ];
        for (agent, vars) in cases {
            create_dir(home.path(), agent, "work").unwrap();
            let env = run_env(home.path(), agent, "work", AccountKind::Login)
                .unwrap()
                .unwrap();
            for var in vars {
                assert!(strips(&env, var), "{agent} keeps {var}");
            }
            // Unrelated variables stay.
            assert!(!strips(&env, "PATH"), "{agent}");
        }
        write_key(home.path(), AgentKind::Claude, "ci", "sk-1").unwrap();
        let env = run_env(home.path(), AgentKind::Claude, "ci", AccountKind::ApiKey)
            .unwrap()
            .unwrap();
        assert!(strips(&env, "CLAUDE_CODE_USE_BEDROCK"));
        assert!(strips(&env, "AWS_SECRET_ACCESS_KEY"));
    }

    #[cfg(unix)]
    #[test]
    fn key_accounts_get_their_own_private_config_directory() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        for (agent, kind, dir_var, key_var) in [
            (
                AgentKind::Claude,
                AccountKind::ApiKey,
                "CLAUDE_CONFIG_DIR",
                "ANTHROPIC_API_KEY",
            ),
            (
                AgentKind::Claude,
                AccountKind::OauthToken,
                "CLAUDE_CONFIG_DIR",
                "CLAUDE_CODE_OAUTH_TOKEN",
            ),
            (
                AgentKind::Codex,
                AccountKind::ApiKey,
                "CODEX_HOME",
                "CODEX_API_KEY",
            ),
            (
                AgentKind::Gemini,
                AccountKind::ApiKey,
                "GEMINI_CLI_HOME",
                "GEMINI_API_KEY",
            ),
        ] {
            let name = format!("k-{}", kind.as_str());
            write_key(home.path(), agent, &name, "sk-xyz").unwrap();
            let dir = account_dir(home.path(), agent, &name);
            // A directory left with loose permissions is made private again.
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            let env = run_env(home.path(), agent, &name, kind).unwrap().unwrap();
            let get = |k: &str| env.set.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
            assert_eq!(
                get(dir_var),
                Some(dir.to_string_lossy().into_owned()),
                "{agent}"
            );
            assert_eq!(get(key_var).as_deref(), Some("sk-xyz"), "{agent}");
            assert_eq!(mode(&dir), 0o700, "{agent}");
            assert!(strips(&env, dir_var), "the owner's {dir_var} is dropped");
        }
        // Cursor has no directory variable: only the key.
        write_key(home.path(), AgentKind::Cursor, "c", "sk-c").unwrap();
        let env = run_env(home.path(), AgentKind::Cursor, "c", AccountKind::ApiKey)
            .unwrap()
            .unwrap();
        assert_eq!(env.set, [("CURSOR_API_KEY".to_owned(), "sk-c".to_owned())]);
    }

    #[test]
    fn gemini_accounts_pick_their_sign_in_and_skip_dotenv_files() {
        let home = tempfile::tempdir().unwrap();
        let settings = |name: &str| -> serde_json::Value {
            let path = account_dir(home.path(), AgentKind::Gemini, name)
                .join(".gemini")
                .join("settings.json");
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
        };
        write_key(home.path(), AgentKind::Gemini, "ci", "AIza-test").unwrap();
        run_env(home.path(), AgentKind::Gemini, "ci", AccountKind::ApiKey).unwrap();
        let s = settings("ci");
        assert_eq!(s["security"]["auth"]["selectedType"], "gemini-api-key");
        assert_eq!(s["advanced"]["ignoreLocalEnv"], true);

        // A login keeps the sign-in Gemini CLI chose and its other settings.
        let dir = create_dir(home.path(), AgentKind::Gemini, "work").unwrap();
        std::fs::create_dir_all(dir.join(".gemini")).unwrap();
        std::fs::write(
            dir.join(".gemini").join("settings.json"),
            r#"{"security":{"auth":{"selectedType":"oauth-personal"}},"ui":{"theme":"x"},"advanced":{"ignoreLocalEnv":false}}"#,
        )
        .unwrap();
        run_env(home.path(), AgentKind::Gemini, "work", AccountKind::Login).unwrap();
        let s = settings("work");
        assert_eq!(s["security"]["auth"]["selectedType"], "oauth-personal");
        assert_eq!(s["ui"]["theme"], "x");
        assert_eq!(s["advanced"]["ignoreLocalEnv"], true);

        // Settings Hashi cannot read refuse the run.
        std::fs::write(
            dir.join(".gemini").join("settings.json"),
            "{ // a comment\n \"ui\": {} }",
        )
        .unwrap();
        let err = run_env(home.path(), AgentKind::Gemini, "work", AccountKind::Login).unwrap_err();
        assert!(err.to_string().contains(".env"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn removing_an_account_never_follows_links() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("keep"), "x").unwrap();
        let dir = create_dir(home.path(), AgentKind::Claude, "work").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.join("link")).unwrap();
        assert!(remove_dir(home.path(), AgentKind::Claude, "work").unwrap());
        assert!(!dir.exists());
        assert!(outside.path().join("keep").exists());
        assert!(!remove_dir(home.path(), AgentKind::Claude, "work").unwrap());
        assert!(remove_dir(home.path(), AgentKind::Claude, "../x").is_err());
    }
}
