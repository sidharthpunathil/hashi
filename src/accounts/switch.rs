//! Pointing the plain `claude` and `codex` commands at one of Hashi's
//! accounts.
//!
//! `hashi switch claude work` makes every Claude Code that reads the
//! owner's normal login (the `claude` command in any terminal, the IDE
//! extensions) sign in as `work` from then on, with no alias and no
//! environment variable, so it works whichever program starts the CLI. The
//! Claude desktop app keeps its own sign-in and is not affected.
//!
//! How: the CLI's normal login lives in one place (Claude Code: the
//! `Claude Code-credentials` Keychain entry on macOS, or
//! `~/.claude/.credentials.json`, plus the `oauthAccount` identity in
//! `~/.claude.json`; Codex: `~/.codex/auth.json`). A switch
//!
//! 1. saves what is there now back to where it belongs (the account being
//!    left, or a private copy of the owner's original login, kept as the
//!    account `default`), unless that copy already holds a fresher token,
//! 2. copies the chosen account's saved login into the normal place,
//! 3. remembers the choice in `<home>/accounts/active.json`.
//!
//! The rest of the owner's Claude Code (settings, history, MCP servers,
//! projects) is untouched: only the login changes. `hashi switch claude
//! default` puts the original login back.
//!
//! Things to know: a Claude Code that is already running keeps the login it
//! started with until it refreshes or restarts; and while an account is
//! switched in, the `default` account of Hashi's own runs is whatever the
//! plain command now signs in as.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;
use serde::de::{Deserializer, MapAccess, Visitor};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};

use crate::accounts::{self as acct, DEFAULT};
use crate::agent::AgentKind;

/// File under `<home>/accounts/` that remembers which account each agent's
/// plain command is switched to.
pub const STATE_FILE: &str = "active.json";

/// Name of the directory (next to the accounts, never a valid account name)
/// holding the owner's original login while an account is switched in.
pub const ORIGINAL_DIR: &str = ".original";

/// Whether `agent` can be switched.
#[must_use]
pub const fn supports(agent: AgentKind) -> bool {
    matches!(agent, AgentKind::Claude | AgentKind::Codex)
}

/// A place secrets are kept by name: the macOS Keychain, or a fake in tests.
pub trait Vault {
    /// The secret stored under `service`, if any.
    ///
    /// # Errors
    ///
    /// When the store cannot be read.
    fn get(&self, service: &str) -> std::io::Result<Option<String>>;

    /// Stores `secret` under `service`, replacing any.
    ///
    /// # Errors
    ///
    /// When the store cannot be written.
    fn set(&self, service: &str, secret: &str) -> std::io::Result<()>;

    /// Removes what is stored under `service`; nothing there is fine.
    ///
    /// # Errors
    ///
    /// When the store cannot be written.
    fn delete(&self, service: &str) -> std::io::Result<()>;
}

/// The macOS Keychain, through the `security` tool Claude Code itself uses.
#[derive(Debug)]
pub struct Keychain {
    user: String,
}

impl Keychain {
    /// The Keychain for the current user.
    #[must_use]
    pub fn new() -> Self {
        let user = std::env::var("USER")
            .ok()
            .filter(|u| {
                !u.is_empty()
                    && u.chars()
                        .all(|c| c.is_ascii_graphic() && c != '"' && c != '\\')
            })
            .unwrap_or_else(|| "user".to_owned());
        Self { user }
    }
}

impl Keychain {
    fn delete_entry(service: &str) -> std::io::Result<()> {
        let out = Command::new("security")
            .args(["delete-generic-password", "-s", service])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if out.success() || out.code() == Some(44) {
            Ok(())
        } else {
            Err(std::io::Error::other(format!(
                "removing the Keychain entry {service} failed"
            )))
        }
    }
}

impl Default for Keychain {
    fn default() -> Self {
        Self::new()
    }
}

