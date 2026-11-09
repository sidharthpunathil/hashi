//! Pools: a host's Hashi that the people at a party lend their agents to.
//!
//! Members never need to be reachable. Each member's own Hashi keeps one
//! long poll open to the host (`POST /v1/pool/poll`), takes the prompts
//! meant for its accounts, runs them through its own queue with its own
//! token (so its own limits, stop and time hold, and its keys never
//! leave it), and posts the answer back (`POST /v1/pool/result`). Every
//! poll also carries the member's headroom per agent, which the host keeps
//! as the usage of that member's `pool` accounts, so `GET /v1/pool` shows
//! everyone's at one endpoint.
//!
//! This module is the host's side in memory: prompts waiting for each
//! member, and the runs waiting for their answers. A restart forgets
//! both; a run then fails over like any account that did not answer.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, oneshot};

use crate::agent::AgentKind;

/// A member whose worker has not asked for work for this long is offline:
/// no new prompts go to it.
pub const ONLINE_FOR: Duration = Duration::from_secs(75);

/// How long one poll waits for work before answering with none.
pub const POLL_WAIT: Duration = Duration::from_secs(25);

/// Prompts that may wait for one member before more are refused.
const MAX_WAITING: usize = 16;

/// One prompt for a member to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    /// Its id, which the answer names.
    pub id: String,
    /// The agent to run.
    pub agent: AgentKind,
    /// The model asked for, if any.
    pub model: Option<String>,
    /// The prompt.
    pub message: String,
}

/// What a member answered.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The run finished.
    Done {
        /// The answer.
        text: String,
        /// Tokens in, if reported.
        input_tokens: Option<u64>,
        /// Tokens out, if reported.
        output_tokens: Option<u64>,
        /// Cost in USD, if reported.
        cost_usd: Option<f64>,
    },
    /// The member is at a limit (its own, or its accounts'), until
    /// `reset_at` when known.
    Limited {
        /// When it can take runs again, Unix ms.
        reset_at: Option<i64>,
        /// Why, in the member's words.
        message: String,
    },
    /// The member could not take it now (busy, paused); try another.
    Busy(String),
    /// The run failed.
    Failed(String),
}

#[derive(Default)]
struct Member {
    jobs: VecDeque<Job>,
    notify: Arc<Notify>,
    seen: Option<Instant>,
}

#[derive(Default)]
struct Inner {
    members: HashMap<String, Member>,
    waiting: HashMap<String, (String, oneshot::Sender<Outcome>)>,
}

/// The host's prompts in flight to members.
#[derive(Default)]
pub struct Hub {
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("pool::Hub")
    }
}

impl Hub {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records that `member`'s worker is asking for work now.
    pub fn seen(&self, member: &str) {
        self.lock()
            .members
            .entry(member.to_owned())
            .or_default()
            .seen = Some(Instant::now());
    }

    /// Whether `member`'s worker asked for work within [`ONLINE_FOR`].
    #[must_use]
    pub fn online(&self, member: &str) -> bool {
        self.lock()
            .members
            .get(member)
            .and_then(|m| m.seen)
            .is_some_and(|s| s.elapsed() < ONLINE_FOR)
    }

    /// Queues `job` for `member` and returns where its answer arrives, or
    /// `None` when too many already wait for that member.
    #[must_use]
    pub fn submit(&self, member: &str, job: Job) -> Option<oneshot::Receiver<Outcome>> {
        let mut inner = self.lock();
        let m = inner.members.entry(member.to_owned()).or_default();
        if m.jobs.len() >= MAX_WAITING {
            return None;
        }
        let (tx, rx) = oneshot::channel();
        let id = job.id.clone();
        m.jobs.push_back(job);
        m.notify.notify_one();
        inner.waiting.insert(id, (member.to_owned(), tx));
        Some(rx)
    }

    /// Takes back a job that was not picked up yet, for a run that stopped
    /// waiting.
    pub fn withdraw(&self, job_id: &str) {
        let mut inner = self.lock();
        if let Some((member, _)) = inner.waiting.remove(job_id)
            && let Some(m) = inner.members.get_mut(&member)
        {
            m.jobs.retain(|j| j.id != job_id);
        }
    }

