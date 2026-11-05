//! Remote accounts: running a prompt on a friend's bridge, and reading what
//! that bridge says about the token's limits and shared headroom.
//!
//! A remote account (see [`crate::accounts`]) names a saved remote from
//! `remotes.json` and the agent to ask for there. A run on it sends
//! `POST /v1/ask` with `wait` to that bridge with the remote's token, then
//! long-polls `GET /v1/ask/{id}` until the request finishes, fails or the
//! local run timeout passes. A local cancel or timeout sends
//! `DELETE /v1/ask/{id}`, best effort.
//!
//! The bridge's refusals map onto what the queue already understands (see
//! [`Failure`]): limits mark the account limited, a paused, revoked or
//! forbidden token disables it with the reason shown to the owner, and a
//! network failure or a transient refusal (`too_many_pending`,
//! `rate_limited`) fails over to the next account after a short back-off.
//!
//! **Loops.** Every forwarded prompt carries [`HOPS_HEADER`], one more than
//! the hops the prompt arrived with, and [`FROM_HEADER`], this bridge's
//! instance id. A bridge refuses a prompt past [`MAX_HOPS`], and one that
//! names itself as the sender.
//!
//! The remote's token is only ever put in the `Authorization` header. It is
//! never logged, stored in the database or returned by the API, and
//! [`Target`]'s `Debug` leaves it out.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::agent::{AgentKind, Usage};
use crate::diagnostics::neutralize_controls;
use crate::remote::Remote;
use crate::usage::{Source, Window};

/// Most bridges a prompt may be forwarded through. A bridge refuses a
/// prompt that arrives with more hops than this.
pub const MAX_HOPS: u8 = 2;

/// Header with the number of bridges that already forwarded a prompt.
pub const HOPS_HEADER: &str = "x-hashi-hops";

/// Header with the instance id of the bridge forwarding a prompt.
pub const FROM_HEADER: &str = "x-hashi-from";

/// Longest single long poll on the remote bridge, in seconds.
const POLL_WAIT_SECS: u64 = 30;

/// Window key of shared headroom read from a remote bridge.
pub const SHARED_WINDOW: &str = "remote";

/// Where a remote account's runs go: the saved remote's bridge and token,
/// and the agent to ask for there.
#[derive(Clone)]
pub struct Target {
    /// The saved remote's name.
    pub remote: String,
    /// `scheme://host[:port]` of the bridge.
    pub base_url: String,
    token: String,
    /// The agent asked for on the bridge.
    pub agent: AgentKind,
    /// The bridge's instance id when the account was added, which each
    /// forward checks first.
    pub instance: Option<String>,
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target")
            .field("remote", &self.remote)
            .field("base_url", &self.base_url)
            .field("agent", &self.agent)
            .field("instance", &self.instance)
            .finish_non_exhaustive()
    }
}

impl Target {
    /// The target for `remote`'s bridge and `agent` there.
    #[must_use]
    pub fn new(remote: &Remote, agent: AgentKind) -> Self {
        Self {
            remote: remote.name.clone(),
            base_url: remote.base_url.trim_end_matches('/').to_owned(),
            token: remote.token.clone(),
            agent,
            instance: None,
        }
    }

    /// The same target, bound to the bridge with instance id `instance`
    /// (as recorded when the account was added). `None` checks nothing.
    #[must_use]
    pub fn expecting(mut self, instance: Option<String>) -> Self {
        self.instance = instance;
        self
    }

    /// The target for the saved remote called `name` in `home`.
    ///
    /// # Errors
    ///
    /// Says why it could not be loaded; never contains the token.
    pub fn load(home: &std::path::Path, name: &str, agent: AgentKind) -> Result<Self, String> {
        let remotes = crate::remote::load(home).map_err(|e| format!("{e:#}"))?;
        remotes
            .iter()
            .find(|r| r.name == name)
            .map(|r| Self::new(r, agent))
            .ok_or_else(|| {
                format!("the saved remote {name} is gone; `hashi remote list` shows them")
            })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }
}

