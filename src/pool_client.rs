//! The member's side of a pool (see [`crate::pool`]).
//!
//! `hashi pool join <link>` asks the host to add this Hashi under a name,
//! makes a local token that holds what the member offers (agents,
//! accounts, a stop on their quota, an end time) and keeps both in
//! `<home>/pools.json`, readable only by the owner. While `hashi serve`
//! runs, a worker for each joined pool keeps one long poll open to the
//! host, runs each prompt it gets through this machine's own queue with
//! that local token, so every limit of this Hashi holds, and posts the
//! answer back. Pausing the local token takes this Hashi out of rotation;
//! revoking it, or the end time passing, leaves the pool.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::accounts::AccountMap;
use crate::agent::AgentKind;
use crate::api::{ApiError, AppState, SubmitInput};
use crate::store::{NewToken, RequestStatus, Store};
use crate::token::{Limits, Role, SessionMode, TokenStatus};
use crate::util::now_ms;

/// The file that keeps the pools this Hashi joined.
pub const POOLS_FILE: &str = "pools.json";

/// Prompts this Hashi runs for one pool at once.
const MEMBER_MAX_PENDING: u32 = 2;

/// One pool this Hashi joined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Joined {
    /// The host's name, as it gave it.
    pub host: String,
    /// This Hashi's name in the pool.
    pub name: String,
    /// The host's base URL.
    pub base_url: String,
    /// The worker secret.
    pub secret: String,
    /// The token for using the pool, on the host.
    pub token: String,
    /// The local token the pool's prompts run with.
    pub local_token_id: String,
    /// The agents offered.
    #[serde(deserialize_with = "crate::agent::known_agents")]
    pub agents: Vec<AgentKind>,
    /// When this Hashi leaves, Unix ms.
    pub until: Option<i64>,
    /// The remote saved for using the pool (`hashi ask --remote <it>`).
    #[serde(default)]
    pub remote: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PoolsFile {
    pools: Vec<Joined>,
}

fn path(home: &Path) -> PathBuf {
    home.join(POOLS_FILE)
}

/// The pools this Hashi joined. A missing file means none.
///
/// # Errors
///
/// Fails if the file cannot be read or parsed.
pub fn load(home: &Path) -> Result<Vec<Joined>> {
    let p = path(home);
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", p.display())),
    };
    let file: PoolsFile =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", p.display()))?;
    Ok(file.pools)
}

fn save(home: &Path, pools: &[Joined]) -> Result<()> {
    crate::server::prepare_home(home)?;
    let p = path(home);
    let tmp = home.join(format!(".{POOLS_FILE}.{}.tmp", std::process::id()));
    let text = serde_json::to_string_pretty(&PoolsFile {
        pools: pools.to_vec(),
    })?;
    crate::diagnostics::write_private(&tmp, &format!("{text}\n"))
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &p).with_context(|| format!("replacing {}", p.display()))
}

/// A pool link's base URL and code.
///
/// # Errors
///
/// Explains what is wrong with the link.
pub fn parse_link(input: &str) -> Result<(String, String)> {
    let (base, _, code) = crate::remote::parse_link_url(input, "/pool/", "a pool link")?;
    let hex = code
        .strip_prefix(crate::store::POOL_LINK_PREFIX)
        .unwrap_or_default();
    if hex.len() < 16 || hex.len() > 100 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("this is not a pool link: the code must be hashi_pool_ and hex digits");
    }
    Ok((base, code))
}

/// What to offer when joining.
#[derive(Debug, Clone)]
pub struct JoinOptions {
    /// The pool link.
    pub link: String,
    /// This Hashi's name in the pool.
    pub name: String,
    /// The agents to offer.
    pub agents: Vec<AgentKind>,
    /// The accounts to run them on, per agent (default: each agent's
    /// default account).
    pub accounts: AccountMap,
    /// Stop at this percent of this machine's quota.
    pub stop_at: Option<u8>,
    /// Hours to stay.
    pub hours: Option<u32>,
}