    /// The next job for `member`, waiting up to `wait` for one.
    pub async fn next(&self, member: &str, wait: Duration) -> Option<Job> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let notify = {
                let mut inner = self.lock();
                let m = inner.members.entry(member.to_owned()).or_default();
                m.seen = Some(Instant::now());
                if let Some(job) = m.jobs.pop_front() {
                    return Some(job);
                }
                Arc::clone(&m.notify)
            };
            if tokio::time::timeout_at(deadline, notify.notified())
                .await
                .is_err()
            {
                return None;
            }
        }
    }

    /// Hands `member`'s answer to the run waiting for `job_id`. Returns
    /// `false` when no such run waits for that member (it gave up, or the
    /// id is not theirs).
    pub fn answer(&self, member: &str, job_id: &str, outcome: Outcome) -> bool {
        let mut inner = self.lock();
        match inner.waiting.get(job_id) {
            Some((m, _)) if m == member => {}
            _ => return false,
        }
        let Some((_, tx)) = inner.waiting.remove(job_id) else {
            return false;
        };
        tx.send(outcome).is_ok()
    }

    /// Forgets `member`: prompts still waiting for it fail, so their runs
    /// move on.
    pub fn forget(&self, member: &str) {
        let mut inner = self.lock();
        let Some(m) = inner.members.remove(member) else {
            return;
        };
        for job in m.jobs {
            if let Some((_, tx)) = inner.waiting.remove(&job.id) {
                let _ = tx.send(Outcome::Busy("left the pool".to_owned()));
            }
        }
    }
}

/// This machine's address on the local network: the one it would reach
/// the internet from. No packet is sent.
#[must_use]
pub fn lan_ip() -> Option<std::net::IpAddr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

/// Whether `host` is on a private network: RFC 1918 and link-local IPv4,
/// IPv6 unique-local and link-local, or an mDNS `.local` name. A pool
/// link may use plain `http://` there, for a party on one wifi.
#[must_use]
pub fn is_lan_host(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    match h.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => v4.is_private() || v4.is_link_local(),
        Ok(std::net::IpAddr::V6(v6)) => {
            let first = v6.segments()[0];
            (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
        Err(_) => {
            let lower = h.to_ascii_lowercase();
            lower.ends_with(".local") && lower.len() > ".local".len()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lan_hosts_are_private_ones_only() {
        for ok in [
            "192.168.1.4",
            "10.0.0.2",
            "172.20.1.1",
            "studio.local",
            "[fd12::1]",
        ] {
            assert!(is_lan_host(ok), "{ok}");
        }
        for no in ["8.8.8.8", "example.com", "172.32.0.1", ".local", "local"] {
            assert!(!is_lan_host(no), "{no}");
        }
    }

    fn job(id: &str) -> Job {
        Job {
            id: id.to_owned(),
            agent: AgentKind::Claude,
            model: None,
            message: "hi".to_owned(),
        }
    }

    #[tokio::test]
    async fn a_job_goes_to_its_member_and_the_answer_comes_back() {
        let hub = Hub::default();
        assert!(!hub.online("m"));
        assert!(hub.next("m", Duration::from_millis(20)).await.is_none());
        assert!(hub.online("m"));
        let rx = hub.submit("m", job("1")).unwrap();
        let got = hub.next("m", Duration::from_secs(1)).await.unwrap();
        assert_eq!(got.id, "1");
        // Only the member it went to can answer.
        assert!(!hub.answer("other", "1", Outcome::Failed("x".into())));
        assert!(hub.answer("m", "1", Outcome::Busy("paused".into())));
        assert_eq!(rx.await.unwrap(), Outcome::Busy("paused".into()));
        assert!(!hub.answer("m", "1", Outcome::Busy("again".into())));
    }

    #[tokio::test]
    async fn a_waiting_poll_wakes_for_new_work() {
        let hub = Arc::new(Hub::default());
        let h = Arc::clone(&hub);
        let poll = tokio::spawn(async move { h.next("m", Duration::from_secs(5)).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _rx = hub.submit("m", job("2")).unwrap();
        assert_eq!(poll.await.unwrap().unwrap().id, "2");
    }

    #[tokio::test]
    async fn withdrawn_and_forgotten_jobs_never_run() {
        let hub = Hub::default();
        let _rx = hub.submit("m", job("a")).unwrap();
        hub.withdraw("a");
        assert!(hub.next("m", Duration::from_millis(20)).await.is_none());
        let rx = hub.submit("m", job("b")).unwrap();
        hub.forget("m");
        assert!(matches!(rx.await.unwrap(), Outcome::Busy(_)));
    }
}