/// How a forwarded run failed.
#[derive(Debug, Clone, PartialEq)]
pub enum Failure {
    /// The remote token or the accounts behind it are at a limit, until
    /// this time (Unix ms) when the bridge said.
    Limited {
        /// When the limit resets.
        reset_at: Option<i64>,
        /// What the bridge said.
        message: String,
    },
    /// The remote token is paused, revoked, expired or not allowed: the
    /// account should be switched off, with this reason.
    Disabled(String),
    /// The bridge could not be reached or could not take the run now: the
    /// run moves to the next account.
    Retryable(String),
    /// The run itself failed on the bridge, with this error.
    Failed(String),
    /// The local timeout passed.
    TimedOut,
}

/// A finished forwarded run.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    /// The answer.
    pub text: String,
    /// Tokens and cost, as the bridge reported them.
    pub usage: Option<Usage>,
}

/// What one forwarded run sends.
#[derive(Debug, Clone, Copy)]
pub struct Call<'a> {
    /// The prompt.
    pub message: &'a str,
    /// Model override.
    pub model: Option<&'a str>,
    /// Hops the prompt arrived here with; the bridge gets one more.
    pub hops: u8,
    /// This bridge's instance id.
    pub from: &'a str,
    /// When the local run timeout passes.
    pub deadline: Instant,
    /// Longest answer kept, in bytes.
    pub max_output: usize,
}

/// Shared between a forwarded run on a blocking thread and the task that
/// owns it: a stop flag, and the remote request id once known, so a
/// cancelled run can be cancelled on the bridge too.
#[derive(Debug, Default)]
pub struct Handle {
    stop: AtomicBool,
    id: Mutex<Option<String>>,
}