/// Joins a pool: asks the host, makes the local token and saves both.
///
/// # Errors
///
/// Fails on a bad link, a refusal from the host, or when saving fails.
pub fn join(store: &Store, home: &Path, opts: &JoinOptions) -> Result<Joined> {
    let (base, code) = parse_link(&opts.link)?;
    if opts.agents.is_empty() {
        bail!("offer at least one agent");
    }
    let name = opts.name.trim().to_ascii_lowercase();
    crate::accounts::check_name(&name, false).map_err(|e| anyhow::anyhow!("--name: {e}"))?;
    let accounts = crate::api::check_accounts(store, opts.accounts.clone())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut pools = load(home)?;
    if pools.iter().any(|p| p.base_url == base) {
        bail!("this Hashi is already in that pool; leave it first with `hashi pool leave`");
    }
    let http = crate::remote::http();
    let (status, v) = crate::remote::post(
        &http,
        &format!("{base}/v1/pool/join"),
        None,
        &json!({
            "code": code,
            "name": name,
            "agents": opts.agents,
            "hours": opts.hours,
        }),
    )?;
    if status != 201 {
        bail!("{}", crate::remote::error_text(status, &v));
    }
    let secret = v["secret"].as_str().unwrap_or_default().to_owned();
    let token = v["token"].as_str().unwrap_or_default().to_owned();
    if !secret.starts_with(crate::store::MEMBER_PREFIX) || token.is_empty() {
        bail!("the host answered without what this Hashi needs to serve the pool");
    }
    let host = v["server"].as_str().unwrap_or("the host").to_owned();
    let until = opts.hours.map(|h| now_ms() + i64::from(h) * 3_600_000);
    let (mut local, _) = store.create_token(&NewToken {
        name: format!("pool {}", crate::remote::slug(&host)),
        role: Role::User,
        session_mode: SessionMode::Stateless,
        agents: opts.agents.clone(),
        expires_in_days: None,
        note: Some(format!("serves the pool on {base}")),
        limits: Limits {
            max_pending: Some(MEMBER_MAX_PENDING),
            stop_at_percent: opts.stop_at,
            ..Limits::default()
        },
        accounts,
        project: None,
    })?;
    if until.is_some() {
        local.expires_at = until;
        store.save_token_policy(&local)?;
    }
    store.audit("pool", "pool.join", &host, &name)?;
    // The pool token as a saved remote, so `hashi ask --remote` and remote
    // accounts can use the pool like any bridge.
    let remote = {
        let mut remotes = crate::remote::load(home)?;
        let wanted = format!("{}-pool", crate::remote::slug(&host));
        let wanted = if crate::remote::valid_name(&wanted) {
            wanted
        } else {
            "pool".to_owned()
        };
        let name = (1..1000)
            .map(|i| {
                if i == 1 {
                    wanted.clone()
                } else {
                    format!("{wanted}-{i}")
                }
            })
            .find(|n| !remotes.iter().any(|r| &r.name == n))
            .unwrap_or_else(|| format!("{wanted}-x"));
        remotes.push(crate::remote::Remote {
            name: name.clone(),
            base_url: base.clone(),
            token: token.clone(),
        });
        crate::remote::save(home, &remotes)?;
        Some(name)
    };
    let joined = Joined {
        host,
        name,
        base_url: base,
        secret,
        token,
        local_token_id: local.id,
        agents: opts.agents.clone(),
        until,
        remote,
    };
    pools.push(joined.clone());
    save(home, &pools)?;
    Ok(joined)
}

/// Leaves the pool at `host_or_url` (or the only one): tells the host,
/// revokes the local token and forgets it. The host is told on a best
/// effort; leaving here always happens.
///
/// # Errors
///
/// Fails when no such pool was joined or the file cannot be written.
pub fn leave(store: &Store, home: &Path, host_or_url: Option<&str>) -> Result<Joined> {
    let mut pools = load(home)?;
    let i = match host_or_url {
        Some(h) => pools
            .iter()
            .position(|p| p.host == h || p.base_url == h || p.name == h)
            .with_context(|| format!("not in a pool called {h}; `hashi pool list` shows them"))?,
        None => match pools.len() {
            0 => bail!("this Hashi is in no pool"),
            1 => 0,
            _ => bail!("this Hashi is in several pools; name one (`hashi pool list`)"),
        },
    };
    let p = pools.remove(i);
    let http = crate::remote::http();
    let _ = crate::remote::post(
        &http,
        &format!("{}/v1/pool/leave", p.base_url),
        Some(&p.secret),
        &json!({}),
    );
    let _ = store.revoke_token(&p.local_token_id);
    let _ = store.audit("pool", "pool.leave", &p.host, &p.name);
    save(home, &pools)?;
    forget_remote(home, &p);
    Ok(p)
}

/// Removes the remote saved for using `p`.
fn forget_remote(home: &Path, p: &Joined) {
    let Some(name) = &p.remote else { return };
    if let Ok(mut remotes) = crate::remote::load(home) {
        remotes.retain(|r| &r.name != name || r.base_url != p.base_url);
        let _ = crate::remote::save(home, &remotes);
    }
}

