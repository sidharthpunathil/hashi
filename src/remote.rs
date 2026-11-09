//! Bridges you were invited to: `hashi connect`, `hashi remote` and
//! `hashi ask --remote`.
//!
//! A friend never runs a server. `hashi connect` redeems their invite link and
//! keeps the token in `<home>/remotes.json`, readable only by them, so
//! `hashi ask --remote` can send prompts to the owner's bridge.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::diagnostics::{neutralize_controls, terminal_safe, write_private};
use crate::store::INVITE_PREFIX;
use crate::token::TOKEN_PREFIX;

/// File name of the saved remotes, inside the Hashi home.
pub const REMOTES_FILE: &str = "remotes.json";
/// Seconds each long poll asks the server to wait.
pub const WAIT_SECS: u64 = 60;
/// Longest remote name.
pub const MAX_NAME_LEN: usize = 40;

// ── invite links ────────────────────────────────────────────────────────────

/// A checked invite link such as `https://host/claim/hashi_inv_<hex>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteLink {
    /// `scheme://host[:port]`, with a lowercase host.
    pub base_url: String,
    /// The host, without port or brackets.
    pub host: String,
    /// The invite code, `hashi_inv_<hex>`.
    pub code: String,
}

/// Whether `host` (without brackets or port) is a loopback name or address.
#[must_use]
pub fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn plain_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        && !host.starts_with(['.', '-'])
        && !host.ends_with(['.', '-'])
}