impl Vault for Keychain {
    fn get(&self, service: &str) -> std::io::Result<Option<String>> {
        let out = Command::new("security")
            .args(["find-generic-password", "-s", service, "-w"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()?;
        if !out.status.success() {
            // 44: the item does not exist.
            return if out.status.code() == Some(44) {
                Ok(None)
            } else {
                Err(std::io::Error::other(format!(
                    "reading the Keychain entry {service} failed"
                )))
            };
        }
        let text = String::from_utf8_lossy(&out.stdout)
            .trim_end_matches(['\n', '\r'])
            .to_owned();
        Ok(Some(text))
    }

    fn delete(&self, service: &str) -> std::io::Result<()> {
        Self::delete_entry(service)
    }

    fn set(&self, service: &str, secret: &str) -> std::io::Result<()> {
        if service.contains(['"', '\\', '\n']) {
            return Err(std::io::Error::other("bad Keychain service name"));
        }
        let hex = hex::encode(secret);
        let line = format!(
            "add-generic-password -U -a \"{}\" -s \"{service}\" -X \"{hex}\"\n",
            self.user
        );
        // `security -i` reads at most 4096 bytes a line; a longer secret goes
        // in the arguments instead, as Claude Code does.
        let status = if line.len() <= 4000 {
            let mut child = Command::new("security")
                .arg("-i")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(line.as_bytes())?;
            }
            child.wait()?
        } else {
            Command::new("security")
                .args([
                    "add-generic-password",
                    "-U",
                    "-a",
                    &self.user,
                    "-s",
                    service,
                    "-w",
                    secret,
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?
        };
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(format!(
                "writing the Keychain entry {service} failed"
            )))
        }
    }
}

/// Where the owner's normal logins are.
#[derive(Debug, Clone)]
pub struct Live {
    /// Claude Code's config folder, `~/.claude`.
    pub claude_dir: PathBuf,
    /// Claude Code's `~/.claude.json`, which holds the signed-in identity.
    pub claude_json: PathBuf,
    /// Codex's folder, `~/.codex`.
    pub codex_home: PathBuf,
    /// Set when the environment already points the CLI elsewhere, which
    /// makes the plain command impossible to switch.
    pub env_override: BTreeMap<AgentKind, String>,
}

impl Live {
    /// The current user's normal places.
    ///
    /// # Errors
    ///
    /// When the home folder is unknown.
    pub fn from_user() -> Result<Self, String> {
        let home = dirs::home_dir().ok_or("cannot find your home folder")?;
        let mut env_override = BTreeMap::new();
        for (agent, var) in [
            (AgentKind::Claude, "CLAUDE_CONFIG_DIR"),
            (AgentKind::Codex, "CODEX_HOME"),
        ] {
            if let Some(v) = std::env::var_os(var).filter(|v| !v.is_empty()) {
                env_override.insert(agent, format!("{var}={}", v.to_string_lossy()));
            }
        }
        Ok(Self {
            claude_dir: home.join(".claude"),
            claude_json: home.join(".claude.json"),
            codex_home: home.join(".codex"),
            env_override,
        })
    }
}

/// Everything a switch reads and writes.
#[derive(Debug)]
pub struct Ctx<'a> {
    /// The Hashi home.
    pub home: &'a Path,
    /// The owner's normal places.
    pub live: &'a Live,
    /// Where Claude Code keeps credentials on this system: `Some` for the
    /// macOS Keychain, `None` for `.credentials.json` files.
    pub vault: Option<&'a dyn Vault>,
}

impl std::fmt::Debug for dyn Vault + '_ {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Vault")
    }
}

/// A place a login is kept.
struct Slot {
    dir: PathBuf,
    live: bool,
}

/// What a switch did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The account the plain command was on.
    pub from: String,
    /// The account it is on now.
    pub to: String,
    /// Whether it already was `to`, so nothing changed.
    pub unchanged: bool,
}

fn state_path(home: &Path) -> PathBuf {
    acct::root(home).join(STATE_FILE)
}

fn load_state(home: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(state_path(home))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// The account `agent`'s plain command is switched to: `default` when no
/// switch is in place.
#[must_use]
pub fn active(home: &Path, agent: AgentKind) -> String {
    load_state(home)
        .remove(agent.as_str())
        .unwrap_or_else(|| DEFAULT.to_owned())
}

fn set_active(home: &Path, agent: AgentKind, name: &str) -> std::io::Result<()> {
    let mut state = load_state(home);
    if name == DEFAULT {
        state.remove(agent.as_str());
    } else {
        state.insert(agent.as_str().to_owned(), name.to_owned());
    }
    let text = serde_json::to_string_pretty(&state).map_err(std::io::Error::other)?;
    write_private(&state_path(home), &text)
}

/// Writes `text` to `path` through a temporary file, readable only by the
/// owner, keeping the file's existing mode when it has one.
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("hashi-{}", std::process::id()));
    let result = (|| {
        std::fs::write(&tmp, text)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path).map_or(0o600, |m| m.permissions().mode() & 0o777);
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
        }
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn slot_of(ctx: &Ctx<'_>, agent: AgentKind, name: &str) -> Slot {
    if name == DEFAULT {
        Slot {
            dir: acct::root(ctx.home).join(agent.as_str()).join(ORIGINAL_DIR),
            live: false,
        }
    } else {
        Slot {
            dir: acct::account_dir(ctx.home, agent, name),
            live: false,
        }
    }
}

