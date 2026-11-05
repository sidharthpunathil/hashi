//! `hashi request <link>` and `hashi request status`: asking someone's
//! Hashi for access from a terminal.
//!
//! The pickup secret the bridge returns is kept in
//! `<home>/pending-requests.json`, readable only by you. `hashi request
//! status` checks each pending request and, once one is accepted, redeems
//! its single-use invite and saves the remote exactly as `hashi connect`
//! does.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::diagnostics::{terminal_safe, write_private};
use crate::remote::{self, ConnectOptions};
use crate::store::{LINK_PREFIX, PICKUP_PREFIX};

/// File name of the pending requests, inside the Hashi home.
pub const PENDING_FILE: &str = "pending-requests.json";

/// A checked ask-access link such as `https://host/ask-access/hashi_ask_<hex>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskLink {
    /// `scheme://host[:port]`, with a lowercase host.
    pub base_url: String,
    /// The link code.
    pub code: String,
}

fn hex_code(code: &str, prefix: &str, len: usize) -> bool {
    code.strip_prefix(prefix).is_some_and(|hex| {
        hex.len() == len && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// Parses and checks an ask-access link, with the same rules as invite
/// links: `https`, or `http` for loopback; no user info, query or
/// fragment; the path exactly `/ask-access/hashi_ask_<32 hex>`.
///
/// # Errors
///
/// Explains what is wrong with the link.
pub fn parse_ask_url(input: &str) -> Result<AskLink> {
    let (base_url, _, code) =
        remote::parse_bridge_url(input, "/ask-access/", "an ask-access link")?;
    if !hex_code(&code, LINK_PREFIX, 32) {
        bail!("this is not an ask-access link: the code must be {LINK_PREFIX} and 32 hex digits");
    }
    Ok(AskLink { base_url, code })
}

// ── pending-requests.json ───────────────────────────────────────────────────

/// One request waiting for an answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRequest {
    /// `scheme://host[:port]` of the bridge, from the link you used.
    pub base_url: String,
    /// The request id.
    pub id: String,
    /// The pickup secret.
    pub pickup: String,
    /// The name you asked under.
    pub name: String,
    /// The bridge's name, as it reported it.
    pub server: String,
    /// Name to save the remote under once accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_name: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PendingFile {
    requests: Vec<PendingRequest>,
}

/// Path of `pending-requests.json` in `home`.
#[must_use]
pub fn pending_path(home: &Path) -> PathBuf {
    home.join(PENDING_FILE)
}

/// Reads the pending requests. A missing file means none.
///
/// # Errors
///
/// Fails if the file cannot be read or parsed.
pub fn load(home: &Path) -> Result<Vec<PendingRequest>> {
    let path = pending_path(home);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let file: PendingFile =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(file.requests)
}

/// Writes the pending requests atomically and owner only. An empty list
/// removes the file.
///
/// # Errors
///
/// Fails if the home cannot be prepared or the file cannot be written.
pub fn save(home: &Path, requests: &[PendingRequest]) -> Result<()> {
    let path = pending_path(home);
    if requests.is_empty() {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
        };
    }
    crate::server::prepare_home(home)?;
    let tmp = home.join(format!(".{PENDING_FILE}.{}.tmp", std::process::id()));
    let text = serde_json::to_string_pretty(&PendingFile {
        requests: requests.to_vec(),
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

// ── asking ──────────────────────────────────────────────────────────────────

/// What `hashi request <link>` sends.
#[derive(Debug, Clone)]
pub struct RequestOptions {
    /// The ask-access link.
    pub url: String,
    /// Your name, 1 to 60 characters.
    pub name: String,
    /// What you want it for.
    pub message: Option<String>,
    /// Agents you want; `None` asks for all the link offers.
    pub agents: Option<Vec<String>>,
    /// Name to save the remote under once accepted.
    pub remote_name: Option<String>,
}

/// Sends a request for access and saves its pickup secret.
///
/// # Errors
///
/// Fails on a bad link, a refused request, or when saving fails.
pub fn request(home: &Path, opts: &RequestOptions) -> Result<PendingRequest> {
    let link = parse_ask_url(&opts.url)?;
    if let Some(n) = &opts.remote_name
        && !remote::valid_name(n)
    {
        bail!(
            "--remote-name must be 1 to {} lowercase letters, digits, - or _, starting with a \
             letter or digit",
            remote::MAX_NAME_LEN
        );
    }
    let agent = remote::http();
    let (status, preview) = remote::post(
        &agent,
        &format!("{}/v1/ask-access/preview", link.base_url),
        None,
        &json!({ "code": link.code }),
    )?;
    if status != 200 {
        bail!("{}", remote::error_text(status, &preview));
    }
    let server = terminal_safe(preview["server"].as_str().unwrap_or("the bridge"));
    if let Some(note) = preview["note"].as_str() {
        println!("  {server} says: {}", terminal_safe(note));
    }
    let mut body = json!({ "code": link.code, "name": opts.name });
    if let Some(m) = &opts.message {
        body["message"] = json!(m);
    }
    if let Some(a) = &opts.agents {
        body["agents"] = json!(a);
    }
    let (status, r) = remote::post(
        &agent,
        &format!("{}/v1/ask-access", link.base_url),
        None,
        &body,
    )?;
    if status != 201 {
        bail!("{}", remote::error_text(status, &r));
    }
    let pickup = r["pickup"].as_str().unwrap_or_default();
    let id = r["id"].as_str().unwrap_or_default();
    if !hex_code(pickup, PICKUP_PREFIX, 64)
        || id.is_empty()
        || !id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
    {
        bail!("the server answered without a usable pickup secret");
    }
    let pending = PendingRequest {
        base_url: link.base_url,
        id: id.to_owned(),
        pickup: pickup.to_owned(),
        name: opts.name.clone(),
        server: preview["server"].as_str().unwrap_or("hashi").to_owned(),
        remote_name: opts.remote_name.clone(),
    };
    let mut all = load(home)?;
    all.push(pending.clone());
    save(home, &all)?;
    println!();
    println!("  \u{2713} asked {server} for access");
    println!(
        "    the owner decides; check with `hashi request status` (saved in {}, only you can \
         read it)",
        pending_path(home).display()
    );
    println!();
    Ok(pending)
}

/// What checking one pending request found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    /// The bridge's name, safe to print.
    pub server: String,
    /// `pending`, `connected`, `declined`, `expired`, `claimed`, `gone` or
    /// `error`.
    pub state: &'static str,
    /// The reason, error or remote name, safe to print.
    pub detail: String,
}

/// Checks every pending request. Accepted ones are redeemed and saved as
/// remotes; decided, expired and unknown ones are forgotten; the rest stay.
///
/// # Errors
///
/// Fails if `pending-requests.json` cannot be read or written.
pub fn check(home: &Path) -> Result<Vec<Checked>> {
    let all = load(home)?;
    let agent = remote::http();
    let mut keep = Vec::new();
    let mut out = Vec::new();
    for p in all {
        let server = terminal_safe(&p.server);
        let url = format!("{}/v1/ask-access/status", p.base_url);
        let answer = remote::post(&agent, &url, None, &json!({ "pickup": p.pickup }));
        let (status, v) = match answer {
            Ok(x) => x,
            Err(e) => {
                out.push(Checked {
                    server,
                    state: "error",
                    detail: terminal_safe(&format!("{e:#}")),
                });
                keep.push(p);
                continue;
            }
        };
        if status == 404 {
            out.push(Checked {
                server,
                state: "gone",
                detail: "the bridge no longer knows this request".to_owned(),
            });
            continue;
        }
        if status != 200 {
            out.push(Checked {
                server,
                state: "error",
                detail: remote::error_text(status, &v),
            });
            keep.push(p);
            continue;
        }
        match v["status"].as_str() {
            Some("accepted") => match connect(home, &p, &v) {
                Ok(name) => out.push(Checked {
                    server,
                    state: "connected",
                    detail: name,
                }),
                Err(e) => {
                    out.push(Checked {
                        server,
                        state: "error",
                        detail: terminal_safe(&format!("{e:#}")),
                    });
                    keep.push(p);
                }
            },
            Some("declined") => out.push(Checked {
                server,
                state: "declined",
                detail: v["reason"].as_str().map(terminal_safe).unwrap_or_default(),
            }),
            Some("expired") => out.push(Checked {
                server,
                state: "expired",
                detail: String::new(),
            }),
            Some("claimed") => out.push(Checked {
                server,
                state: "claimed",
                detail: "the access was already collected elsewhere".to_owned(),
            }),
            _ => {
                out.push(Checked {
                    server,
                    state: "pending",
                    detail: v["expiresAt"]
                        .as_str()
                        .map(terminal_safe)
                        .unwrap_or_default(),
                });
                keep.push(p);
            }
        }
    }
    save(home, &keep)?;
    Ok(out)
}

/// Redeems the claim code with the base URL of the link the request went
/// to: a URL in the answer could point the token somewhere else, so only
/// the code is taken from it.
fn connect(home: &Path, p: &PendingRequest, v: &Value) -> Result<String> {
    let code = v["claimCode"].as_str().unwrap_or_default();
    let link = remote::parse_invite_url(&format!("{}/claim/{code}", p.base_url))
        .context("the bridge answered without a usable claim code")?;
    let taken = remote::load(home)?
        .iter()
        .any(|r| Some(&r.name) == p.remote_name.as_ref());
    let remote = remote::connect(
        home,
        &ConnectOptions {
            url: format!("{}/claim/{}", link.base_url, link.code),
            name: p.remote_name.clone().filter(|_| !taken),
            setup_claude: None,
        },
    )?;
    Ok(remote.name)
}

/// Runs `hashi request status` and prints what it found.
///
/// # Errors
///
/// Fails if `pending-requests.json` cannot be read or written.
pub fn status(home: &Path) -> Result<()> {
    let found = check(home)?;
    if found.is_empty() {
        println!("No pending requests. Ask with `hashi request <link>`.");
        return Ok(());
    }
    for c in &found {
        match c.state {
            "pending" => println!(
                "  \u{25cc} {}: waiting for the owner (expires {})",
                c.server, c.detail
            ),
            "connected" => println!(
                "  \u{2713} {}: accepted, saved as remote \"{}\"",
                c.server, c.detail
            ),
            "declined" if c.detail.is_empty() => println!("  \u{2717} {}: declined", c.server),
            "declined" => println!("  \u{2717} {}: declined: {}", c.server, c.detail),
            "expired" => println!("  \u{2717} {}: expired without an answer", c.server),
            other => println!("  ! {}: {other}: {}", c.server, c.detail),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODE: &str = "hashi_ask_0123456789abcdef0123456789abcdef";

    #[test]
    fn ask_links_are_checked_like_invite_links() {
        let l = parse_ask_url(&format!("https://Bridge.Example/ask-access/{CODE}")).unwrap();
        assert_eq!(
            (l.base_url.as_str(), l.code.as_str()),
            ("https://bridge.example", CODE)
        );
        assert!(parse_ask_url(&format!("http://127.0.0.1:7878/ask-access/{CODE}")).is_ok());
        for bad in [
            format!("http://h.example/ask-access/{CODE}"),
            format!("https://h.example/claim/{CODE}"),
            format!("https://h.example/ask-access/{CODE}?x"),
            format!("https://u@h.example/ask-access/{CODE}"),
            "https://h.example/ask-access/hashi_ask_0123".to_owned(),
            "https://h.example/ask-access/hashi_inv_0123456789abcdef0123456789abcdef".to_owned(),
        ] {
            assert!(parse_ask_url(&bad).is_err(), "accepted {bad}");
        }
    }

    #[test]
    fn pending_file_round_trips_and_is_removed_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        assert!(load(&home).unwrap().is_empty());
        let p = PendingRequest {
            base_url: "https://h".into(),
            id: "1".into(),
            pickup: "hashi_pick_x".into(),
            name: "me".into(),
            server: "S".into(),
            remote_name: None,
        };
        save(&home, std::slice::from_ref(&p)).unwrap();
        assert_eq!(load(&home).unwrap(), vec![p]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(pending_path(&home))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        save(&home, &[]).unwrap();
        assert!(!pending_path(&home).exists());
    }
}
