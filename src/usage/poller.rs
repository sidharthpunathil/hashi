//! The background usage poller.
//!
//! It reads the live quota of every Claude Code and Codex account that can
//! take runs: the agent's default account, and each login account bound to
//! at least one token or project. Each account is read every
//! [`UsageSettings::interval`] (five minutes) plus up to a minute of
//! jitter, and again shortly after each finished run on it, but never more
//! than once a minute. A 429 or 403 waits for `retry-after` (five minutes
//! without one, an hour at most); a rejected token waits an hour.
//!
//! A read is skipped while a run on the same account is active: Codex may
//! refresh its token during a read, and two refreshes must never race.
//!
//! Remote accounts, of any agent, are read the same way and on the same
//! cadence: `GET /v1/me` on the friend's bridge with the remote's token
//! (see [`crate::forward::me_windows`]). Their figures have source
//! `remote`.
//!
//! Nothing here logs a token. Errors are redacted before they are stored or
//! logged.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use super::{Hub, Key, Policy, claude, codex};
use crate::accounts::{self as acct, AccountKind};
use crate::agent::AgentKind;
use crate::events::Bus;
use crate::store::Store;
use crate::util::now_ms;

/// The shortest gap between two reads of one account.
pub const MIN_GAP: Duration = Duration::from_secs(60);

/// How often the loop looks for accounts that are due.
const TICK: Duration = Duration::from_secs(15);

/// Usage settings of a server.
#[derive(Debug, Clone)]
pub struct UsageSettings {
    /// Poll in the background. Runs and their errors are recorded either
    /// way.
    pub poll: bool,
    /// Time between two background reads of one account.
    pub interval: Duration,
    /// Base URL of the Claude usage endpoint.
    pub claude_base_url: String,
    /// The macOS `security` executable, or `None` to read only
    /// `.credentials.json`.
    pub security_bin: Option<PathBuf>,
    /// The Keychain account name, `$USER`.
    pub keychain_user: Option<String>,
    /// The default Claude config dir, `~/.claude`, where its
    /// `.credentials.json` lives when there is no Keychain item.
    pub claude_home: Option<PathBuf>,
    /// The Codex executable for `codex app-server`; `None` uses the agent's.
    pub codex_bin: Option<PathBuf>,
    /// The default account's `CODEX_HOME`.
    pub codex_home: Option<PathBuf>,
    /// The strategy and threshold when a project sets none.
    pub policy: Policy,
}

impl Default for UsageSettings {
    /// Polling off, the real endpoint and locations otherwise. Tests set
    /// their own fakes before asking for a read.
    fn default() -> Self {
        Self {
            poll: false,
            interval: Duration::from_secs(5 * 60),
            claude_base_url: claude::DEFAULT_BASE_URL.to_owned(),
            security_bin: cfg!(target_os = "macos").then(|| PathBuf::from("/usr/bin/security")),
            keychain_user: std::env::var("USER").ok().filter(|u| !u.is_empty()),
            claude_home: dirs::home_dir().map(|h| h.join(".claude")),
            codex_bin: None,
            codex_home: codex::default_home(),
            policy: Policy::default(),
        }
    }
}

/// Why a read gave no figures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollError {
    /// Read less than a minute ago: try again after this long.
    TooSoon(Duration),
    /// A run on the account is active.
    Busy,
    /// This account has no live usage (an API key, or another agent).
    NotSupported(String),
    /// The read failed. `note` is what the account shows; the next read is
    /// after `retry`.
    Failed {
        /// What the account shows.
        note: String,
        /// When to try again.
        retry: Duration,
    },
}

impl std::fmt::Display for PollError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooSoon(d) => write!(
                f,
                "read less than a minute ago; try again in {}s",
                d.as_secs().max(1)
            ),
            Self::Busy => f.write_str("a run on this account is active; it is read after the run"),
            Self::NotSupported(why) | Self::Failed { note: why, .. } => f.write_str(why),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Slot {
    last: Option<Instant>,
    due: Instant,
}

/// Reads usage for a server, or once for `hashi account usage --refresh`.
#[derive(Debug)]
pub struct Poller {
    store: Arc<Store>,
    bus: Bus,
    hub: Weak<Hub>,
    home: PathBuf,
    settings: UsageSettings,
    codex_bin: Option<PathBuf>,
    agents: Vec<AgentKind>,
    slots: Mutex<HashMap<Key, Slot>>,
}