fn invite_code(code: &str) -> bool {
    code.strip_prefix(INVITE_PREFIX).is_some_and(|hex| {
        (16..=118).contains(&hex.len())
            && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// Parses and checks an invite link. Only `https`, or `http` for a loopback
/// host, is accepted; there must be no user info, query or fragment; and the
/// path must be exactly `/claim/hashi_inv_<hex>`.
///
/// # Errors
///
/// Explains what is wrong with the link.
pub fn parse_invite_url(input: &str) -> Result<InviteLink> {
    let (base_url, host, code) = parse_link_url(input, "/claim/", "an invite link")?;
    if !invite_code(&code) {
        bail!("this is not an invite link: the code must be {INVITE_PREFIX} and hex digits");
    }
    Ok(InviteLink {
        base_url,
        host,
        code,
    })
}

/// [`parse_bridge_url`] for a link someone shares: it may also use plain
/// `http://` on a private network (192.168.x.x, 10.x.x.x, `.local`), for
/// sharing on one wifi.
///
/// # Errors
///
/// Explains what is wrong with the link.
pub fn parse_link_url(input: &str, path: &str, what: &str) -> Result<(String, String, String)> {
    let input = input.trim();
    let lan = input.strip_prefix("http://").and_then(|rest| {
        let authority = rest.split('/').next().unwrap_or_default();
        let host = match authority.rsplit_once(':') {
            Some((h, p)) if p.bytes().all(|b| b.is_ascii_digit()) => h,
            _ => authority,
        };
        crate::pool::is_lan_host(host).then(|| format!("https://{rest}"))
    });
    match lan {
        Some(as_https) => {
            let (b, h, c) = parse_bridge_url(&as_https, path, what)?;
            Ok((b.replacen("https://", "http://", 1), h, c))
        }
        None => parse_bridge_url(input, path, what),
    }
}

/// Splits `scheme://host[:port]<path><code>` into its base URL, host and
/// code, with the checks [`parse_invite_url`] describes. `what` names the
/// kind of link in messages. The code itself is checked by the caller.
///
/// # Errors
///
/// Explains what is wrong with the link.
pub fn parse_bridge_url(input: &str, path: &str, what: &str) -> Result<(String, String, String)> {
    let s = input.trim();
    let (scheme, rest) = if let Some(r) = s.strip_prefix("https://") {
        ("https", r)
    } else if let Some(r) = s.strip_prefix("http://") {
        ("http", r)
    } else {
        bail!("{what} starts with https://");
    };
    let slash = rest
        .find('/')
        .with_context(|| format!("the link has no {path} path; paste the whole link"))?;
    let (authority, rest_path) = rest.split_at(slash);
    if authority.contains('@') {
        bail!("the link must not contain a user name or password");
    }
    let (host, port) = if let Some(inner) = authority.strip_prefix('[') {
        let (ip, after) = inner
            .split_once(']')
            .context("the link's host is not valid")?;
        if ip.parse::<std::net::Ipv6Addr>().is_err() {
            bail!("the link's host is not valid");
        }
        (ip.to_owned(), after)
    } else {
        let (h, after) = match authority.find(':') {
            Some(i) => authority.split_at(i),
            None => (authority, ""),
        };
        if !plain_host(h) {
            bail!("the link's host is not valid");
        }
        (h.to_ascii_lowercase(), after)
    };
    if !port.is_empty() {
        let digits = port.strip_prefix(':').unwrap_or("x");
        let ok = !digits.is_empty()
            && digits.len() <= 5
            && digits.bytes().all(|b| b.is_ascii_digit())
            && digits.parse::<u16>().is_ok_and(|p| p > 0);
        if !ok {
            bail!("the link's port is not valid");
        }
    }
    if scheme == "http" && !is_loopback_host(&host) {
        bail!("refusing a plain http:// link for {host}; {what} must use https://");
    }
    let code = rest_path
        .strip_prefix(path)
        .with_context(|| format!("this is not {what}: the path must be {path}<code>"))?;
    let shown_host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.clone()
    };
    Ok((
        format!("{scheme}://{shown_host}{port}"),
        host,
        code.to_owned(),
    ))
}

// ── names and quoting ───────────────────────────────────────────────────────

/// Whether `name` is a usable remote name: 1 to 40 of `a-z`, `0-9`, `-` and
/// `_`, starting with a letter or digit. These are also valid MCP server
/// names for `claude mcp add`.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    name.len() <= MAX_NAME_LEN
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// `s` as a remote name: lowercase letters and digits, with runs of anything
/// else turned into one `-`. Empty when nothing usable is left.
#[must_use]
pub fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.truncate(MAX_NAME_LEN);
    out.trim_matches('-').to_owned()
}

/// `s` quoted for a POSIX shell: wrapped in single quotes, with each `'`
/// written as `'\''`. Nothing inside is expanded.
#[must_use]
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn valid_token(t: &str) -> bool {
    t.strip_prefix(TOKEN_PREFIX).is_some_and(|hex| {
        (16..=128).contains(&hex.len()) && hex.bytes().all(|b| b.is_ascii_alphanumeric())
    })
}

// ── remotes.json ────────────────────────────────────────────────────────────

/// One saved bridge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Remote {
    /// Local name, see [`valid_name`].
    pub name: String,
    /// `scheme://host[:port]` of the bridge.
    pub base_url: String,
    /// The bearer token.
    pub token: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RemotesFile {
    remotes: Vec<Remote>,
}

/// Path of `remotes.json` in `home`.
#[must_use]
pub fn remotes_path(home: &Path) -> PathBuf {
    home.join(REMOTES_FILE)
}

/// Reads the saved remotes. A missing file means none.
///
/// # Errors
///
/// Fails if the file cannot be read or parsed.
pub fn load(home: &Path) -> Result<Vec<Remote>> {
    let path = remotes_path(home);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let file: RemotesFile =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(file.remotes)
}

/// Writes the remotes atomically: a new owner-only file next to it, then a
/// rename over the old one. The home is created owner only first.
///
/// # Errors
///
/// Fails if the home cannot be prepared or the file cannot be written.
pub fn save(home: &Path, remotes: &[Remote]) -> Result<()> {
    crate::server::prepare_home(home)?;
    let path = remotes_path(home);
    let tmp = home.join(format!(".{REMOTES_FILE}.{}.tmp", std::process::id()));
    let text = serde_json::to_string_pretty(&RemotesFile {
        remotes: remotes.to_vec(),
    })?;
    let written = write_private(&tmp, &format!("{text}\n"))
        .with_context(|| format!("writing {}", tmp.display()))
        .and_then(|()| {
            std::fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))
        });
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// The remote called `name`, or the only one saved when `name` is `None`.
///
/// # Errors
///
/// Fails when there is no such remote, none at all, or several and no name.
pub fn pick<'a>(remotes: &'a [Remote], name: Option<&str>) -> Result<&'a Remote> {
    match name {
        Some(n) => remotes.iter().find(|r| r.name == n).with_context(|| {
            format!(
                "no remote called {}; `hashi remote list` shows them",
                terminal_safe(n)
            )
        }),
        None => match remotes {
            [] => bail!("no remotes saved; connect one with `hashi connect <invite link>`"),
            [one] => Ok(one),
            _ => bail!(
                "several remotes are saved, name one: `hashi ask --remote <name> \"...\"` \
                 (`hashi remote list` shows them)"
            ),
        },
    }
}