fn live_slot(ctx: &Ctx<'_>, agent: AgentKind) -> Slot {
    Slot {
        dir: match agent {
            AgentKind::Codex => ctx.live.codex_home.clone(),
            _ => ctx.live.claude_dir.clone(),
        },
        live: true,
    }
}

/// The Keychain service Claude Code uses for a config folder.
#[must_use]
pub fn claude_service(dir: &Path, live: bool) -> String {
    if live {
        "Claude Code-credentials".to_owned()
    } else {
        let digest = Sha256::digest(dir.to_string_lossy().as_bytes());
        format!("Claude Code-credentials-{}", &hex::encode(digest)[..8])
    }
}

fn cred_file(agent: AgentKind, slot: &Slot) -> PathBuf {
    match agent {
        AgentKind::Codex => slot.dir.join("auth.json"),
        _ => slot.dir.join(".credentials.json"),
    }
}

fn read_creds(ctx: &Ctx<'_>, agent: AgentKind, slot: &Slot) -> std::io::Result<Option<String>> {
    if agent == AgentKind::Claude
        && let Some(vault) = ctx.vault
    {
        return vault.get(&claude_service(&slot.dir, slot.live));
    }
    match std::fs::read_to_string(cred_file(agent, slot)) {
        Ok(t) if t.trim().is_empty() => Ok(None),
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn write_creds(ctx: &Ctx<'_>, agent: AgentKind, slot: &Slot, secret: &str) -> std::io::Result<()> {
    if agent == AgentKind::Claude
        && let Some(vault) = ctx.vault
    {
        return vault.set(&claude_service(&slot.dir, slot.live), secret);
    }
    if !slot.live {
        ensure_private_dir(&slot.dir)?;
    }
    write_private(&cred_file(agent, slot), secret)
}

fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        if !dir.is_dir() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// Whether `saved` holds a login at least as new as `live`.
fn saved_is_fresher(agent: AgentKind, saved: &str, live: &str) -> bool {
    fn claude_expiry(s: &str) -> Option<i64> {
        serde_json::from_str::<serde_json::Value>(s)
            .ok()?
            .get("claudeAiOauth")?
            .get("expiresAt")?
            .as_i64()
    }
    fn codex_refresh(s: &str) -> Option<String> {
        serde_json::from_str::<serde_json::Value>(s)
            .ok()?
            .get("last_refresh")?
            .as_str()
            .map(str::to_owned)
    }
    match agent {
        AgentKind::Codex => match (codex_refresh(saved), codex_refresh(live)) {
            (Some(a), Some(b)) => a > b,
            _ => false,
        },
        _ => match (claude_expiry(saved), claude_expiry(live)) {
            (Some(a), Some(b)) => a > b,
            _ => false,
        },
    }
}

// The signed-in identity: the `oauthAccount` entry of `.claude.json`. The
// file is edited without reordering or reformatting anything else.

struct Ordered(Vec<(String, Box<RawValue>)>);

impl<'de> Deserialize<'de> for Ordered {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Ordered;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Ordered, A::Error> {
                let mut out = Vec::new();
                while let Some(entry) = map.next_entry::<String, Box<RawValue>>()? {
                    out.push(entry);
                }
                Ok(Ordered(out))
            }
        }
        d.deserialize_map(V)
    }
}

const IDENTITY_KEY: &str = "oauthAccount";

fn claude_json_of(ctx: &Ctx<'_>, slot: &Slot) -> PathBuf {
    if slot.live {
        ctx.live.claude_json.clone()
    } else {
        slot.dir.join(".claude.json")
    }
}

fn read_identity(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let Ordered(entries) = serde_json::from_str(&text).ok()?;
    entries
        .into_iter()
        .find(|(k, _)| k == IDENTITY_KEY)
        .map(|(_, v)| v.get().to_owned())
}