/// Up to `max` of random delay.
fn jitter(max: Duration) -> Duration {
    let ms = u64::try_from(max.as_millis()).unwrap_or(0);
    if ms == 0 {
        return Duration::ZERO;
    }
    Duration::from_millis(rand::random::<u64>() % ms)
}

impl Poller {
    /// A poller for `agents` (only Claude Code and Codex have live usage).
    /// `codex_bin` is the configured Codex executable.
    #[must_use]
    pub fn new(
        store: Arc<Store>,
        bus: Bus,
        hub: &Arc<Hub>,
        home: PathBuf,
        settings: UsageSettings,
        codex_bin: Option<PathBuf>,
        agents: &[AgentKind],
    ) -> Self {
        Self {
            store,
            bus,
            hub: Arc::downgrade(hub),
            home,
            codex_bin: settings.codex_bin.clone().or(codex_bin),
            settings,
            agents: agents.iter().copied().filter(|a| supports(*a)).collect(),
            slots: Mutex::default(),
        }
    }

    /// The accounts the background loop reads: each supported agent's
    /// default account, all of its enabled login accounts, and every enabled
    /// remote account, whatever its agent. Accounts are read even before a
    /// token or project uses them, so the owner sees what is left while
    /// deciding where to put them.
    #[must_use]
    pub fn eligible(&self) -> Vec<Key> {
        let mut out = Vec::new();
        let accounts = self.store.list_accounts().unwrap_or_default();
        for agent in &self.agents {
            out.push((*agent, acct::DEFAULT.to_owned()));
            out.extend(
                accounts
                    .iter()
                    .filter(|a| a.agent == *agent && a.kind == AccountKind::Login && !a.disabled)
                    .map(|a| (*agent, a.name.clone())),
            );
        }
        // Remote accounts, and logins someone shared, are read through the
        // friend's bridge whatever their agent.
        out.extend(
            accounts
                .iter()
                .filter(|a| (a.kind == AccountKind::Remote || a.shared_by.is_some()) && !a.disabled)
                .map(|a| (a.agent, a.name.clone())),
        );
        out
    }

    /// Reads one account's usage now, records it and publishes the change.
    ///
    /// # Errors
    ///
    /// See [`PollError`]. A failure is also recorded as the account's note.
    pub async fn poll(&self, agent: AgentKind, name: &str) -> Result<(), PollError> {
        let key = (agent, name.to_owned());
        {
            let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
            let slot = slots.entry(key.clone()).or_insert(Slot {
                last: None,
                due: Instant::now(),
            });
            if let Some(last) = slot.last
                && last.elapsed() < MIN_GAP
            {
                return Err(PollError::TooSoon(MIN_GAP.saturating_sub(last.elapsed())));
            }
        }
        let state = if name == acct::DEFAULT {
            None
        } else {
            match self.store.get_account(agent, name) {
                Ok(Some(a)) => Some(a),
                Ok(None) => {
                    return Err(PollError::NotSupported(format!(
                        "no {agent} account named {name}"
                    )));
                }
                Err(e) => {
                    return Err(PollError::Failed {
                        note: e.to_string(),
                        retry: MIN_GAP,
                    });
                }
            }
        };
        let before = self.used(agent, name);
        if let Some(a) = state
            .as_ref()
            .filter(|a| a.kind == AccountKind::Remote || a.shared_by.is_some())
        {
            self.mark(&key, None);
            let result = self.read_remote(a).await;
            self.finish(&key, agent, name, &result);
            if result.is_ok() {
                self.remind(agent, name, before);
            }
            return result;
        }
        if !supports(agent) {
            return Err(PollError::NotSupported(format!(
                "{} has no live usage to read",
                agent.display_name()
            )));
        }
        let kind = if name == acct::DEFAULT {
            AccountKind::Default
        } else {
            match state {
                Some(a) => a.kind,
                None => {
                    return Err(PollError::NotSupported(format!(
                        "no {agent} account named {name}"
                    )));
                }
            }
        };
        if !matches!(kind, AccountKind::Default | AccountKind::Login) {
            return Err(PollError::NotSupported(format!(
                "{agent}:{name} is a {kind} account; only sign-ins have a plan quota to read"
            )));
        }
        if self.store.account_busy(agent, name).unwrap_or(false) {
            return Err(PollError::Busy);
        }
        self.mark(&key, None);
        let result = match agent {
            AgentKind::Claude => self.read_claude(name, kind).await,
            _ => self.read_codex(name, kind).await,
        };
        self.finish(&key, agent, name, &result);
        if result.is_ok() {
            self.remind(agent, name, before);
        }
        result
    }