impl Handle {
    /// Stops the run at its next step and returns the remote request id,
    /// if one was given and the run has not finished, so it can be
    /// cancelled on the bridge. Only the first call returns it.
    pub fn stop(&self) -> Option<String> {
        self.stop.store(true, Ordering::SeqCst);
        self.id
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn set_id(&self, id: &str) {
        *self.id.lock().unwrap_or_else(PoisonError::into_inner) = Some(id.to_owned());
    }

    fn finish(&self) {
        self.id
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }
}

fn http(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        // Never carry the token to another host.
        .max_redirects(0)
        .timeout_global(Some(timeout))
        .user_agent(concat!("hashi/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

fn read(
    res: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    remote: &str,
) -> Result<(u16, Value), Failure> {
    let mut res = res.map_err(|e| {
        Failure::Retryable(format!(
            "could not reach the bridge of remote {remote}: {}",
            crate::diagnostics::terminal_safe(&e.to_string())
        ))
    })?;
    let status = res.status().as_u16();
    let body = res
        .body_mut()
        .with_config()
        .limit(16 * 1024 * 1024)
        .read_json::<Value>()
        .unwrap_or(Value::Null);
    Ok((status, body))
}

/// The bridge's error message, made safe to show.
fn message(status: u16, body: &Value) -> String {
    let msg = body["error"]["message"]
        .as_str()
        .map_or_else(|| format!("the bridge answered {status}"), str::to_owned);
    crate::util::truncate(&crate::diagnostics::terminal_safe(&msg), 300)
}

/// What a refusal from the bridge means for the account.
///
/// Only `limit_reached`, a daily or monthly cap of the remote token, marks
/// the account limited. `too_many_pending`, `rate_limited` and any other
/// `429` are transient and say nothing about the account: one token holder
/// here can cause them by sending a burst, so they are [`Failure::Retryable`]
/// and the queue backs off the account for seconds, not the cooldown.
#[must_use]
pub fn classify(status: u16, body: &Value) -> Failure {
    let code = body["error"]["code"].as_str().unwrap_or_default();
    let text = message(status, body);
    match (status, code) {
        (_, "limit_reached") => Failure::Limited {
            reset_at: body["error"]["resetAt"]
                .as_str()
                .and_then(crate::util::parse_rfc3339),
            message: text,
        },
        (_, "too_many_pending" | "rate_limited") | (429, _) => Failure::Retryable(text),
        (_, "token_paused") => Failure::Disabled(format!("the bridge owner paused it: {text}")),
        (401, _) | (_, "unauthorized") => Failure::Disabled(format!(
            "the bridge no longer accepts its token (revoked or expired): {text}"
        )),
        (_, "loop") => Failure::Disabled(
            "it points back at this same Hashi; remove it and add the friend's bridge".to_owned(),
        ),
        (403, _) | (_, "forbidden") => {
            Failure::Disabled(format!("the bridge does not allow it: {text}"))
        }
        (s, _) if s >= 500 || s == 408 => Failure::Retryable(text),
        _ => Failure::Failed(text),
    }
}

/// A failed request's error on the bridge, as a [`Failure`]. When every
/// account behind the token was at its limit, the account counts as
/// limited until the time the bridge gave.
fn failed_run(error: &str) -> Failure {
    let error = crate::util::truncate(&neutralize_controls(error, true), 4_000);
    if error.starts_with("every account for ") && error.contains(" is at its limit") {
        let reset_at = error
            .rsplit_once(" until ")
            .and_then(|(_, t)| crate::util::parse_rfc3339(t));
        return Failure::Limited {
            reset_at,
            message: error,
        };
    }
    Failure::Failed(error)
}

fn usage_of(v: &Value) -> Option<Usage> {
    let u = v.get("usage").filter(|u| u.is_object())?;
    Some(Usage {
        input_tokens: u["inputTokens"].as_u64().unwrap_or(0),
        output_tokens: u["outputTokens"].as_u64().unwrap_or(0),
        cost_usd: u["costUsd"].as_f64().filter(|c| c.is_finite() && *c >= 0.0),
    })
}

/// The body of the `POST /v1/ask` that forwards a prompt.
fn submit_body(message: &str, model: Option<&str>, agent: AgentKind) -> Value {
    let mut body = json!({
        "message": message,
        "agent": agent,
    });
    if let Some(m) = model {
        body["model"] = json!(m);
    }
    body
}

/// Whether a prompt, forwarded to a remote bridge for any agent, fits that
/// bridge's [`crate::api::MAX_BODY_BYTES`]. A prompt that fits here can
/// still grow past it once re-encoded with the agent's name, so a token
/// that may run on a remote account is refused such a prompt up front with
/// the usual `413`, rather than sending it and failing there.
#[must_use]
pub fn fits_remote(message: &str, model: Option<&str>) -> bool {
    let longest = AgentKind::ALL
        .iter()
        .copied()
        .max_by_key(|a| a.as_str().len())
        .unwrap_or(AgentKind::Claude);
    serde_json::to_vec(&submit_body(message, model, longest))
        .is_ok_and(|b| b.len() <= crate::api::MAX_BODY_BYTES)
}

fn remaining(deadline: Instant) -> Option<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
}

/// Runs one prompt on the bridge and waits for its answer, polling until
/// it finishes, `handle` is stopped or the deadline passes. Blocking: call
/// it on a blocking thread.
///
/// # Errors
///
/// See [`Failure`].
pub fn forward(target: &Target, call: &Call<'_>, handle: &Handle) -> Result<Reply, Failure> {
    check_instance(target, instance_id)?;
    let auth = format!("Bearer {}", target.token);
    let hops = call.hops.saturating_add(1).to_string();
    let wait_for =
        |deadline: Instant| remaining(deadline).map(|d| d.as_secs().clamp(1, POLL_WAIT_SECS));
    if remaining(call.deadline).is_none() {
        return Err(Failure::TimedOut);
    }
    // No wait on the submit: the request id comes back at once, so a
    // cancel from here on can reach the bridge.
    let body = submit_body(call.message, call.model, target.agent);
    let agent = http(Duration::from_secs(30));
    let res = agent
        .post(&target.url("/v1/ask"))
        .header("Authorization", &auth)
        .header(HOPS_HEADER, &hops)
        .header(FROM_HEADER, call.from)
        .send_json(&body);
    let (status, mut v) = read(res, &target.remote)?;
    if status != 200 && status != 202 {
        return Err(classify(status, &v));
    }
    let id = v["id"].as_str().unwrap_or_default().to_owned();
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        return Err(Failure::Retryable(
            "the bridge answered without a request id".to_owned(),
        ));
    }
    handle.set_id(&id);
    loop {
        match v["status"].as_str() {
            Some("completed") => {
                handle.finish();
                let mut text =
                    neutralize_controls(v["response"].as_str().unwrap_or_default(), true);
                if text.len() > call.max_output {
                    let mut cut = call.max_output;
                    while !text.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    text.truncate(cut);
                }
                return Ok(Reply {
                    text,
                    usage: usage_of(&v),
                });
            }
            Some("failed") => {
                handle.finish();
                return Err(failed_run(
                    v["error"].as_str().unwrap_or("the agent failed"),
                ));
            }
            _ => {}
        }
        if handle.stopped() {
            // Stopped before the id was known to the owner of the handle.
            if let Some(id) = handle.stop() {
                cancel(target, &id);
            }
            return Err(Failure::Failed("cancelled".to_owned()));
        }
        let Some(wait) = wait_for(call.deadline) else {
            return Err(Failure::TimedOut);
        };
        let started = Instant::now();
        let res = http(Duration::from_secs(wait + 15))
            .get(&target.url(&format!("/v1/ask/{id}?wait={wait}")))
            .header("Authorization", &auth)
            .call();
        let (status, next) = read(res, &target.remote)?;
        if status != 200 && status != 202 {
            return Err(classify(status, &next));
        }
        v = next;
        // A bridge that cannot hold the wait answers at once; do not spin.
        if let Some(rest) = Duration::from_millis(250).checked_sub(started.elapsed()) {
            std::thread::sleep(rest);
        }
    }
}

/// Cancels request `id` on the bridge, best effort. Blocking.
pub fn cancel(target: &Target, id: &str) {
    let res = http(Duration::from_secs(10))
        .delete(&target.url(&format!("/v1/ask/{id}")))
        .header("Authorization", &format!("Bearer {}", target.token))
        .call();
    match res {
        Ok(r) if r.status().is_success() => {
            tracing::info!(remote = %target.remote, "cancelled the forwarded request on the bridge");
        }
        Ok(r) => tracing::info!(remote = %target.remote, status = r.status().as_u16(),
                                "the bridge did not cancel the forwarded request"),
        Err(e) => tracing::info!(remote = %target.remote, error = %e,
                                 "could not cancel the forwarded request on the bridge"),
    }
}

/// Checks that the bridge behind `target` is still the one its account was
/// added for. `lookup` is [`instance_id`]; tests pass their own. A different
/// id (or none, from a bridge that reported one before) switches the account
/// off; a bridge that cannot be reached is retried.
///
/// # Errors
///
/// [`Failure::Disabled`] on a mismatch, [`Failure::Retryable`] when the
/// bridge cannot be asked.
pub fn check_instance(
    target: &Target,
    lookup: impl FnOnce(&str) -> Result<Option<String>, String>,
) -> Result<(), Failure> {
    let Some(expected) = &target.instance else {
        return Ok(());
    };
    match lookup(&target.base_url) {
        Ok(Some(id)) if &id == expected => Ok(()),
        Ok(_) => Err(Failure::Disabled(format!(
            "the remote {} is no longer the bridge this account was added for; if that is \
             intended, remove the account and add it again",
            target.remote
        ))),
        Err(e) => Err(Failure::Retryable(format!(
            "could not check the bridge behind {}: {e}",
            target.remote
        ))),
    }
}

/// The instance id the bridge at `base_url` reports in `/v1/status`, or
/// `None` when it reports none (an older Hashi). Blocking.
///
/// # Errors
///
/// Fails when the bridge cannot be reached or answers badly.
pub fn instance_id(base_url: &str) -> Result<Option<String>, String> {
    let url = format!("{}/v1/status", base_url.trim_end_matches('/'));
    let mut res = http(Duration::from_secs(10))
        .get(&url)
        .call()
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("the bridge answered {}", res.status().as_u16()));
    }
    let v: Value = res.body_mut().read_json().map_err(|e| e.to_string())?;
    Ok(v["instance"].as_str().map(str::to_owned))
}

