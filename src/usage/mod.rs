//! How much of each account's quota is left, and choosing accounts by it.
//!
//! Usage arrives three ways, and each kept window says which:
//!
//! - **poll**: a background read of the plan's live quota, for Claude Code
//!   (its OAuth usage endpoint, see [`claude`]) and Codex (`codex
//!   app-server`, see [`codex`]). See [`poller`].
//! - **run**: a Claude Code run's `rate_limit_event` messages.
//! - **error**: a run that failed with the agent's limit message, which
//!   counts as the whole window used until it resets.
//! - **remote**: a remote account's `GET /v1/me` on the friend's bridge:
//!   the headroom the friend shares (`shareUsage`), or else the remote
//!   token's own request and budget caps. Read on the poll cadence.
//!
//! [`summarize`] folds an account's windows into a [`Summary`]: the
//! headroom (100 minus the highest used percent), the window that binds,
//! the next reset and a [`UsageState`]. [`choose`] picks the account a run
//! uses under a [`Strategy`] and the rotate threshold, as a pure function
//! over those summaries, and [`keep_session`] says whether a stateful
//! conversation may stay where it is.
//!
//! Hashi never refreshes, stores, logs or returns an access token. Tokens
//! are read only to call the usage endpoint, and only from where the agent
//! itself keeps them.

pub mod claude;
pub mod codex;
pub mod poller;

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::agent::AgentKind;

pub use poller::{Poller, UsageSettings};

/// Default rotate threshold, in percent used.
pub const DEFAULT_ROTATE_AT: u8 = 90;

/// Lowest rotate threshold accepted.
pub const MIN_ROTATE_AT: u8 = 50;

/// Highest rotate threshold accepted.
pub const MAX_ROTATE_AT: u8 = 99;

/// A window with no reset time is trusted for this long after it was seen.
pub const UNDATED_FOR: Duration = Duration::from_secs(6 * 3_600);

/// How the account a run uses is chosen from its ordered list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Strategy {
    /// The first available account in the list, failing over on limits.
    #[default]
    InOrder,
    /// The available account with the most headroom.
    MostLeft,
    /// The account whose weekly window resets soonest, so quota that would
    /// expire unused is spent first.
    SoonestReset,
}

impl Strategy {
    /// Every strategy.
    pub const ALL: [Self; 3] = [Self::InOrder, Self::MostLeft, Self::SoonestReset];

    /// Stable identifier used in the database, the API and on the command
    /// line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InOrder => "in-order",
            Self::MostLeft => "most-left",
            Self::SoonestReset => "soonest-reset",
        }
    }
}

impl fmt::Display for Strategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Strategy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "in-order" | "in_order" | "inorder" => Ok(Self::InOrder),
            "most-left" | "most_left" => Ok(Self::MostLeft),
            "soonest-reset" | "soonest_reset" => Ok(Self::SoonestReset),
            other => Err(format!(
                "unknown account strategy `{}` (expected in-order, most-left or soonest-reset)",
                crate::util::truncate(other, 40)
            )),
        }
    }
}

/// Checks a rotate threshold: 50 to 99 percent.
///
/// # Errors
///
/// Returns a message saying what is wrong.
pub fn check_rotate_at(pct: u8) -> Result<u8, String> {
    if (MIN_ROTATE_AT..=MAX_ROTATE_AT).contains(&pct) {
        Ok(pct)
    } else {
        Err(format!(
            "the rotate threshold must be {MIN_ROTATE_AT} to {MAX_ROTATE_AT} percent, not {pct}"
        ))
    }
}

/// Where a usage window came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// A background or requested poll of the live quota.
    Poll,
    /// A run's own rate-limit messages.
    Run,
    /// A run that failed with the agent's limit message.
    Error,
    /// A remote account's `/v1/me` on the friend's bridge.
    Remote,
}

impl Source {
    /// `poll`, `run`, `error` or `remote`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Poll => "poll",
            Self::Run => "run",
            Self::Error => "error",
            Self::Remote => "remote",
        }
    }
}

impl FromStr for Source {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "poll" => Ok(Self::Poll),
            "run" => Ok(Self::Run),
            "error" => Ok(Self::Error),
            "remote" => Ok(Self::Remote),
            other => Err(format!("unknown usage source `{other}`")),
        }
    }
}

/// One quota window of one account, as last seen.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Window {
    /// Stable key, such as `five_hour`, `seven_day_opus` or `primary`.
    pub key: String,
    /// For people: "5 hours", "week", "week, Opus".
    pub label: String,
    /// Share of the window used, 0 to 100.
    pub used_percent: f64,
    /// When the window resets, Unix ms.
    #[serde(serialize_with = "ser_time")]
    pub resets_at: Option<i64>,
    /// The window's length in minutes, when known.
    pub window_mins: Option<i64>,
    /// Where the figure came from.
    pub source: Source,
    /// When it was seen, Unix ms.
    #[serde(serialize_with = "ser_time")]
    pub observed_at: Option<i64>,
}