/// Sets (or, with `None`, removes) the identity in `path`, creating the
/// file when it is missing. A file that is not a JSON object is left alone.
fn write_identity(path: &Path, identity: Option<&str>) -> std::io::Result<()> {
    let mut entries = match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<Ordered>(&text) {
            Ok(Ordered(e)) => e,
            Err(_) => return Ok(()),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    entries.retain(|(k, _)| k != IDENTITY_KEY);
    if let Some(raw) = identity {
        let raw = RawValue::from_string(raw.to_owned()).map_err(std::io::Error::other)?;
        entries.push((IDENTITY_KEY.to_owned(), raw));
    }
    let mut out = String::from("{\n");
    for (i, (k, v)) in entries.iter().enumerate() {
        if i > 0 {
            out.push_str(",\n");
        }
        let key = serde_json::to_string(k).map_err(std::io::Error::other)?;
        out.push_str(&format!("  {key}: {}", v.get()));
    }
    out.push_str(if entries.is_empty() { "}\n" } else { "\n}\n" });
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_private(path, &out)
}

/// Switches `agent`'s plain command to account `target` (`default` for the
/// owner's original login). The caller has checked that `target` is a login
/// account Hashi knows.
///
/// # Errors
///
/// A message when the agent cannot be switched, the environment overrides
/// the plain command, the target has no saved login, or a file or the
/// Keychain cannot be read or written. Nothing is changed before the
/// target's login has been read.
pub fn switch(ctx: &Ctx<'_>, agent: AgentKind, target: &str) -> Result<Outcome, String> {
    if !supports(agent) {
        return Err(format!(
            "{} cannot be switched; only claude and codex can",
            agent.display_name()
        ));
    }
    if let Some(var) = ctx.live.env_override.get(&agent) {
        return Err(format!(
            "{var} is set in this shell, so the plain {agent} command does not use the normal \
             login and a switch could not change it. Unset it and run this again"
        ));
    }
    let from = active(ctx.home, agent);
    if from == target {
        return Ok(Outcome {
            from,
            to: target.to_owned(),
            unchanged: true,
        });
    }
    let io = |what: &str, e: std::io::Error| format!("{what}: {e}");
    let live = live_slot(ctx, agent);
    let tgt = slot_of(ctx, agent, target);
    let wanted = read_creds(ctx, agent, &tgt)
        .map_err(|e| io("reading the saved login", e))?
        .ok_or_else(|| {
            if target == DEFAULT {
                format!("no original {agent} login was saved, so there is nothing to go back to")
            } else {
                format!(
                    "{agent} account {target} is not signed in; run: hashi account login {agent} {target}"
                )
            }
        })?;
    let wanted_identity = (agent == AgentKind::Claude)
        .then(|| read_identity(&claude_json_of(ctx, &tgt)))
        .flatten();

    // Keep what the plain command holds now, where it belongs.
    let out = slot_of(ctx, agent, &from);
    if let Some(now) =
        read_creds(ctx, agent, &live).map_err(|e| io("reading the current login", e))?
    {
        let kept = read_creds(ctx, agent, &out).map_err(|e| io("reading the saved login", e))?;
        if !kept.is_some_and(|k| saved_is_fresher(agent, &k, &now)) {
            write_creds(ctx, agent, &out, &now).map_err(|e| io("saving the current login", e))?;
        }
        if agent == AgentKind::Claude {
            let identity = read_identity(&ctx.live.claude_json);
            write_identity(&claude_json_of(ctx, &out), identity.as_deref())
                .map_err(|e| io("saving the current identity", e))?;
        }
    }

    write_creds(ctx, agent, &live, &wanted).map_err(|e| io("switching the login", e))?;
    if agent == AgentKind::Claude {
        write_identity(&ctx.live.claude_json, wanted_identity.as_deref())
            .map_err(|e| io("switching the identity", e))?;
    }
    set_active(ctx.home, agent, target).map_err(|e| io("saving the choice", e))?;
    Ok(Outcome {
        from,
        to: target.to_owned(),
        unchanged: false,
    })
}

/// Saves the login the plain command holds into the active account's
/// place, so a token it refreshed is not lost. A no-op without a switch.
///
/// # Errors
///
/// A message when a file or the Keychain cannot be read or written.
pub fn save_back(ctx: &Ctx<'_>, agent: AgentKind) -> Result<(), String> {
    let from = active(ctx.home, agent);
    if !supports(agent) || from == DEFAULT {
        return Ok(());
    }
    let live = live_slot(ctx, agent);
    let out = slot_of(ctx, agent, &from);
    let io = |e: std::io::Error| e.to_string();
    if let Some(now) = read_creds(ctx, agent, &live).map_err(io)? {
        let kept = read_creds(ctx, agent, &out).map_err(io)?;
        if !kept.is_some_and(|k| saved_is_fresher(agent, &k, &now)) {
            write_creds(ctx, agent, &out, &now).map_err(io)?;
        }
    }
    Ok(())
}

/// The email the plain command is signed in as, when it can be read:
/// Claude Code's `oauthAccount` in `~/.claude.json`, or the email in
/// Codex's ID token. Reads only; nothing secret is returned.
#[must_use]
pub fn signed_in_as(live: &Live, agent: AgentKind) -> Option<String> {
    match agent {
        AgentKind::Claude => {
            let raw = read_identity(&live.claude_json)?;
            let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
            v.get("emailAddress")?.as_str().map(str::to_owned)
        }
        AgentKind::Codex => {
            let text = std::fs::read_to_string(live.codex_home.join("auth.json")).ok()?;
            let v: serde_json::Value = serde_json::from_str(&text).ok()?;
            let jwt = v.get("tokens")?.get("id_token")?.as_str()?;
            let payload = base64url(jwt.split('.').nth(1)?)?;
            let claims: serde_json::Value = serde_json::from_slice(&payload).ok()?;
            claims.get("email")?.as_str().map(str::to_owned)
        }
        _ => None,
    }
}

/// Decodes unpadded base64url, as in a JWT.
fn base64url(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes().filter(|&c| c != b'=') {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xff).ok()?);
        }
    }
    Some(out)
}