/// Reads the remote token's `GET /v1/me`. Blocking.
///
/// # Errors
///
/// See [`Failure`]; a paused or expired token answers normally, with its
/// status in the body.
pub fn read_me(target: &Target) -> Result<Value, Failure> {
    let res = http(Duration::from_secs(20))
        .get(&target.url("/v1/me"))
        .header("Authorization", &format!("Bearer {}", target.token))
        .call();
    let (status, v) = read(res, &target.remote)?;
    if status == 200 {
        Ok(v)
    } else {
        Err(classify(status, &v))
    }
}

/// What a remote token's `/v1/me` says about a remote account.
#[derive(Debug, Clone, PartialEq)]
pub struct MeReading {
    /// The windows to keep for the account (source `remote`).
    pub windows: Vec<Window>,
    /// Until when the account is at a limit, when it is.
    pub limited_until: Option<i64>,
    /// Whether the figures are the friend's shared headroom (rather than
    /// the token's own caps).
    pub shared: bool,
    /// What the account shows when there is nothing to report or the
    /// token cannot run.
    pub note: Option<String>,
}

fn pct(used: f64, max: f64) -> f64 {
    if max <= 0.0 {
        100.0
    } else {
        used / max * 100.0
    }
}

/// Folds a remote token's `/v1/me` into the account's usage: the shared
/// headroom for `agent` when the friend shares it, and one window per
/// request or budget cap the token has, resetting with the bridge's day or
/// month. A cap that is used up counts as a limit until its reset.
#[must_use]
pub fn me_windows(me: &Value, agent: AgentKind, remote: &str) -> MeReading {
    let mut out = MeReading {
        windows: Vec::new(),
        limited_until: None,
        shared: false,
        note: None,
    };
    let status = me["status"].as_str().unwrap_or("active");
    if status != "active" {
        out.note = Some(format!("the token on {remote} is {status}"));
    }
    if let Some(shared) = me["agentUsage"]
        .get(agent.as_str())
        .filter(|v| v.is_object())
    {
        out.shared = true;
        let reset = shared["nextReset"]
            .as_str()
            .and_then(crate::util::parse_rfc3339);
        let state = shared["state"].as_str().unwrap_or("unknown");
        let headroom = shared["headroom"].as_f64();
        let used = match (state, headroom) {
            ("limited", _) => Some(100.0),
            (_, Some(h)) => Some(100.0 - h),
            _ => None,
        };
        if state == "limited" {
            out.limited_until = reset;
        }
        match used {
            Some(used) => out.windows.push(Window::new(
                SHARED_WINDOW,
                format!("via {remote}"),
                used,
                reset,
                None,
                Source::Remote,
            )),
            None => {
                out.note.get_or_insert_with(|| {
                    format!("{remote} shares usage but has no figures for {agent} yet")
                });
            }
        }
    }
    let limits = &me["limits"];
    let usage = &me["usage"];
    let day = me["resets"]["day"]
        .as_str()
        .and_then(crate::util::parse_rfc3339);
    let month = me["resets"]["month"]
        .as_str()
        .and_then(crate::util::parse_rfc3339);
    let caps = [
        (
            "daily_requests",
            "requests today",
            "dailyRequests",
            "todayRequests",
            day,
            1_440,
        ),
        (
            "monthly_requests",
            "requests this month",
            "monthlyRequests",
            "monthRequests",
            month,
            43_200,
        ),
        (
            "daily_budget",
            "budget today",
            "dailyBudgetUsd",
            "todayCostUsd",
            day,
            1_440,
        ),
        (
            "monthly_budget",
            "budget this month",
            "monthlyBudgetUsd",
            "monthCostUsd",
            month,
            43_200,
        ),
    ];
    for (key, label, cap, used, reset, mins) in caps {
        let Some(max) = limits[cap].as_f64() else {
            continue;
        };
        let used = pct(usage[used].as_f64().unwrap_or(0.0), max);
        if used >= 100.0
            && let Some(r) = reset
            && out.limited_until.is_none_or(|u| u < r)
        {
            out.limited_until = Some(r);
        }
        out.windows.push(Window::new(
            key,
            label,
            used,
            reset,
            Some(mins),
            Source::Remote,
        ));
    }
    if out.windows.is_empty() && !out.shared {
        out.note
            .get_or_insert_with(|| format!("{remote} shares no usage and the token has no caps"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_forward_checks_the_bridge_is_the_one_the_account_was_added_for() {
        let remote = crate::remote::Remote {
            name: "bob".into(),
            base_url: "https://bob.example".into(),
            token: "hashi_x".into(),
        };
        let unbound = Target::new(&remote, AgentKind::Claude);
        assert!(check_instance(&unbound, |_| unreachable!("nothing to check")).is_ok());
        let bound = unbound.expecting(Some("abc".into()));
        assert!(
            check_instance(&bound, |url| {
                assert_eq!(url, "https://bob.example");
                Ok(Some("abc".into()))
            })
            .is_ok()
        );
        for answer in [Ok(Some("other".to_owned())), Ok(None)] {
            let r = check_instance(&bound, |_| answer.clone());
            assert!(
                matches!(&r, Err(Failure::Disabled(why)) if why.contains("no longer the bridge")),
                "{r:?}"
            );
        }
        assert!(matches!(
            check_instance(&bound, |_| Err("refused".into())),
            Err(Failure::Retryable(_))
        ));
    }

    #[test]
    fn refusals_map_to_what_the_account_should_do() {
        let body = |code: &str| json!({"error": {"code": code, "message": "m"}});
        assert!(matches!(
            classify(
                429,
                &json!({"error": {"code": "limit_reached", "message": "m",
                "resetAt": "2030-01-01T00:00:00Z"}})
            ),
            Failure::Limited {
                reset_at: Some(_),
                ..
            }
        ));
        assert!(matches!(
            classify(429, &body("limit_reached")),
            Failure::Limited { reset_at: None, .. }
        ));
        // Transient refusals one holder can cause never mark the account
        // limited for the cooldown.
        assert!(matches!(
            classify(429, &body("too_many_pending")),
            Failure::Retryable(_)
        ));
        assert!(matches!(
            classify(429, &body("rate_limited")),
            Failure::Retryable(_)
        ));
        assert!(matches!(classify(429, &json!({})), Failure::Retryable(_)));
        assert!(matches!(
            classify(403, &body("token_paused")),
            Failure::Disabled(_)
        ));
        assert!(matches!(
            classify(401, &body("unauthorized")),
            Failure::Disabled(_)
        ));
        assert!(matches!(
            classify(403, &body("forbidden")),
            Failure::Disabled(_)
        ));
        assert!(matches!(classify(508, &body("loop")), Failure::Disabled(_)));
        assert!(matches!(
            classify(508, &body("hop_limit")),
            Failure::Retryable(_)
        ));
        assert!(matches!(
            classify(503, &body("unavailable")),
            Failure::Retryable(_)
        ));
        assert!(matches!(
            classify(400, &body("bad_request")),
            Failure::Failed(_)
        ));
    }

    #[test]
    fn only_prompts_whose_forwarded_body_fits_the_far_cap_fit() {
        assert!(fits_remote("hello", Some("opus")));
        // Fits a 64 KB body here as `{"message":"..."}`, not once the agent
        // is added on the way out.
        let quotes = "\"".repeat(32_760);
        let here = serde_json::to_vec(&json!({ "message": quotes })).unwrap();
        assert!(here.len() <= crate::api::MAX_BODY_BYTES);
        assert!(!fits_remote(&quotes, None));
    }

    #[test]
    fn a_bridge_out_of_accounts_counts_as_a_limit() {
        let f = failed_run(
            "every account for Claude Code this token can use is at its limit until 2030-01-01T00:00:00Z",
        );
        assert_eq!(
            f,
            Failure::Limited {
                reset_at: crate::util::parse_rfc3339("2030-01-01T00:00:00Z"),
                message: "every account for Claude Code this token can use is at its limit until 2030-01-01T00:00:00Z".to_owned(),
            }
        );
        assert!(matches!(
            failed_run("agent exited with 1"),
            Failure::Failed(_)
        ));
    }

    #[test]
    fn the_target_never_shows_its_token() {
        let t = Target::new(
            &Remote {
                name: "bob".into(),
                base_url: "https://bob.example/".into(),
                token: "hashi_secret_token_value".into(),
            },
            AgentKind::Claude,
        );
        let shown = format!("{t:?}");
        assert!(!shown.contains("hashi_secret_token_value"), "{shown}");
        assert_eq!(t.base_url, "https://bob.example");
    }

    #[test]
    fn me_without_sharing_turns_caps_into_windows() {
        let me = json!({
            "status": "active",
            "limits": {"dailyRequests": 10, "monthlyBudgetUsd": 5.0},
            "usage": {"todayRequests": 10, "monthCostUsd": 1.0},
            "resets": {"day": "2030-01-02T00:00:00Z", "month": "2030-02-01T00:00:00Z"},
        });
        let r = me_windows(&me, AgentKind::Claude, "bob");
        assert!(!r.shared);
        assert_eq!(r.windows.len(), 2);
        assert!((r.windows[0].used_percent - 100.0).abs() < 1e-9);
        assert!((r.windows[1].used_percent - 20.0).abs() < 1e-9);
        assert_eq!(
            r.limited_until,
            crate::util::parse_rfc3339("2030-01-02T00:00:00Z")
        );
        let none = me_windows(
            &json!({"status": "paused", "limits": {}}),
            AgentKind::Claude,
            "bob",
        );
        assert!(none.windows.is_empty());
        assert!(none.note.unwrap().contains("paused"));
    }

    #[test]
    fn shared_headroom_becomes_one_window() {
        let me = json!({
            "status": "active",
            "limits": {},
            "usage": {},
            "agentUsage": {"claude": {"headroom": 62, "nextReset": "2030-01-01T00:00:00Z", "state": "ok"}},
        });
        let r = me_windows(&me, AgentKind::Claude, "bob");
        assert!(r.shared);
        assert_eq!(r.windows.len(), 1);
        assert_eq!(r.windows[0].label, "via bob");
        assert!((r.windows[0].used_percent - 38.0).abs() < 1e-9);
        assert_eq!(r.limited_until, None);
        let limited = json!({"agentUsage": {"claude": {"headroom": 0, "nextReset": "2030-01-01T00:00:00Z", "state": "limited"}}});
        let r = me_windows(&limited, AgentKind::Claude, "bob");
        assert!(r.limited_until.is_some());
    }
}