/// Forgets a pool the host already dropped, and its local token.
fn forget(state: &AppState, home: &Path, secret: &str, why: &str) {
    let Ok(mut pools) = load(home) else {
        return;
    };
    let Some(i) = pools.iter().position(|p| p.secret == secret) else {
        return;
    };
    let p = pools.remove(i);
    let _ = state.store.revoke_token(&p.local_token_id);
    forget_remote(home, &p);
    if let Err(e) = save(home, &pools) {
        tracing::warn!(error = %e, "could not forget a pool");
    }
    state.bus.log(format!("left the pool on {}: {why}", p.host));
}

/// Starts the workers: every few seconds, one for each joined pool that
/// has none yet, until `cancel` fires.
pub fn spawn(state: AppState, home: PathBuf, cancel: CancellationToken) {
    let running: Arc<Mutex<HashSet<String>>> = Arc::default();
    tokio::spawn(async move {
        loop {
            for p in load(&home).unwrap_or_default() {
                let fresh = running
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(p.secret.clone());
                if fresh {
                    let (state, home, cancel, running) = (
                        state.clone(),
                        home.clone(),
                        cancel.clone(),
                        Arc::clone(&running),
                    );
                    tokio::spawn(async move {
                        let secret = p.secret.clone();
                        serve(&state, &home, &p, &cancel).await;
                        running
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .remove(&secret);
                    });
                }
            }
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(Duration::from_secs(5)) => {}
            }
        }
    });
}

/// What one poll got.
enum Polled {
    Job(crate::pool::Job),
    Nothing,
    Dropped(String),
    Failed,
}

fn poll_once(p: &Joined, usage: &Value) -> Polled {
    let http = crate::remote::http();
    match crate::remote::post(
        &http,
        &format!("{}/v1/pool/poll", p.base_url),
        Some(&p.secret),
        &json!({ "usage": usage }),
    ) {
        Ok((200, v)) => {
            match serde_json::from_value::<Option<crate::pool::Job>>(v["job"].clone()) {
                Ok(Some(job)) => Polled::Job(job),
                _ => Polled::Nothing,
            }
        }
        Ok((401, v)) => Polled::Dropped(crate::remote::error_text(401, &v)),
        _ => Polled::Failed,
    }
}

/// One pool's worker: polls the host while this Hashi is in the pool.
async fn serve(state: &AppState, home: &Path, p: &Joined, cancel: &CancellationToken) {
    let mut failures: u32 = 0;
    loop {
        if cancel.is_cancelled()
            || !load(home)
                .unwrap_or_default()
                .iter()
                .any(|x| x.secret == p.secret)
        {
            return;
        }
        let now = now_ms();
        let token = match state.store.get_token(&p.local_token_id) {
            Ok(Some(t)) => t,
            Ok(None) => {
                let _ = tokio::task::spawn_blocking({
                    let p = p.clone();
                    move || {
                        let http = crate::remote::http();
                        crate::remote::post(
                            &http,
                            &format!("{}/v1/pool/leave", p.base_url),
                            Some(&p.secret),
                            &json!({}),
                        )
                    }
                })
                .await;
                forget(state, home, &p.secret, "its token here was revoked");
                return;
            }
            Err(_) => {
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        match token.status(now) {
            TokenStatus::Expired => {
                let p2 = p.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    let http = crate::remote::http();
                    crate::remote::post(
                        &http,
                        &format!("{}/v1/pool/leave", p2.base_url),
                        Some(&p2.secret),
                        &json!({}),
                    )
                })
                .await;
                forget(state, home, &p.secret, "its time is up");
                return;
            }
            // Paused: out of rotation. The host sees it go offline.
            TokenStatus::Paused => {
                tokio::select! {
                    () = cancel.cancelled() => return,
                    () = tokio::time::sleep(Duration::from_secs(5)) => {}
                }
                continue;
            }
            TokenStatus::Active => {}
        }
        let usage = crate::api::shared_usage(state, &token, now).unwrap_or(Value::Null);
        let job = {
            let p = p.clone();
            tokio::select! {
                () = cancel.cancelled() => return,
                r = tokio::task::spawn_blocking(move || poll_once(&p, &usage)) => r,
            }
        };
        match job {
            Ok(Polled::Job(job)) => {
                failures = 0;
                let (state, p, token) = (state.clone(), p.clone(), token.clone());
                tokio::spawn(async move {
                    let outcome = run(&state, &token, &job).await;
                    let _ = tokio::task::spawn_blocking(move || {
                        let http = crate::remote::http();
                        crate::remote::post(
                            &http,
                            &format!("{}/v1/pool/result", p.base_url),
                            Some(&p.secret),
                            &outcome_json(&job.id, &outcome),
                        )
                    })
                    .await;
                });
            }
            Ok(Polled::Nothing) => failures = 0,
            Ok(Polled::Dropped(why)) => {
                let _ = state.store.revoke_token(&p.local_token_id);
                forget(state, home, &p.secret, &why);
                return;
            }
            Ok(Polled::Failed) | Err(_) => {
                failures = failures.saturating_add(1);
                let wait = Duration::from_secs(u64::from(failures.min(6)) * 5);
                tokio::select! {
                    () = cancel.cancelled() => return,
                    () = tokio::time::sleep(wait) => {}
                }
            }
        }
    }
}