    /// The highest used percent of an account's windows now, when known.
    fn used(&self, agent: AgentKind, name: &str) -> Option<f64> {
        self.store
            .account_summary(agent, name, 100, now_ms())
            .ok()
            .and_then(|s| s.used())
    }

    /// Reminds about logins shared either way once a read takes an
    /// account from under the agreed percent to at or over it: the person
    /// holding a shared login, and the owner who shared one whose quota
    /// this account is.
    fn remind(&self, agent: AgentKind, name: &str, before: Option<f64>) {
        let after = self.used(agent, name);
        let shown = after.unwrap_or_default();
        if let Ok(Some(a)) = self.store.get_account(agent, name)
            && let (Some(by), Some(pct)) = (&a.shared_by, a.remind_at)
            && crate::share::crossed(before, after, pct)
        {
            self.bus.log(format!(
                "reminder: {agent}:{name} is at {shown:.0}% of {by}'s quota; \
                 they asked you to stop at {pct}%"
            ));
        }
        let now = now_ms();
        for t in self.store.list_tokens().unwrap_or_default() {
            let (Some(login), Some(pct)) = (&t.login_account, t.remind_at) else {
                continue;
            };
            if t.status(now) != crate::token::TokenStatus::Active
                || !acct::for_agent(&t.accounts, agent)
                    .iter()
                    .any(|n| n == name)
                || !t.allows(agent)
                || !crate::share::crossed(before, after, pct)
            {
                continue;
            }
            self.bus.log(format!(
                "reminder: {agent}:{name} is at {shown:.0}% and {} holds your login {login}; \
                 you agreed they stop at {pct}%",
                t.name
            ));
        }
    }

    /// Records a failed read as the account's note and schedules the
    /// retry, then publishes the account's usage.
    fn finish(&self, key: &Key, agent: AgentKind, name: &str, result: &Result<(), PollError>) {
        if let Err(PollError::Failed { note, retry }) = result {
            let note = crate::util::truncate(&crate::diagnostics::redact(note), 200);
            tracing::info!(agent = %agent, account = %name, "usage unknown: {note}");
            if let Err(e) = self
                .store
                .set_poll_status(agent, name, false, None, Some(&note))
            {
                tracing::warn!(error = %e, "could not record a usage read");
            }
            self.mark(key, Some(*retry));
        }
        if let Some(hub) = self.hub.upgrade() {
            super::publish(&self.store, &self.bus, &hub, agent, name);
        }
    }

    /// Reads a remote account's `/v1/me` on the friend's bridge and keeps
    /// what it says as the account's usage (source `remote`): the shared
    /// headroom, or the token's own caps. The token is never stored or
    /// logged.
    async fn read_remote(&self, a: &crate::store::AccountState) -> Result<(), PollError> {
        let remote = a
            .remote
            .clone()
            .or_else(|| a.shared_by.clone())
            .unwrap_or_default();
        let target =
            crate::forward::Target::load(&self.home, &remote, a.remote_agent.unwrap_or(a.agent))
                .map_err(|why| PollError::Failed {
                    note: format!("unknown, {why}"),
                    retry: self.settings.interval,
                })?;
        let remote_agent = target.agent;
        let me = tokio::task::spawn_blocking(move || crate::forward::read_me(&target))
            .await
            .unwrap_or_else(|_| {
                Err(crate::forward::Failure::Retryable(
                    "the usage read stopped".to_owned(),
                ))
            });
        let me = me.map_err(|f| PollError::Failed {
            note: match f {
                crate::forward::Failure::Disabled(why) => format!("unknown, {why}"),
                crate::forward::Failure::Limited { message, .. }
                | crate::forward::Failure::Retryable(message)
                | crate::forward::Failure::Failed(message) => format!("unknown, {message}"),
                crate::forward::Failure::TimedOut => {
                    "unknown, the bridge did not answer".to_owned()
                }
            },
            retry: claude::DEFAULT_RETRY,
        })?;
        let reading = crate::forward::me_windows(&me, remote_agent, &remote);
        let store_err = |e: crate::store::StoreError| PollError::Failed {
            note: e.to_string(),
            retry: MIN_GAP,
        };
        self.store
            .replace_usage(a.agent, &a.name, &reading.windows)
            .map_err(store_err)?;
        let plan = if reading.shared {
            format!("via {remote}, shared")
        } else {
            format!("via {remote}")
        };
        self.store
            .set_poll_status(a.agent, &a.name, true, Some(&plan), reading.note.as_deref())
            .map_err(store_err)?;
        if let Some(until) = reading.limited_until.filter(|u| *u > now_ms()) {
            self.store
                .set_account_limited(a.agent, &a.name, until)
                .map_err(store_err)?;
        }
        Ok(())
    }