// ── HTTP ────────────────────────────────────────────────────────────────────

pub(crate) fn http() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        // Never carry the token, or the invite code, to another host.
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(WAIT_SECS + 30)))
        .user_agent(concat!("hashi/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

fn read(
    res: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    url: &str,
) -> Result<(u16, Value)> {
    let mut res = res.with_context(|| format!("could not reach {}", terminal_safe(url)))?;
    let status = res.status().as_u16();
    let body = res.body_mut().read_json::<Value>().unwrap_or(Value::Null);
    Ok((status, body))
}

pub(crate) fn post(
    agent: &ureq::Agent,
    url: &str,
    token: Option<&str>,
    body: &Value,
) -> Result<(u16, Value)> {
    let mut req = agent.post(url);
    if let Some(t) = token {
        req = req.header("Authorization", &format!("Bearer {t}"));
    }
    read(req.send_json(body), url)
}

fn get(agent: &ureq::Agent, url: &str, token: &str) -> Result<(u16, Value)> {
    let req = agent
        .get(url)
        .header("Authorization", &format!("Bearer {token}"));
    read(req.call(), url)
}

/// The server's error message, made safe to print.
pub(crate) fn error_text(status: u16, body: &Value) -> String {
    let msg = body["error"]["message"]
        .as_str()
        .map_or_else(|| format!("request failed ({status})"), str::to_owned);
    terminal_safe(&msg)
}

// ── connect ─────────────────────────────────────────────────────────────────

/// What `hashi connect` should do.
#[derive(Debug, Clone)]
pub struct ConnectOptions {
    /// The invite link.
    pub url: String,
    /// Name to save it under; defaults to the server's name.
    pub name: Option<String>,
    /// Run `claude mcp add` with this Claude Code executable.
    pub setup_claude: Option<PathBuf>,
}

/// The `claude mcp add` arguments, the same as the claim page's snippet.
#[must_use]
pub fn claude_mcp_args(name: &str, base_url: &str, token: &str) -> Vec<String> {
    vec![
        "mcp".into(),
        "add".into(),
        "--transport".into(),
        "http".into(),
        name.into(),
        format!("{base_url}/mcp"),
        "--header".into(),
        format!("Authorization: Bearer {token}"),
    ]
}

fn free_name(remotes: &[Remote], wanted: &str) -> String {
    let taken = |n: &str| remotes.iter().any(|r| r.name == n);
    if !taken(wanted) {
        return wanted.to_owned();
    }
    let stem: String = wanted.chars().take(MAX_NAME_LEN - 4).collect();
    (2..1000)
        .map(|i| format!("{stem}-{i}"))
        .find(|n| !taken(n))
        .unwrap_or_else(|| format!("{stem}-x"))
}

/// Redeems an invite link, saves the token and prints how to use it.
///
/// # Errors
///
/// Fails on a bad link, a refused claim, or when saving fails.
pub fn connect(home: &Path, opts: &ConnectOptions) -> Result<Remote> {
    let r = redeem(home, &opts.url, opts.name.as_deref())?;
    if let Some(login) = &r.login {
        print_login(&r.remote, &r.server, login);
    } else {
        print_connected(home, &r.remote, &r.server, opts.setup_claude.as_deref());
    }
    Ok(r.remote)
}

/// What redeeming an invite link saved.
#[derive(Debug, Clone)]
pub struct Redemption {
    /// The saved bridge.
    pub remote: Remote,
    /// The server's name, as it gave it.
    pub server: String,
    /// For a login share: what happened to the key.
    pub login: Option<LoginAdded>,
}

/// Where a login share's key went.
#[derive(Debug, Clone)]
pub enum LoginAdded {
    /// Added as the account `<agent>:<name>`.
    Account {
        /// The agent.
        agent: crate::agent::AgentKind,
        /// The account's name.
        name: String,
        /// Remind at this percent.
        remind_at: Option<u8>,
        /// When the share ends, Unix ms.
        until: Option<i64>,
    },
    /// Could not be added; the key was kept in this owner-only file, with
    /// the reason.
    Saved {
        /// The agent.
        agent: crate::agent::AgentKind,
        /// The file holding the key.
        path: PathBuf,
        /// Why it was not added.
        why: String,
    },
}

/// Adds a login share's key as an account, or keeps it in an owner-only
/// file when that fails: the link is already used, so the key must not be
/// lost.
fn add_login(home: &Path, remote: &str, v: &Value) -> Result<LoginAdded> {
    let grant = crate::share::LoginGrant::from_json(v).map_err(|e| anyhow::anyhow!(e))?;
    let added = crate::server::open_store(home)
        .and_then(|store| crate::share::add_shared_login(&store, home, &grant, remote, "connect"));
    match added {
        Ok(name) => Ok(LoginAdded::Account {
            agent: grant.agent,
            name,
            remind_at: grant.remind_at,
            until: grant.until,
        }),
        Err(e) => {
            let path = home.join(format!("shared-login-{remote}.key"));
            write_private(&path, &format!("{}\n", grant.key.as_str()))
                .with_context(|| format!("saving the shared key to {}", path.display()))?;
            Ok(LoginAdded::Saved {
                agent: grant.agent,
                path,
                why: format!("{e:#}"),
            })
        }
    }
}

/// What `hashi connect` prints for a login share.
fn print_login(remote: &Remote, server: &str, login: &LoginAdded) {
    let server = terminal_safe(server);
    println!();
    match login {
        LoginAdded::Account {
            agent,
            name,
            remind_at,
            until,
        } => {
            println!(
                "  \u{2713} {server} shared their {} login, added as account {agent}:{name}",
                agent.display_name()
            );
            if let Some(p) = remind_at {
                println!("    they asked you to stop at {p}% of their quota; Hashi reminds you");
            }
            if let Some(t) = until.and_then(crate::util::rfc3339) {
                println!("    the share ends {t}; then the account is switched off");
            }
            println!(
                "    their usage is read through \"{}\": hashi account usage",
                remote.name
            );
            println!();
            println!("  Use it from a token or project:");
            println!("    hashi token update <id> --account {agent}={name}");
        }
        LoginAdded::Saved { agent, path, why } => {
            println!(
                "  ! {server} shared their {} login, but it could not be added: {}",
                agent.display_name(),
                terminal_safe(why)
            );
            println!(
                "    the key is kept in {} (only you can read it)",
                path.display()
            );
            println!(
                "    add it with: hashi account add {agent} {} --kind oauth-token --key-stdin < {}",
                remote.name.replace('_', "-"),
                path.display()
            );
        }
    }
    println!();
}

/// Redeems an invite link and saves the token under `name` (default: from
/// the server's name), printing nothing. Returns the saved remote and the
/// server's name as it gave it. `hashi connect` and the dashboard both use
/// this.
///
/// # Errors
///
/// Fails on a bad link, a refused claim, or when saving fails.
pub fn redeem(home: &Path, url: &str, name: Option<&str>) -> Result<Redemption> {
    let link = parse_invite_url(url)?;
    let mut remotes = load(home)?;
    if let Some(n) = name {
        if !valid_name(n) {
            bail!(
                "--name must be 1 to {MAX_NAME_LEN} lowercase letters, digits, - or _, \
                 starting with a letter or digit"
            );
        }
        if remotes.iter().any(|r| r.name == n) {
            bail!(
                "a remote called {n} is already saved; pick another --name or run \
                 `hashi remote remove {n}` first"
            );
        }
    }

    let agent = http();
    let body = json!({ "code": link.code });
    let (status, preview) = post(
        &agent,
        &format!("{}/v1/claim/preview", link.base_url),
        None,
        &body,
    )?;
    if status != 200 {
        bail!("{}", error_text(status, &preview));
    }
    let name = match name {
        Some(n) => n.to_owned(),
        None => {
            let from_server = slug(preview["server"].as_str().unwrap_or_default());
            let base = [from_server, slug(&link.host)]
                .into_iter()
                .find(|s| valid_name(s))
                .unwrap_or_else(|| "hashi".to_owned());
            free_name(&remotes, &base)
        }
    };

    let (status, r) = post(
        &agent,
        &format!("{}/v1/claim/redeem", link.base_url),
        None,
        &body,
    )?;
    if status != 201 && status != 200 {
        bail!("{}", error_text(status, &r));
    }
    let token = r["token"].as_str().unwrap_or_default();
    if !valid_token(token) {
        bail!("the server answered without a usable token");
    }
    // The link the friend used is known to reach the bridge; a base URL in
    // the answer could point the token somewhere else, so it is not used.
    let remote = Remote {
        name,
        base_url: link.base_url,
        token: token.to_owned(),
    };
    remotes.push(remote.clone());
    save(home, &remotes)?;
    let server = r["server"].as_str().unwrap_or("the bridge").to_owned();
    let login = match r.get("login").filter(|v| v.is_object()) {
        Some(v) => Some(add_login(home, &remote.name, v)?),
        None => None,
    };
    Ok(Redemption {
        remote,
        server,
        login,
    })
}

/// What `hashi connect` prints once a bridge is saved: where the token is
/// and how to use it from Claude Code, Codex and this terminal.
fn print_connected(home: &Path, remote: &Remote, server: &str, setup_claude: Option<&Path>) {
    let server = terminal_safe(server);
    let path = remotes_path(home);
    println!();
    println!(
        "  \u{2713} connected to {server}, saved as \"{}\"",
        remote.name
    );
    println!(
        "    token stored in {} (only you can read it)",
        path.display()
    );
    println!();
    let base = &remote.base_url;
    let quoted_header = shell_quote(&format!("Authorization: Bearer {}", remote.token));
    if let Some(bin) = setup_claude {
        let status = std::process::Command::new(bin)
            .args(claude_mcp_args(&remote.name, base, &remote.token))
            .stdin(std::process::Stdio::null())
            .status();
        match status {
            Ok(s) if s.success() => {
                println!(
                    "  \u{2713} added to Claude Code as MCP server \"{}\"",
                    remote.name
                );
            }
            Ok(s) => eprintln!(
                "  ! claude mcp add failed ({s}); run it yourself:\n    claude mcp add --transport http {} {base}/mcp --header {quoted_header}",
                remote.name
            ),
            Err(e) => eprintln!(
                "  ! could not run {} ({e}); is Claude Code installed? Run it yourself:\n    claude mcp add --transport http {} {base}/mcp --header {quoted_header}",
                bin.display(),
                remote.name
            ),
        }
    } else {
        println!("  Claude Code:");
        println!(
            "    claude mcp add --transport http {} {base}/mcp --header {quoted_header}",
            remote.name
        );
    }
    println!();
    println!("  Codex:");
    println!("    export HASHI_TOKEN={}", shell_quote(&remote.token));
    println!(
        "    codex mcp add {} --url {base}/mcp --bearer-token-env-var HASHI_TOKEN",
        remote.name
    );
    println!();
    println!("  Ask from this terminal:");
    println!("    hashi ask --remote {} \"hello\"", remote.name);
    println!();
    println!("  Your limits and usage: {base}/me");
    println!();
}

/// Prints the saved remotes' names and base URLs. Tokens are never shown.
///
/// # Errors
///
/// Fails if `remotes.json` cannot be read.
pub fn list(home: &Path) -> Result<()> {
    let remotes = load(home)?;
    if remotes.is_empty() {
        println!("No remotes saved. Connect one with `hashi connect <invite link>`.");
        return Ok(());
    }
    let width = remotes
        .iter()
        .map(|r| r.name.len())
        .max()
        .unwrap_or(4)
        .max(4);
    println!("{:<width$}  URL", "NAME");
    for r in &remotes {
        println!(
            "{:<width$}  {}",
            terminal_safe(&r.name),
            terminal_safe(&r.base_url)
        );
    }
    Ok(())
}

/// Forgets a saved remote and switches off the accounts that used it. The
/// token stays valid on the server until its owner revokes it.
///
/// # Errors
///
/// Fails when there is no remote with that name or saving fails.
pub fn remove(home: &Path, name: &str) -> Result<()> {
    let disabled = forget_counting(home, name)?;
    println!(
        "Removed {}. Its token still works until the owner revokes it.",
        terminal_safe(name)
    );
    if disabled > 0 {
        println!(
            "Switched off {disabled} account(s) that used it; remove them, or add them again \
             for another remote."
        );
    }
    Ok(())
}

/// Forgets a saved remote without printing anything, and switches off the
/// accounts that used it (see [`disable_remote_accounts`]).
///
/// # Errors
///
/// Fails when there is no remote with that name or saving fails.
pub fn forget(home: &Path, name: &str) -> Result<()> {
    forget_counting(home, name).map(drop)
}

/// Switches off every remote account that forwards to the remote `name`,
/// with a reason, so a new remote saved under the same name later does not
/// silently receive their prompts. Returns how many it switched off.
///
/// # Errors
///
/// Fails on database errors.
pub fn disable_remote_accounts(store: &crate::store::Store, name: &str) -> Result<usize> {
    let reason = format!(
        "its remote {} was removed; remove this account, or add it again for another remote",
        crate::util::one_line(name)
    );
    let mut n = 0;
    for a in store.list_accounts()? {
        if a.remote.as_deref() == Some(name) && !a.disabled {
            store.disable_account_because(a.agent, &a.name, &reason)?;
            n += 1;
        }
    }
    Ok(n)
}

fn forget_counting(home: &Path, name: &str) -> Result<usize> {
    let mut remotes = load(home)?;
    let before = remotes.len();
    remotes.retain(|r| r.name != name);
    if remotes.len() == before {
        bail!(
            "no remote called {}; `hashi remote list` shows them",
            terminal_safe(name)
        );
    }
    save(home, &remotes)?;
    // Only a home with a database has accounts; a client-only home gets none.
    if !home.join("hashi.db").is_file() {
        return Ok(0);
    }
    let store = crate::server::open_store(home)?;
    disable_remote_accounts(&store, name)
}

// ── ask ─────────────────────────────────────────────────────────────────────

/// What `hashi ask --remote` should send.
#[derive(Debug, Clone)]
pub struct AskOptions {
    /// Saved remote; `None` when exactly one is saved.
    pub name: Option<String>,
    /// The prompt.
    pub message: String,
    /// Agent; `None` uses the server's default.
    pub agent: Option<String>,
    /// Model override.
    pub model: Option<String>,
}

/// The result of a remote prompt.
#[derive(Debug, Clone, PartialEq)]
pub struct AskOutcome {
    /// The agent's answer, safe to print.
    pub response: String,
    /// The usage line, when the server reported usage.
    pub usage: Option<String>,
}

/// Sends a prompt to a saved remote and waits for the answer, polling with
/// long waits until it completes or fails.
///
/// # Errors
///
/// Fails when the remote is unknown, the server refuses the prompt, or the
/// run fails; the message is the server's, made safe to print.
pub fn ask(home: &Path, opts: &AskOptions) -> Result<AskOutcome> {
    let remotes = load(home)?;
    let remote = pick(&remotes, opts.name.as_deref())?;
    let agent = http();
    let mut body = json!({ "message": opts.message, "wait": WAIT_SECS });
    if let Some(a) = &opts.agent {
        body["agent"] = json!(a);
    }
    if let Some(m) = &opts.model {
        body["model"] = json!(m);
    }
    let (status, mut v) = post(
        &agent,
        &format!("{}/v1/ask", remote.base_url),
        Some(&remote.token),
        &body,
    )?;
    if status != 200 && status != 202 {
        bail!("{}", error_text(status, &v));
    }
    let id = v["id"].as_str().unwrap_or_default().to_owned();
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        bail!("the server answered without a request id");
    }
    loop {
        match v["status"].as_str() {
            Some("completed") => {
                let response =
                    neutralize_controls(v["response"].as_str().unwrap_or_default(), true);
                let usage = v.get("usage").filter(|u| u.is_object()).map(|u| {
                    let cost = u["costUsd"]
                        .as_f64()
                        .map(|c| format!(" \u{b7} ${c:.4}"))
                        .unwrap_or_default();
                    format!(
                        "{} in \u{b7} {} out{cost}",
                        u["inputTokens"].as_u64().unwrap_or(0),
                        u["outputTokens"].as_u64().unwrap_or(0)
                    )
                });
                return Ok(AskOutcome { response, usage });
            }
            Some("failed") => {
                let err = v["error"].as_str().unwrap_or("the agent failed");
                bail!("{}", terminal_safe(err));
            }
            _ => {}
        }
        let started = Instant::now();
        let (status, next) = get(
            &agent,
            &format!("{}/v1/ask/{id}?wait={WAIT_SECS}", remote.base_url),
            &remote.token,
        )?;
        if status != 200 && status != 202 {
            bail!("{}", error_text(status, &next));
        }
        v = next;
        // A server that cannot hold the wait answers at once; do not spin.
        if let Some(rest) = Duration::from_secs(1).checked_sub(started.elapsed()) {
            std::thread::sleep(rest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODE: &str = "hashi_inv_0123456789abcdef0123456789abcdef";

    #[test]
    fn accepts_https_and_loopback_http_links() {
        let l = parse_invite_url(&format!("https://Bridge.Example.com/claim/{CODE}")).unwrap();
        assert_eq!(l.base_url, "https://bridge.example.com");
        assert_eq!(l.code, CODE);
        for (url, base) in [
            (
                format!("http://127.0.0.1:7878/claim/{CODE}"),
                "http://127.0.0.1:7878",
            ),
            (
                format!("http://localhost:9/claim/{CODE}"),
                "http://localhost:9",
            ),
            (
                format!("http://[::1]:7878/claim/{CODE}"),
                "http://[::1]:7878",
            ),
            (
                format!("  https://h.example:8443/claim/{CODE}\n"),
                "https://h.example:8443",
            ),
        ] {
            assert_eq!(parse_invite_url(&url).unwrap().base_url, base, "{url}");
        }
    }

    #[test]
    fn plain_http_is_accepted_only_on_a_private_network() {
        for (url, base) in [
            (
                format!("http://192.168.1.2:7878/claim/{CODE}"),
                "http://192.168.1.2:7878",
            ),
            (format!("http://10.0.0.9/claim/{CODE}"), "http://10.0.0.9"),
            (
                format!("http://studio.local:7878/claim/{CODE}"),
                "http://studio.local:7878",
            ),
        ] {
            assert_eq!(parse_invite_url(&url).unwrap().base_url, base, "{url}");
        }
    }

    #[test]
    fn rejects_unsafe_or_malformed_links() {
        for url in [
            format!("http://h.example/claim/{CODE}"),
            format!("http://8.8.8.8/claim/{CODE}"),
            format!("http://172.32.0.1/claim/{CODE}"),
            format!("ftp://h.example/claim/{CODE}"),
            format!("https://user@h.example/claim/{CODE}"),
            format!("https://h.example/claim/{CODE}?x=1"),
            format!("https://h.example/claim/{CODE}#x"),
            format!("https://h.example/claim/{CODE}/"),
            format!("https://h.example/other/{CODE}"),
            format!("https://h.example:0/claim/{CODE}"),
            format!("https://h.example:99999/claim/{CODE}"),
            format!("https://h.example:/claim/{CODE}"),
            format!("https://h ex/claim/{CODE}"),
            format!("https://h.example';rm/claim/{CODE}"),
            format!("https://[zz]/claim/{CODE}"),
            "https://h.example/claim/hashi_inv_XYZ0123456789abcdef".to_owned(),
            "https://h.example/claim/hashi_inv_0123'".to_owned(),
            "https://h.example/claim/hashi_tok_0123456789abcdef".to_owned(),
            "https://h.example".to_owned(),
            String::new(),
        ] {
            assert!(parse_invite_url(&url).is_err(), "accepted {url:?}");
        }
    }

    #[test]
    fn loopback_hosts() {
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("127.0.0.2"));
        assert!(is_loopback_host("::1"));
        assert!(!is_loopback_host("10.0.0.1"));
        assert!(!is_loopback_host("localhost.example.com"));
    }

    #[test]
    fn names_are_slugged_and_checked() {
        assert_eq!(slug("Sid's Hashi!"), "sid-s-hashi");
        assert_eq!(slug("  --Test  Hashi--  "), "test-hashi");
        assert_eq!(slug("\u{202e}evil"), "evil");
        assert_eq!(slug("\u{2603}"), "");
        assert!(slug(&"a".repeat(100)).len() <= MAX_NAME_LEN);
        assert!(valid_name("work") && valid_name("a-b_c9"));
        assert!(!valid_name("") && !valid_name("-x") && !valid_name("A") && !valid_name("a b"));
        assert!(!valid_name(&"a".repeat(MAX_NAME_LEN + 1)));
    }

    #[test]
    fn free_name_adds_a_suffix() {
        let r = |n: &str| Remote {
            name: n.into(),
            base_url: "https://h".into(),
            token: "t".into(),
        };
        let saved = [r("hashi"), r("hashi-2")];
        assert_eq!(free_name(&saved, "hashi"), "hashi-3");
        assert_eq!(free_name(&saved, "other"), "other");
    }

    #[test]
    fn shell_quote_cannot_be_broken_out_of() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote("$(x) `y` \"z\""), "'$(x) `y` \"z\"'");
    }

    #[test]
    fn tokens_must_look_like_hashi_tokens() {
        assert!(valid_token("hashi_0123456789abcdef0123"));
        assert!(!valid_token("hashi_short"));
        assert!(!valid_token("hashi_0123456789abcdef'; rm -rf ~"));
        assert!(!valid_token("other_0123456789abcdef0123"));
    }

    #[test]
    fn claude_args_match_the_claim_page_snippet() {
        let args = claude_mcp_args("work", "https://h.example", "hashi_abc");
        let snippet = crate::api::snippets("https://h.example", "hashi_abc")["claudeCodeMcp"]
            .as_str()
            .unwrap()
            .to_owned();
        let joined = format!("claude {}", args[..7].join(" ")).replace(" work ", " hashi ")
            + &format!(" \"{}\"", args[7]);
        assert_eq!(joined, snippet);
    }

    #[test]
    fn remotes_round_trip_and_pick() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        assert!(load(&home).unwrap().is_empty());
        assert!(pick(&[], None).is_err());
        let a = Remote {
            name: "a".into(),
            base_url: "https://a.example".into(),
            token: "hashi_aaaa".into(),
        };
        save(&home, std::slice::from_ref(&a)).unwrap();
        let loaded = load(&home).unwrap();
        assert_eq!(loaded, vec![a.clone()]);
        assert_eq!(pick(&loaded, None).unwrap(), &a);
        let b = Remote {
            name: "b".into(),
            ..a.clone()
        };
        save(&home, &[a, b.clone()]).unwrap();
        let loaded = load(&home).unwrap();
        assert!(pick(&loaded, None).is_err(), "two saved needs a name");
        assert_eq!(pick(&loaded, Some("b")).unwrap(), &b);
        assert!(pick(&loaded, Some("c")).is_err());
        let text = std::fs::read_to_string(remotes_path(&home)).unwrap();
        assert!(text.contains("\"baseUrl\""));
        let leftovers: Vec<_> = std::fs::read_dir(&home)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn remotes_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        save(&home, &[]).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&remotes_path(&home)), 0o600);
        assert_eq!(mode(&home), 0o700);
        // Rewriting an existing, too-open file tightens it again.
        std::fs::set_permissions(remotes_path(&home), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        save(&home, &[]).unwrap();
        assert_eq!(mode(&remotes_path(&home)), 0o600);
    }
}