fn ser_time<S: serde::Serializer>(t: &Option<i64>, s: S) -> Result<S::Ok, S::Error> {
    match t.and_then(crate::util::rfc3339) {
        Some(v) => s.serialize_str(&v),
        None => s.serialize_none(),
    }
}

impl Window {
    /// A window seen now.
    #[must_use]
    pub fn new(
        key: impl Into<String>,
        label: impl Into<String>,
        used_percent: f64,
        resets_at: Option<i64>,
        window_mins: Option<i64>,
        source: Source,
    ) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            used_percent: clamp_percent(used_percent),
            resets_at,
            window_mins,
            source,
            observed_at: Some(crate::util::now_ms()),
        }
    }

    /// Whether the window is a week or longer.
    #[must_use]
    pub fn is_weekly(&self) -> bool {
        self.window_mins.is_some_and(|m| m >= 6 * 1_440) || self.key.starts_with("seven_day")
    }

    /// The share used at `now`: nothing once the window has reset, and
    /// `None` when it is too old to trust (no reset time, seen more than
    /// [`UNDATED_FOR`] ago).
    #[must_use]
    pub fn used_at(&self, now: i64) -> Option<f64> {
        match self.resets_at {
            Some(r) if r <= now => Some(0.0),
            Some(_) => Some(self.used_percent),
            None => {
                let undated = i64::try_from(UNDATED_FOR.as_millis()).unwrap_or(i64::MAX);
                self.observed_at
                    .is_some_and(|t| now.saturating_sub(t) <= undated)
                    .then_some(self.used_percent)
            }
        }
    }
}

/// How far past a token's stop an account is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PastStop {
    /// The highest used percent among the windows at or over the stop.
    pub used: f64,
    /// When every one of those windows has reset, Unix ms, when known.
    pub opens_at: Option<i64>,
}

/// Whether `windows` say an account has used `stop` percent or more of
/// any window at `now`. Unknown usage is never past the stop: a token is
/// only held back on figures Hashi has.
#[must_use]
pub fn past_stop(windows: &[Window], stop: u8, now: i64) -> Option<PastStop> {
    let over: Vec<(f64, Option<i64>)> = windows
        .iter()
        .filter_map(|w| w.used_at(now).map(|u| (u, w.resets_at)))
        .filter(|(u, _)| *u >= f64::from(stop))
        .collect();
    if over.is_empty() {
        return None;
    }
    let used = over.iter().map(|(u, _)| *u).fold(0.0, f64::max);
    let opens_at = over
        .iter()
        .map(|(_, r)| *r)
        .try_fold(i64::MIN, |acc, r| r.map(|r| acc.max(r)));
    Some(PastStop { used, opens_at })
}

/// What a token holder is told when the owner's quota is past their
/// token's stop: no account names, only the agent, the stop and when it
/// opens again.
#[must_use]
pub fn stop_message(agent: AgentKind, stop: u8, held: &PastStop) -> String {
    let agent = agent.display_name();
    let base = format!(
        "the owner keeps the rest of their {agent} quota: this token stops at {stop}% \
         and it is at {:.0}%",
        held.used
    );
    match held.opens_at.and_then(crate::util::rfc3339) {
        Some(at) => format!("{base}; it opens again at {at}"),
        None => format!("{base}; try again later"),
    }
}

/// `pct` within 0 to 100, with anything not a number as 0.
#[must_use]
pub fn clamp_percent(pct: f64) -> f64 {
    if pct.is_finite() {
        pct.clamp(0.0, 100.0)
    } else {
        0.0
    }
}

/// An account's quota state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsageState {
    /// Under the rotate threshold.
    Ok,
    /// At or above the rotate threshold.
    Warning,
    /// At its limit, until [`Summary::limited_until`] when known.
    Limited,
    /// No live figures.
    Unknown,
}

impl UsageState {
    /// `ok`, `warning`, `limited` or `unknown`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warning => "warning",
            Self::Limited => "limited",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for UsageState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One account's windows folded into the figures that drive choices.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    /// 100 minus the highest used percent across windows, or `None` when
    /// unknown.
    pub headroom: Option<f64>,
    /// Label of the window with that highest used percent.
    pub binding: Option<String>,
    /// The soonest reset of any window still running, Unix ms.
    #[serde(serialize_with = "ser_time")]
    pub next_reset: Option<i64>,
    /// When the most used weekly window resets, Unix ms.
    #[serde(serialize_with = "ser_time")]
    pub weekly_reset: Option<i64>,
    /// The state.
    pub state: UsageState,
    /// Until when the account is limited, Unix ms, when it is and that is
    /// known.
    #[serde(serialize_with = "ser_time")]
    pub limited_until: Option<i64>,
}