    /// Records a read of `key` now; the next is after `retry`, or after the
    /// interval plus jitter.
    fn mark(&self, key: &Key, retry: Option<Duration>) {
        let now = Instant::now();
        let wait = retry.unwrap_or_else(|| self.settings.interval + jitter(MIN_GAP));
        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                key.clone(),
                Slot {
                    last: Some(now),
                    due: now + wait.max(MIN_GAP),
                },
            );
    }

    async fn read_claude(&self, name: &str, kind: AccountKind) -> Result<(), PollError> {
        let (config_dir, file_dir) = if kind == AccountKind::Default {
            match std::env::var("CLAUDE_CONFIG_DIR")
                .ok()
                .filter(|v| !v.is_empty())
            {
                Some(dir) => (Some(dir.clone()), Some(PathBuf::from(dir))),
                None => (None, self.settings.claude_home.clone()),
            }
        } else {
            // Exactly the string runs pass in CLAUDE_CONFIG_DIR.
            let dir = acct::account_dir(&self.home, AgentKind::Claude, name);
            (Some(dir.to_string_lossy().into_owned()), Some(dir))
        };
        let src = claude::TokenSource {
            config_dir,
            security: self.settings.security_bin.clone(),
            user: self.settings.keychain_user.clone(),
            file_dir,
        };
        let creds = claude::read_token(&src, now_ms())
            .await
            .map_err(|e| PollError::Failed {
                note: e.note().to_owned(),
                retry: self.settings.interval,
            })?;
        let base = self.settings.claude_base_url.clone();
        let token = creds.token.clone();
        let reply = tokio::task::spawn_blocking(move || claude::fetch(&base, &token))
            .await
            .unwrap_or_else(|_| claude::Reply::Failed("the usage read stopped".to_owned()));
        match reply {
            claude::Reply::Usage(windows) => self
                .store
                .replace_usage(AgentKind::Claude, name, &windows)
                .and_then(|()| {
                    self.store.set_poll_status(
                        AgentKind::Claude,
                        name,
                        true,
                        creds.plan.as_deref(),
                        None,
                    )
                })
                .map_err(|e| PollError::Failed {
                    note: e.to_string(),
                    retry: MIN_GAP,
                }),
            claude::Reply::Busy(after) => Err(PollError::Failed {
                note: format!(
                    "unknown, the usage endpoint asked to wait {}s",
                    after.as_secs()
                ),
                retry: after,
            }),
            claude::Reply::Rejected => Err(PollError::Failed {
                note: format!(
                    "{}; the usage endpoint rejected the sign-in",
                    claude::REFRESHES_ON_RUN
                ),
                retry: claude::AUTH_RETRY,
            }),
            claude::Reply::Failed(why) => Err(PollError::Failed {
                note: format!("unknown, {why}"),
                retry: claude::DEFAULT_RETRY,
            }),
        }
    }

    async fn read_codex(&self, name: &str, kind: AccountKind) -> Result<(), PollError> {
        let bin = self.codex_bin.clone().ok_or_else(|| PollError::Failed {
            note: "unknown, Codex is not installed".to_owned(),
            retry: self.settings.interval,
        })?;
        let env = acct::run_env(&self.home, AgentKind::Codex, name, kind).map_err(|e| {
            PollError::Failed {
                note: format!("unknown, the account could not be loaded: {e}"),
                retry: self.settings.interval,
            }
        })?;
        let expected = match kind {
            AccountKind::Default => self.settings.codex_home.clone(),
            _ => Some(acct::account_dir(&self.home, AgentKind::Codex, name)),
        };
        let quota = codex::read(&bin, env.as_ref(), expected.as_deref(), &self.home)
            .await
            .map_err(|why| {
                if why == codex::SIGN_IN_REJECTED {
                    PollError::Failed {
                        note: format!(
                            "unknown, {why}; sign in again with `hashi account login codex {name}`"
                        ),
                        retry: claude::AUTH_RETRY,
                    }
                } else {
                    PollError::Failed {
                        note: format!("unknown, {why}"),
                        retry: claude::DEFAULT_RETRY,
                    }
                }
            })?;
        let store_err = |e: crate::store::StoreError| PollError::Failed {
            note: e.to_string(),
            retry: MIN_GAP,
        };
        self.store
            .replace_usage(AgentKind::Codex, name, &quota.windows)
            .map_err(store_err)?;
        self.store
            .set_poll_status(AgentKind::Codex, name, true, quota.plan.as_deref(), None)
            .map_err(store_err)?;
        if let Some(Some(until)) = quota.reached
            && until > now_ms()
        {
            self.store
                .set_account_limited(AgentKind::Codex, name, until)
                .map_err(store_err)?;
        }
        Ok(())
    }

    /// Reads every eligible account that is due, until `cancel` fires.
    /// `after_run` names accounts whose run just finished; each is read
    /// again a minute after its last read.
    pub async fn run(
        self: Arc<Self>,
        mut after_run: tokio::sync::mpsc::UnboundedReceiver<Key>,
        cancel: CancellationToken,
    ) {
        loop {
            match crate::share::end_expired_shares(&self.store, now_ms()) {
                Ok(ended) => ended.into_iter().for_each(|m| self.bus.log(m)),
                Err(e) => tracing::warn!(error = %e, "could not check shared logins"),
            }
            let now = Instant::now();
            let due: Vec<Key> = {
                let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
                let mut due = Vec::new();
                for key in self.eligible() {
                    let slot = slots.entry(key.clone()).or_insert(Slot {
                        last: None,
                        due: now + jitter(TICK * 2),
                    });
                    let rested = slot.last.is_none_or(|l| l.elapsed() >= MIN_GAP);
                    if slot.due <= now && rested {
                        due.push(key);
                    }
                }
                due
            };
            for (agent, name) in due {
                if cancel.is_cancelled() {
                    return;
                }
                match self.poll(agent, &name).await {
                    Err(PollError::Busy) => self.retry_in(&(agent, name), MIN_GAP),
                    Err(PollError::NotSupported(_)) => {
                        self.retry_in(&(agent, name), self.settings.interval);
                    }
                    _ => {}
                }
            }
            tokio::select! {
                () = cancel.cancelled() => return,
                key = after_run.recv() => {
                    if let Some(key) = key {
                        self.soon(&key);
                    }
                }
                () = tokio::time::sleep(TICK) => {}
            }
        }
    }

    fn retry_in(&self, key: &Key, wait: Duration) {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(slot) = slots.get_mut(key) {
            slot.due = Instant::now() + wait;
        }
    }

    /// Schedules a read of `key` for a minute after its last one.
    fn soon(&self, key: &Key) {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        let slot = slots.entry(key.clone()).or_insert(Slot {
            last: None,
            due: now,
        });
        slot.due = slot.last.map_or(now, |l| (l + MIN_GAP).max(now));
    }

    /// When `key` is next due, for tests and diagnostics.
    #[must_use]
    pub fn next_due(&self, key: &Key) -> Option<Instant> {
        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .map(|s| s.due)
    }
}

/// Whether Hashi can read `agent`'s live usage.
#[must_use]
pub const fn supports(agent: AgentKind) -> bool {
    matches!(agent, AgentKind::Claude | AgentKind::Codex)
}