/// Who the plain command signs in as, in a line: the Hashi account (or
/// your own login) and its email when known.
#[must_use]
pub fn describe(home: &Path, live: Option<&Live>, agent: AgentKind) -> String {
    let name = active(home, agent);
    let who = if name == DEFAULT {
        "your own login".to_owned()
    } else {
        format!("the Hashi account {name}")
    };
    match live.and_then(|l| signed_in_as(l, agent)) {
        Some(email) => format!("plain {agent} signs in as {who} ({email})"),
        None => format!("plain {agent} signs in as {who}"),
    }
}

/// Who an agent's plain command signs in as, for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlainLogin {
    /// The agent.
    pub agent: AgentKind,
    /// The Hashi account switched in, or `default` for the owner's own.
    pub account: String,
    /// The signed-in email, when it can be read.
    pub email: Option<String>,
}

impl PlainLogin {
    /// Reads it for `agent`.
    #[must_use]
    pub fn read(home: &Path, live: Option<&Live>, agent: AgentKind) -> Self {
        Self {
            agent,
            account: active(home, agent),
            email: live.and_then(|l| signed_in_as(l, agent)),
        }
    }

    /// Whether a Hashi account is switched in.
    #[must_use]
    pub fn switched(&self) -> bool {
        self.account != DEFAULT
    }
}

/// What [`reset`] did for one agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reset {
    /// Nothing was switched and no copy was kept.
    AlreadyClean,
    /// The original login is back (it was switched to `from`).
    Restored {
        /// The account that was switched in.
        from: String,
    },
    /// There was no original login to put back, so the plain command is
    /// signed out again, as it was before the first switch.
    SignedOut {
        /// The account that was switched in.
        from: String,
    },
}