impl Summary {
    /// A summary with nothing known.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            headroom: None,
            binding: None,
            next_reset: None,
            weekly_reset: None,
            state: UsageState::Unknown,
            limited_until: None,
        }
    }

    /// The highest used percent, when known.
    #[must_use]
    pub fn used(&self) -> Option<f64> {
        self.headroom.map(|h| 100.0 - h)
    }

    /// Headroom rounded down to a whole percent, for display and events.
    #[must_use]
    pub fn headroom_pct(&self) -> Option<u8> {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        self.headroom.map(|h| clamp_percent(h).floor() as u8)
    }

    /// Whether the account is at or above `rotate_at` percent used.
    #[must_use]
    pub fn over(&self, rotate_at: u8) -> bool {
        self.used().is_some_and(|u| u >= f64::from(rotate_at))
    }
}

/// Folds `windows` into a [`Summary`] at `now`. `limited_until` is the
/// account's own limited mark (from a failed run or a poll), which wins
/// over the figures while it lasts. A window at 100% is a limit until it
/// resets.
#[must_use]
pub fn summarize(
    windows: &[Window],
    limited_until: Option<i64>,
    rotate_at: u8,
    now: i64,
) -> Summary {
    let mut out = Summary::unknown();
    let mut highest: Option<(f64, &Window)> = None;
    let mut weekly: Option<(f64, i64)> = None;
    let mut full_until: Option<i64> = None;
    for w in windows {
        let Some(used) = w.used_at(now) else {
            continue;
        };
        if highest.is_none_or(|(u, _)| used > u) {
            highest = Some((used, w));
        }
        if let Some(r) = w.resets_at.filter(|r| *r > now) {
            out.next_reset = Some(out.next_reset.map_or(r, |n| n.min(r)));
            if w.is_weekly() && weekly.is_none_or(|(u, _)| used > u) {
                weekly = Some((used, r));
            }
            if used >= 100.0 {
                full_until = Some(full_until.map_or(r, |f| f.max(r)));
            }
        }
    }
    if let Some((used, w)) = highest {
        out.headroom = Some(100.0 - used);
        out.binding = Some(w.label.clone());
        out.state = if used >= f64::from(rotate_at) {
            UsageState::Warning
        } else {
            UsageState::Ok
        };
    }
    out.weekly_reset = weekly.map(|(_, r)| r);
    let marked = limited_until.filter(|u| *u > now);
    if marked.is_some() || full_until.is_some() {
        out.state = UsageState::Limited;
        out.limited_until = match (marked, full_until) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
    }
    out
}

/// One account a run could use, in list order.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// Enabled, not limited by its mark, and not tried yet for this request.
    pub available: bool,
    /// Its usage.
    pub summary: Summary,
}

/// The index of the account a run uses under `strategy`, or `None` when no
/// account is available.
///
/// Common rules: limited and unavailable accounts are never picked; an
/// account at or above `rotate_at` is skipped while any account is under
/// it; when every account is over, the one with the most headroom runs
/// rather than none.
///
/// - [`Strategy::InOrder`]: the first account under the threshold.
/// - [`Strategy::MostLeft`]: the most headroom; unknown headroom ranks
///   after every known account under the threshold. List order breaks
///   ties.
/// - [`Strategy::SoonestReset`]: the account whose weekly window resets
///   soonest, among those under the threshold whose weekly reset is known;
///   otherwise as [`Strategy::MostLeft`].
#[must_use]
pub fn choose(strategy: Strategy, candidates: &[Candidate], rotate_at: u8) -> Option<usize> {
    let pool: Vec<usize> = candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| c.available && c.summary.state != UsageState::Limited)
        .map(|(i, _)| i)
        .collect();
    let under: Vec<usize> = pool
        .iter()
        .copied()
        .filter(|i| !candidates[*i].summary.over(rotate_at))
        .collect();
    if under.is_empty() {
        return most_left(candidates, &pool);
    }
    match strategy {
        Strategy::InOrder => under.first().copied(),
        Strategy::MostLeft => most_left(candidates, &under),
        Strategy::SoonestReset => under
            .iter()
            .copied()
            .filter_map(|i| candidates[i].summary.weekly_reset.map(|r| (r, i)))
            .min()
            .map(|(_, i)| i)
            .or_else(|| most_left(candidates, &under)),
    }
}

/// Of `among` (indexes in list order), the one with the most headroom;
/// unknown headroom after every known one, list order breaking ties.
fn most_left(candidates: &[Candidate], among: &[usize]) -> Option<usize> {
    let known = among
        .iter()
        .copied()
        .filter_map(|i| candidates[i].summary.headroom.map(|h| (h, i)))
        .fold(None::<(f64, usize)>, |best, (h, i)| match best {
            Some((bh, _)) if bh >= h => best,
            _ => Some((h, i)),
        });
    known.map(|(_, i)| i).or_else(|| among.first().copied())
}