/// Runs a pool prompt through this Hashi's own queue with the pool's
/// local token, and waits for it.
async fn run(
    state: &AppState,
    token: &crate::token::Token,
    job: &crate::pool::Job,
) -> crate::pool::Outcome {
    use crate::pool::Outcome;
    let submitted = crate::api::submit(
        state,
        token,
        SubmitInput {
            message: job.message.clone(),
            agent: Some(job.agent.as_str().to_owned()),
            model: job.model.clone(),
            account: None,
            hops: 1,
        },
    )
    .await;
    let req = match submitted {
        Ok(r) => r,
        Err(ApiError::Coded {
            code: "limit_reached",
            message,
            extra,
            ..
        }) => {
            return Outcome::Limited {
                reset_at: extra
                    .as_ref()
                    .and_then(|e| e["resetAt"].as_str())
                    .and_then(crate::util::parse_rfc3339),
                message,
            };
        }
        Err(
            e @ (ApiError::Coded { .. }
            | ApiError::TooManyRequests(_)
            | ApiError::Forbidden(_)
            | ApiError::Unavailable(_)),
        ) => {
            return Outcome::Busy(e.to_string());
        }
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    let limit = state.run_timeout_secs.saturating_add(60);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(limit);
    loop {
        match state.store.get_request(&req.id) {
            Ok(Some(r)) if r.status == RequestStatus::Completed => {
                return Outcome::Done {
                    text: r.response.unwrap_or_default(),
                    input_tokens: r.usage.map(|u| u.input_tokens),
                    output_tokens: r.usage.map(|u| u.output_tokens),
                    cost_usd: r.usage.and_then(|u| u.cost_usd),
                };
            }
            Ok(Some(r)) if r.status == RequestStatus::Failed => {
                let error = r.error.unwrap_or_default();
                return if error.contains("at its limit") || error.contains("keeps the rest") {
                    Outcome::Limited {
                        reset_at: None,
                        message: error,
                    }
                } else {
                    Outcome::Failed(error)
                };
            }
            Ok(None) => return Outcome::Failed("the run was removed".to_owned()),
            _ => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Outcome::Failed("the run took too long".to_owned());
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

fn outcome_json(job_id: &str, o: &crate::pool::Outcome) -> Value {
    use crate::pool::Outcome;
    match o {
        Outcome::Done {
            text,
            input_tokens,
            output_tokens,
            cost_usd,
        } => json!({
            "jobId": job_id, "kind": "done", "text": text,
            "inputTokens": input_tokens, "outputTokens": output_tokens, "costUsd": cost_usd,
        }),
        Outcome::Limited { reset_at, message } => json!({
            "jobId": job_id, "kind": "limited", "message": message,
            "resetAt": reset_at.and_then(crate::util::rfc3339),
        }),
        Outcome::Busy(m) => json!({ "jobId": job_id, "kind": "busy", "message": m }),
        Outcome::Failed(m) => json!({ "jobId": job_id, "kind": "failed", "message": m }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_links_parse() {
        let (base, code) =
            parse_link("https://party.example/pool/hashi_pool_0123456789abcdef0123").unwrap();
        assert_eq!(base, "https://party.example");
        assert!(code.starts_with("hashi_pool_"));
        assert!(parse_link("https://party.example/claim/hashi_inv_0123456789abcdef").is_err());
        let (lan, _) =
            parse_link("http://192.168.1.4:7878/pool/hashi_pool_0123456789abcdef").unwrap();
        assert_eq!(lan, "http://192.168.1.4:7878");
        assert!(parse_link("http://8.8.8.8:7878/pool/hashi_pool_0123456789abcdef").is_err());
        assert!(parse_link("http://127.0.0.1:7878/pool/hashi_pool_0123456789abcdef").is_ok());
    }
}