/// Puts `agent`'s plain command back exactly as it was before any switch
/// and deletes Hashi's copy of the original login. The account that was
/// switched in keeps its own login (any token refreshed meanwhile is saved
/// back first).
///
/// # Errors
///
/// A message when a file or the Keychain cannot be read or written.
pub fn reset(ctx: &Ctx<'_>, agent: AgentKind) -> Result<Reset, String> {
    if !supports(agent) {
        return Ok(Reset::AlreadyClean);
    }
    let from = active(ctx.home, agent);
    let stash = slot_of(ctx, agent, DEFAULT);
    let io = |what: &str, e: std::io::Error| format!("{what}: {e}");
    let had_copy = read_creds(ctx, agent, &stash)
        .map_err(|e| io("reading the original login", e))?
        .is_some();
    let done = if from == DEFAULT {
        Reset::AlreadyClean
    } else if had_copy {
        switch(ctx, agent, DEFAULT)?;
        Reset::Restored { from }
    } else {
        // The plain command had no login before the first switch.
        if ctx.live.env_override.contains_key(&agent) {
            return Err(format!(
                "the environment points {agent} elsewhere; unset it and run this again"
            ));
        }
        save_back(ctx, agent)?;
        let live = live_slot(ctx, agent);
        if agent == AgentKind::Claude
            && let Some(vault) = ctx.vault
        {
            vault
                .delete(&claude_service(&live.dir, true))
                .map_err(|e| io("signing out", e))?;
        } else {
            match std::fs::remove_file(cred_file(agent, &live)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io("signing out", e)),
            }
        }
        if agent == AgentKind::Claude {
            write_identity(&ctx.live.claude_json, None)
                .map_err(|e| io("clearing the identity", e))?;
        }
        set_active(ctx.home, agent, DEFAULT).map_err(|e| io("saving the choice", e))?;
        Reset::SignedOut { from }
    };
    // Hashi's copy of the original login is no longer needed.
    if agent == AgentKind::Claude
        && let Some(vault) = ctx.vault
    {
        vault
            .delete(&claude_service(&stash.dir, false))
            .map_err(|e| io("removing the saved copy", e))?;
    }
    match std::fs::remove_dir_all(&stash.dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(io("removing the saved copy", e)),
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Mem(RefCell<HashMap<String, String>>);

    impl Vault for Mem {
        fn get(&self, s: &str) -> std::io::Result<Option<String>> {
            Ok(self.0.borrow().get(s).cloned())
        }
        fn set(&self, s: &str, v: &str) -> std::io::Result<()> {
            self.0.borrow_mut().insert(s.to_owned(), v.to_owned());
            Ok(())
        }
        fn delete(&self, s: &str) -> std::io::Result<()> {
            self.0.borrow_mut().remove(s);
            Ok(())
        }
    }

    struct Rig {
        _tmp: tempfile::TempDir,
        home: PathBuf,
        live: Live,
    }

    fn rig() -> Rig {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let home = root.join("hashi");
        std::fs::create_dir_all(&home).unwrap();
        let live = Live {
            claude_dir: root.join("u/.claude"),
            claude_json: root.join("u/.claude.json"),
            codex_home: root.join("u/.codex"),
            env_override: BTreeMap::new(),
        };
        std::fs::create_dir_all(&live.claude_dir).unwrap();
        std::fs::create_dir_all(&live.codex_home).unwrap();
        Rig {
            _tmp: tmp,
            home,
            live,
        }
    }

    fn creds(token: &str, expires: i64) -> String {
        format!(r#"{{"claudeAiOauth":{{"accessToken":"{token}","expiresAt":{expires}}}}}"#)
    }

    fn account(rig: &Rig, vault: Option<&Mem>, name: &str, token: &str, email: &str) {
        let ctx = Ctx {
            home: &rig.home,
            live: &rig.live,
            vault: vault.map(|v| v as &dyn Vault),
        };
        let slot = slot_of(&ctx, AgentKind::Claude, name);
        write_creds(&ctx, AgentKind::Claude, &slot, &creds(token, 100)).unwrap();
        write_identity(
            &claude_json_of(&ctx, &slot),
            Some(&format!(r#"{{"emailAddress":"{email}"}}"#)),
        )
        .unwrap();
    }

    fn live_token(rig: &Rig, vault: Option<&Mem>) -> String {
        let ctx = Ctx {
            home: &rig.home,
            live: &rig.live,
            vault: vault.map(|v| v as &dyn Vault),
        };
        read_creds(&ctx, AgentKind::Claude, &live_slot(&ctx, AgentKind::Claude))
            .unwrap()
            .unwrap_or_default()
    }

    #[test]
    fn the_keychain_service_matches_claude_codes() {
        // Checked against the entries Claude Code made for these folders.
        assert_eq!(
            claude_service(
                Path::new("/Users/sidharthp/.hashi/accounts/claude/rajat"),
                false
            ),
            "Claude Code-credentials-f4e91b83"
        );
        assert_eq!(
            claude_service(Path::new("/x"), true),
            "Claude Code-credentials"
        );
    }

    #[test]
    fn switches_and_back_with_files_and_keeps_the_rest_of_claude_json() {
        let rig = rig();
        let ctx = Ctx {
            home: &rig.home,
            live: &rig.live,
            vault: None,
        };
        std::fs::write(
            rig.live.claude_dir.join(".credentials.json"),
            creds("original", 100),
        )
        .unwrap();
        std::fs::write(
            &rig.live.claude_json,
            "{\n  \"zeta\": 1,\n  \"oauthAccount\": {\"emailAddress\":\"me@x\"},\n  \"alpha\": [1, 2]\n}",
        )
        .unwrap();
        account(&rig, None, "work", "work-token", "work@x");

        let o = switch(&ctx, AgentKind::Claude, "work").unwrap();
        assert_eq!(
            (o.from.as_str(), o.to.as_str(), o.unchanged),
            ("default", "work", false)
        );
        assert!(live_token(&rig, None).contains("work-token"));
        let json = std::fs::read_to_string(&rig.live.claude_json).unwrap();
        assert!(json.contains("work@x") && !json.contains("me@x"), "{json}");
        assert!(
            json.find("zeta").unwrap() < json.find("alpha").unwrap(),
            "order kept: {json}"
        );
        assert_eq!(active(&rig.home, AgentKind::Claude), "work");

        // Same again does nothing.
        assert!(switch(&ctx, AgentKind::Claude, "work").unwrap().unchanged);

        // Back to the original, which comes back exactly.
        switch(&ctx, AgentKind::Claude, "default").unwrap();
        assert!(live_token(&rig, None).contains("original"));
        assert!(
            std::fs::read_to_string(&rig.live.claude_json)
                .unwrap()
                .contains("me@x")
        );
        assert_eq!(active(&rig.home, AgentKind::Claude), "default");
    }

    #[test]
    fn a_token_refreshed_while_switched_in_is_saved_back() {
        let rig = rig();
        let vault = Mem::default();
        let ctx = Ctx {
            home: &rig.home,
            live: &rig.live,
            vault: Some(&vault),
        };
        let live = live_slot(&ctx, AgentKind::Claude);
        write_creds(&ctx, AgentKind::Claude, &live, &creds("orig", 100)).unwrap();
        account(&rig, Some(&vault), "work", "w1", "w@x");
        switch(&ctx, AgentKind::Claude, "work").unwrap();
        // Claude Code refreshes the live token.
        write_creds(&ctx, AgentKind::Claude, &live, &creds("w2", 500)).unwrap();
        switch(&ctx, AgentKind::Claude, "default").unwrap();
        switch(&ctx, AgentKind::Claude, "work").unwrap();
        assert!(live_token(&rig, Some(&vault)).contains("w2"));
    }

    #[test]
    fn a_fresher_saved_copy_is_not_overwritten_by_a_stale_live_one() {
        let rig = rig();
        let ctx = Ctx {
            home: &rig.home,
            live: &rig.live,
            vault: None,
        };
        let live = live_slot(&ctx, AgentKind::Claude);
        write_creds(&ctx, AgentKind::Claude, &live, &creds("orig", 100)).unwrap();
        account(&rig, None, "work", "w1", "w@x");
        switch(&ctx, AgentKind::Claude, "work").unwrap();
        // A Hashi run refreshed the account's own copy meanwhile.
        let slot = slot_of(&ctx, AgentKind::Claude, "work");
        write_creds(&ctx, AgentKind::Claude, &slot, &creds("w-fresh", 900)).unwrap();
        switch(&ctx, AgentKind::Claude, "default").unwrap();
        switch(&ctx, AgentKind::Claude, "work").unwrap();
        assert!(live_token(&rig, None).contains("w-fresh"));
    }

    #[test]
    fn refuses_without_changing_anything() {
        let rig = rig();
        let ctx = Ctx {
            home: &rig.home,
            live: &rig.live,
            vault: None,
        };
        std::fs::write(
            rig.live.claude_dir.join(".credentials.json"),
            creds("orig", 1),
        )
        .unwrap();
        let e = switch(&ctx, AgentKind::Claude, "nobody").unwrap_err();
        assert!(e.contains("not signed in"), "{e}");
        assert!(
            switch(&ctx, AgentKind::Claude, "default")
                .unwrap()
                .unchanged
        );
        assert!(live_token(&rig, None).contains("orig"));
        assert!(switch(&ctx, AgentKind::Gemini, "x").is_err());
        let mut live = rig.live.clone();
        live.env_override
            .insert(AgentKind::Claude, "CLAUDE_CONFIG_DIR=/x".to_owned());
        let ctx = Ctx { live: &live, ..ctx };
        assert!(
            switch(&ctx, AgentKind::Claude, "x")
                .unwrap_err()
                .contains("CLAUDE_CONFIG_DIR")
        );
    }

    #[test]
    fn codex_swaps_auth_json() {
        let rig = rig();
        let ctx = Ctx {
            home: &rig.home,
            live: &rig.live,
            vault: None,
        };
        std::fs::write(rig.live.codex_home.join("auth.json"), r#"{"k":"orig"}"#).unwrap();
        let dir = acct::account_dir(&rig.home, AgentKind::Codex, "alt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("auth.json"), r#"{"k":"alt"}"#).unwrap();
        switch(&ctx, AgentKind::Codex, "alt").unwrap();
        assert!(
            std::fs::read_to_string(rig.live.codex_home.join("auth.json"))
                .unwrap()
                .contains("alt")
        );
        switch(&ctx, AgentKind::Codex, "default").unwrap();
        assert!(
            std::fs::read_to_string(rig.live.codex_home.join("auth.json"))
                .unwrap()
                .contains("orig")
        );
    }

    /// Real Keychain round trip on a throwaway entry. Run by hand on macOS:
    /// `cargo test --lib keychain_round_trip -- --ignored`.
    #[test]
    #[ignore = "touches the real macOS Keychain"]
    fn keychain_round_trip() {
        let k = Keychain::new();
        let service = format!("hashi-test-{}", std::process::id());
        assert_eq!(k.get(&service).unwrap(), None);
        let short = creds("t1", 1);
        k.set(&service, &short).unwrap();
        assert_eq!(k.get(&service).unwrap().as_deref(), Some(short.as_str()));
        let long = format!(r#"{{"pad":"{}"}}"#, "x".repeat(5000));
        k.set(&service, &long).unwrap();
        assert_eq!(k.get(&service).unwrap().as_deref(), Some(long.as_str()));
        k.delete(&service).unwrap();
        assert_eq!(k.get(&service).unwrap(), None);
        k.delete(&service).unwrap();
    }

    #[test]
    fn reset_puts_everything_back_and_removes_the_copy() {
        let rig = rig();
        let vault = Mem::default();
        let ctx = Ctx {
            home: &rig.home,
            live: &rig.live,
            vault: Some(&vault),
        };
        let live = live_slot(&ctx, AgentKind::Claude);
        write_creds(&ctx, AgentKind::Claude, &live, &creds("orig", 100)).unwrap();
        std::fs::write(
            &rig.live.claude_json,
            r#"{"oauthAccount":{"emailAddress":"me@x"}}"#,
        )
        .unwrap();
        account(&rig, Some(&vault), "work", "w1", "w@x");
        assert_eq!(reset(&ctx, AgentKind::Claude).unwrap(), Reset::AlreadyClean);
        switch(&ctx, AgentKind::Claude, "work").unwrap();
        assert_eq!(
            signed_in_as(&rig.live, AgentKind::Claude).as_deref(),
            Some("w@x")
        );
        assert!(describe(&rig.home, Some(&rig.live), AgentKind::Claude).contains("work (w@x)"));
        assert_eq!(
            reset(&ctx, AgentKind::Claude).unwrap(),
            Reset::Restored {
                from: "work".to_owned()
            }
        );
        assert!(live_token(&rig, Some(&vault)).contains("orig"));
        assert_eq!(
            signed_in_as(&rig.live, AgentKind::Claude).as_deref(),
            Some("me@x")
        );
        let stash = slot_of(&ctx, AgentKind::Claude, DEFAULT);
        assert!(!stash.dir.exists());
        assert!(
            vault
                .get(&claude_service(&stash.dir, false))
                .unwrap()
                .is_none()
        );
        // The account keeps its own login.
        let slot = slot_of(&ctx, AgentKind::Claude, "work");
        assert!(
            read_creds(&ctx, AgentKind::Claude, &slot)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn reset_signs_out_when_there_was_no_login_before() {
        let rig = rig();
        let ctx = Ctx {
            home: &rig.home,
            live: &rig.live,
            vault: None,
        };
        account(&rig, None, "work", "w1", "w@x");
        switch(&ctx, AgentKind::Claude, "work").unwrap();
        assert_eq!(
            reset(&ctx, AgentKind::Claude).unwrap(),
            Reset::SignedOut {
                from: "work".to_owned()
            }
        );
        assert!(live_token(&rig, None).is_empty());
        assert_eq!(active(&rig.home, AgentKind::Claude), DEFAULT);
    }

    #[test]
    fn reads_the_codex_email_from_its_id_token() {
        let rig = rig();
        // {"email":"c@x"}
        let jwt = "e30.eyJlbWFpbCI6ImNAeCJ9.sig";
        std::fs::write(
            rig.live.codex_home.join("auth.json"),
            format!(r#"{{"tokens":{{"id_token":"{jwt}"}}}}"#),
        )
        .unwrap();
        assert_eq!(
            signed_in_as(&rig.live, AgentKind::Codex).as_deref(),
            Some("c@x")
        );
    }
}