/// Whether a stateful conversation may stay on its account: the account is
/// available, not limited and under the threshold.
#[must_use]
pub fn keep_session(candidate: &Candidate, rotate_at: u8) -> bool {
    candidate.available
        && candidate.summary.state != UsageState::Limited
        && !candidate.summary.over(rotate_at)
}

/// The strategy and threshold that apply when a project sets none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Account strategy.
    pub strategy: Strategy,
    /// Rotate threshold, percent used.
    pub rotate_at: u8,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            strategy: Strategy::InOrder,
            rotate_at: DEFAULT_ROTATE_AT,
        }
    }
}

/// An account, as `(agent, name)`.
pub type Key = (AgentKind, String);

/// Shared usage state of a running server: the default policy, the
/// poller (when there is one), wake-ups after runs and the last state published per account. Lives in the
/// [`crate::queue::Dispatcher`], so everything that can reach the queue
/// can reach it.
#[derive(Debug, Default)]
pub struct Hub {
    policy: RwLock<Policy>,
    poller: Mutex<Option<Arc<Poller>>>,
    after_run: Mutex<Option<tokio::sync::mpsc::UnboundedSender<Key>>>,
    published: Mutex<HashMap<Key, (UsageState, Option<u8>)>>,
}

impl Hub {
    /// The server's default strategy and threshold.
    #[must_use]
    pub fn policy(&self) -> Policy {
        *self.policy.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sets the server's default strategy and threshold.
    pub fn set_policy(&self, policy: Policy) {
        *self.policy.write().unwrap_or_else(PoisonError::into_inner) = policy;
    }

    /// The poller, once the server has made one.
    #[must_use]
    pub fn poller(&self) -> Option<Arc<Poller>> {
        self.poller
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Installs the poller and the channel that asks it to poll after runs.
    pub fn install(
        &self,
        poller: Arc<Poller>,
        after_run: Option<tokio::sync::mpsc::UnboundedSender<Key>>,
    ) {
        *self.poller.lock().unwrap_or_else(PoisonError::into_inner) = Some(poller);
        *self
            .after_run
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = after_run;
    }

    /// Asks the background poller to read `agent:name` again soon, after a
    /// run on it finished. Does nothing without one.
    pub fn ran(&self, agent: AgentKind, name: &str) {
        if let Some(tx) = self
            .after_run
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            let _ = tx.send((agent, name.to_owned()));
        }
    }

    /// Records the state now published for an account and returns the one
    /// before it, or `Err(())` when nothing changed.
    #[allow(clippy::result_unit_err)]
    pub fn note(
        &self,
        agent: AgentKind,
        name: &str,
        state: UsageState,
        headroom: Option<u8>,
    ) -> Result<Option<UsageState>, ()> {
        let mut map = self
            .published
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let key = (agent, name.to_owned());
        let before = map.get(&key).copied();
        if before == Some((state, headroom)) {
            return Err(());
        }
        map.insert(key, (state, headroom));
        Ok(before.map(|(s, _)| s))
    }
}

/// Reads an account's windows and mark from `store`, summarises them under
/// `rotate_at` and publishes [`crate::events::Event::AccountUsage`] on the
/// bus when the state or whole-percent headroom changed since the last one.
pub fn publish(
    store: &crate::store::Store,
    bus: &crate::events::Bus,
    hub: &Hub,
    agent: AgentKind,
    name: &str,
) {
    let now = crate::util::now_ms();
    let rotate_at = hub.policy().rotate_at;
    let summary = match store.account_summary(agent, name, rotate_at, now) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(agent = %agent, error = %e, "could not read an account's usage");
            return;
        }
    };
    if let Ok(previous) = hub.note(agent, name, summary.state, summary.headroom_pct()) {
        bus.emit(crate::events::Event::AccountUsage {
            agent,
            account: name.to_owned(),
            state: summary.state,
            previous,
            headroom: summary.headroom_pct(),
            until: summary.limited_until,
        });
    }
}

/// A window length for people: "5 hours", "week", "30 days".
#[must_use]
pub fn duration_label(mins: i64) -> String {
    match mins {
        10_080 => "week".to_owned(),
        m if m > 0 && m % 1_440 == 0 => {
            let d = m / 1_440;
            if d == 1 {
                "day".to_owned()
            } else {
                format!("{d} days")
            }
        }
        m if m > 0 && m % 60 == 0 => {
            let h = m / 60;
            if h == 1 {
                "hour".to_owned()
            } else {
                format!("{h} hours")
            }
        }
        m => format!("{m} minutes"),
    }
}

#[cfg(test)]
mod tests;
