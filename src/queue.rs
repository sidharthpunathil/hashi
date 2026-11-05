//! Durable work queue with an independent worker pool per agent.
//!
//! Requests live in SQLite, so a crash never loses work: on startup anything
//! left `running` is re-queued. Workers drain their agent's queue, then park
//! on a [`Notify`] until the API enqueues more (with a periodic fallback poll).
//!
//! Before claiming anything, a worker takes a slot from the shared
//! [`ResourceGuard`] and checks memory and disk. Held work stays `queued`, so
//! it keeps its place in line and runs once the machine has room.
//!
//! A paused token's prompts stay `queued` until it is resumed; an expired
//! token's queued prompts fail without running. Every claimed run is
//! registered with the [`Dispatcher`], so revoking a token can stop its runs
//! (killing their process groups) before its workspace is deleted. A revoke
//! from another process, such as `hashi token revoke`, deletes the request
//! row; workers notice that within [`ROW_CHECK_EVERY`] and stop the run too.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::agent::{self, AgentConfig, AgentKind, Invocation, Limits};
use crate::events::{Bus, Event};
use crate::guard::ResourceGuard;
use crate::store::{EXPIRED_BEFORE_START, Request, RequestStatus, Store, StoreError};
use crate::token::TokenStatus;
use crate::util::{now_ms, truncate};

/// Characters of an agent's error written to the log. The database row keeps
/// the full text, so a noisy failure cannot grow the log quickly.
pub const LOG_ERROR_CHARS: usize = 500;

/// How often a running worker checks that its request row still exists.
pub const ROW_CHECK_EVERY: Duration = Duration::from_secs(2);

/// Longest [`Dispatcher::cancel_token_runs`] waits for runs to stop.
const CANCEL_WAIT: Duration = Duration::from_secs(10);

/// Error reported for a run stopped because its token was revoked.
pub const REVOKED_ERROR: &str = "stopped: the token was revoked";

/// Error reported for a request cancelled with `DELETE /v1/ask/{id}`, the
/// MCP `cancel` tool or the dashboard.
pub const CANCELLED_ERROR: &str = "cancelled";

/// Settings shared by all workers.
#[derive(Debug, Clone)]
pub struct QueueConfig {
    /// Per-agent backend configuration.
    pub agents: HashMap<AgentKind, AgentConfig>,
    /// Workers per agent.
    pub concurrency: usize,
    /// Per-run limits.
    pub limits: Limits,
    /// Root directory under which each token gets its own workspace.
    pub workspace: PathBuf,
}

/// One claimed run, as seen by [`Dispatcher::cancel_token_runs`].
#[derive(Debug)]
struct RunEntry {
    token_id: String,
    /// Fired to stop the run.
    cancel: CancellationToken,
    /// Fired by the worker once the run (and its process group) is gone.
    done: CancellationToken,
}

type Runs = Arc<Mutex<HashMap<String, RunEntry>>>;

/// Wakes workers when new work arrives, and tracks the runs in flight so
/// they can be stopped per token.
#[derive(Debug, Clone, Default)]
pub struct Dispatcher {
    notify: Arc<HashMap<AgentKind, Arc<Notify>>>,
    runs: Runs,
}

/// Registration of one run. Dropping it marks the run as finished.
#[derive(Debug)]
struct RunGuard {
    runs: Runs,
    id: String,
    cancel: CancellationToken,
    done: CancellationToken,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        self.runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id);
        self.done.cancel();
    }
}

impl Dispatcher {
    /// Creates a dispatcher with one wake-up channel per agent.
    #[must_use]
    pub fn new() -> Self {
        let map = AgentKind::ALL
            .iter()
            .map(|k| (*k, Arc::new(Notify::new())))
            .collect();
        Self {
            notify: Arc::new(map),
            runs: Runs::default(),
        }
    }

    /// Signals that `agent` has queued work.
    pub fn wake(&self, agent: AgentKind) {
        if let Some(n) = self.notify.get(&agent) {
            n.notify_one();
        }
    }

    /// Signals every agent, for example after a token is resumed.
    pub fn wake_all(&self) {
        for n in self.notify.values() {
            n.notify_waiters();
            n.notify_one();
        }
    }

    fn handle(&self, agent: AgentKind) -> Arc<Notify> {
        self.notify.get(&agent).cloned().unwrap_or_default()
    }

    fn register(&self, id: &str, token_id: &str) -> RunGuard {
        let cancel = CancellationToken::new();
        let done = CancellationToken::new();
        self.runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                id.to_owned(),
                RunEntry {
                    token_id: token_id.to_owned(),
                    cancel: cancel.clone(),
                    done: done.clone(),
                },
            );
        RunGuard {
            runs: Arc::clone(&self.runs),
            id: id.to_owned(),
            cancel,
            done,
        }
    }

    /// Runs in flight for `token_id`.
    #[must_use]
    pub fn running_for(&self, token_id: &str) -> usize {
        self.runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .filter(|r| r.token_id == token_id)
            .count()
    }

    /// Stops the run of request `id`, if one is registered, the same way
    /// [`Dispatcher::cancel_token_runs`] does. The returned future resolves
    /// once the run (and its process group) is gone, or after ten seconds.
    pub fn cancel_run(&self, id: &str) -> Option<impl Future<Output = ()> + Send + 'static> {
        let done = {
            let runs = self.runs.lock().unwrap_or_else(PoisonError::into_inner);
            let run = runs.get(id)?;
            run.cancel.cancel();
            run.done.clone()
        };
        Some(async move {
            if tokio::time::timeout(CANCEL_WAIT, done.cancelled_owned())
                .await
                .is_err()
            {
                warn!("a cancelled run did not stop in time");
            }
        })
    }

    /// Stops every run of `token_id` now: each run is dropped, which kills
    /// its process group, and is not re-queued. The returned future resolves
    /// with the number of runs stopped once all of them are gone (or after
    /// ten seconds), so a caller can then delete the workspace safely.
    ///
    /// Call it after the token row is deleted: a worker that claims one of
    /// its requests afterwards checks the token before it creates the
    /// workspace or spawns anything, and so cannot slip past.
    pub fn cancel_token_runs(
        &self,
        token_id: &str,
    ) -> impl Future<Output = usize> + Send + 'static {
        let done: Vec<CancellationToken> = self
            .runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .filter(|r| r.token_id == token_id)
            .map(|r| {
                r.cancel.cancel();
                r.done.clone()
            })
            .collect();
        async move {
            let n = done.len();
            let all = futures::future::join_all(done.iter().map(CancellationToken::cancelled));
            if tokio::time::timeout(CANCEL_WAIT, all).await.is_err() {
                warn!("a revoked token's run did not stop in time");
            }
            n
        }
    }
}

/// Spawns `concurrency` workers per agent, all sharing `guard`. They stop
/// when `cancel` fires.
pub fn spawn_workers(
    store: Arc<Store>,
    bus: Bus,
    dispatcher: &Dispatcher,
    cfg: Arc<QueueConfig>,
    guard: Arc<ResourceGuard>,
    cancel: CancellationToken,
) -> JoinSet<()> {
    let mut set = JoinSet::new();
    for agent in AgentKind::ALL {
        for _ in 0..cfg.concurrency.max(1) {
            let worker = Worker {
                agent,
                store: Arc::clone(&store),
                bus: bus.clone(),
                cfg: Arc::clone(&cfg),
                guard: Arc::clone(&guard),
                notify: dispatcher.handle(agent),
                dispatcher: dispatcher.clone(),
            };
            set.spawn(worker.run(cancel.clone()));
        }
    }
    set
}

struct Worker {
    agent: AgentKind,
    store: Arc<Store>,
    bus: Bus,
    cfg: Arc<QueueConfig>,
    guard: Arc<ResourceGuard>,
    notify: Arc<Notify>,
    dispatcher: Dispatcher,
}

/// Why a claimed run did not produce an answer or an agent error.
enum Halt {
    /// Failed with this message.
    Failed(String),
    /// Put back in the queue (its token is paused).
    Requeued,
    /// Its token is gone.
    Revoked,
}

impl From<String> for Halt {
    fn from(msg: String) -> Self {
        Self::Failed(msg)
    }
}

impl From<&str> for Halt {
    fn from(msg: &str) -> Self {
        Self::Failed(msg.to_owned())
    }
}

impl Worker {
    async fn run(self, cancel: CancellationToken) {
        loop {
            self.fail_expired();
            let has_work = self.store.has_queued(self.agent).unwrap_or_else(|e| {
                warn!(agent = %self.agent, error = %e, "failed to check the queue");
                false
            });
            if has_work {
                // A slot first, so held requests are never claimed.
                let permit = tokio::select! {
                    () = cancel.cancelled() => return,
                    p = self.guard.acquire() => p,
                };
                if let Some(mut permit) = permit {
                    if let Err(hold) = self.guard.check() {
                        drop(permit);
                        self.guard.hold(&hold);
                        tokio::select! {
                            () = cancel.cancelled() => return,
                            () = tokio::time::sleep(self.guard.config().retry_after) => {}
                        }
                        continue;
                    }
                    self.guard.clear_hold();
                    match self.store.claim_next(self.agent) {
                        Ok(Some(req)) => {
                            self.guard.start(&mut permit);
                            // Registered before the first await, so a revoke
                            // that has already deleted the token either sees
                            // this run or is seen by the token check in
                            // `execute`.
                            let run = self.dispatcher.register(&req.id, &req.token_id);
                            let id = req.id.clone();
                            let started = now_ms();
                            tokio::select! {
                                biased;
                                () = cancel.cancelled() => {
                                    // Dropping the run kills its process tree. A clean
                                    // stop is not the prompt's fault, so put it back
                                    // without counting an interruption. If this fails,
                                    // recovery re-queues it on the next start.
                                    if let Err(e) = self.store.requeue(&id) {
                                        warn!(id = %id, error = %e, "failed to re-queue on shutdown");
                                    }
                                    return;
                                }
                                () = run.cancel.cancelled() => {
                                    self.stopped(&id, started, REVOKED_ERROR);
                                }
                                () = self.watch_row(&id) => {
                                    self.stopped(&id, started, REVOKED_ERROR);
                                }
                                () = self.process(req) => {}
                            }
                            // The run future is gone now; tell any revoke
                            // waiting on it.
                            drop(run);
                            continue;
                        }
                        // Another worker for this agent got there first.
                        Ok(None) => {}
                        Err(e) => warn!(agent = %self.agent, error = %e, "failed to claim work"),
                    }
                }
            }
            tokio::select! {
                () = cancel.cancelled() => return,
                () = self.notify.notified() => {}
                () = tokio::time::sleep(Duration::from_secs(5)) => {}
            }
        }
    }

    /// Fails this agent's queued prompts whose token has expired.
    fn fail_expired(&self) {
        match self.store.fail_expired_queued(self.agent) {
            Ok(ids) => {
                for id in ids {
                    info!(id = %id, agent = %self.agent, "token expired before the request started");
                    self.bus.emit(Event::Finished {
                        id,
                        agent: self.agent,
                        ok: false,
                        duration_ms: 0,
                        error: Some(EXPIRED_BEFORE_START.to_owned()),
                    });
                }
            }
            Err(e) => warn!(agent = %self.agent, error = %e, "failed to expire queued work"),
        }
    }

    /// Resolves once request `id` no longer exists or is no longer running,
    /// for example after a revoke from another process.
    async fn watch_row(&self, id: &str) {
        loop {
            tokio::time::sleep(ROW_CHECK_EVERY).await;
            match self.store.is_running(id) {
                Ok(false) => return,
                Ok(true) => {}
                Err(e) => warn!(id = %id, error = %e, "failed to check a running request"),
            }
        }
    }

    /// Reports a run that was stopped from outside.
    fn stopped(&self, id: &str, started: i64, why: &str) {
        // A cancel fails the row before it stops the run, so its reason is
        // stored; a revoke deletes the row.
        let why = match self.store.get_request(id) {
            Ok(Some(r)) if r.status == RequestStatus::Failed => {
                r.error.unwrap_or_else(|| why.to_owned())
            }
            _ => why.to_owned(),
        };
        info!(id = %id, agent = %self.agent, "run stopped: {why}");
        self.bus.emit(Event::Finished {
            id: id.to_owned(),
            agent: self.agent,
            ok: false,
            duration_ms: u64::try_from(now_ms() - started).unwrap_or(0),
            error: Some(why),
        });
    }

    async fn process(&self, req: Request) {
        self.bus.emit(Event::Started {
            id: req.id.clone(),
            agent: self.agent,
        });
        let started = now_ms();
        let result = self.execute(&req).await;
        let duration_ms = u64::try_from(now_ms() - started).unwrap_or(0);

        let (ok, error) = match result {
            Ok(out) => {
                if let Err(e) = self.store.complete_request(&req.id, &out.text, out.usage) {
                    warn!(id = %req.id, error = %e, "failed to store response");
                }
                (true, None)
            }
            Err(Halt::Requeued) => {
                info!(id = %req.id, agent = %self.agent, "token paused, request left in the queue");
                return;
            }
            Err(Halt::Revoked) => (false, Some(REVOKED_ERROR.to_owned())),
            Err(Halt::Failed(err)) => {
                let msg = client_error(&err);
                let error_id = crate::diagnostics::new_error_id();
                warn!(
                    id = %req.id,
                    agent = %self.agent,
                    error_id = %error_id,
                    error = %log_excerpt(&msg),
                    "request failed"
                );
                if let Err(e) = self.store.fail_request(&req.id, &msg) {
                    warn!(id = %req.id, error = %e, "failed to store error");
                }
                (false, Some(msg))
            }
        };
        info!(id = %req.id, agent = %self.agent, ok, duration_ms, "request finished");
        self.bus.emit(Event::Finished {
            id: req.id,
            agent: self.agent,
            ok,
            duration_ms,
            error,
        });
    }

    /// Re-reads the request's token right before the run. A paused token
    /// puts the request back, an expired one fails it, and a missing one
    /// (revoked) drops it.
    fn token_gate(&self, req: &Request) -> Result<(), Halt> {
        let token = self
            .store
            .get_token(&req.token_id)
            .map_err(|e| e.to_string())?;
        match token.map(|t| t.status(now_ms())) {
            None => Err(Halt::Revoked),
            Some(TokenStatus::Active) => Ok(()),
            Some(TokenStatus::Paused) => {
                self.store.requeue(&req.id).map_err(|e| e.to_string())?;
                Err(Halt::Requeued)
            }
            Some(TokenStatus::Expired) => Err(EXPIRED_BEFORE_START.into()),
        }
    }

    async fn execute(&self, req: &Request) -> Result<agent::AgentOutput, Halt> {
        let cfg = self
            .cfg
            .agents
            .get(&self.agent)
            .ok_or("agent is not configured")?;

        self.token_gate(req)?;

        // Each token gets a private working directory per agent. Agents key
        // their session history by cwd, so this keeps resumes consistent. It
        // also stops one agent with write access from planting project config
        // (for example `.claude/settings.json` or `.gemini/settings.json`,
        // which can define hooks) that a different agent would then load.
        //
        // Created synchronously, so a revoke that cancels this run cannot
        // interrupt it half way and then race it with the workspace removal.
        let workdir = self
            .cfg
            .workspace
            .join(&req.token_id)
            .join(self.agent.as_str());
        std::fs::create_dir_all(&workdir).map_err(|e| format!("cannot create workspace: {e}"))?;
        // A revoke from another process may have deleted the token, and its
        // workspace, between the check above and the line above. If so, the
        // directory just created is removed again and nothing runs.
        if self
            .store
            .get_token(&req.token_id)
            .map_err(|e| e.to_string())?
            .is_none()
        {
            if let Err(e) =
                crate::server::remove_token_workspace(&self.cfg.workspace, &req.token_id)
            {
                warn!(error = %e, "could not delete a revoked token's workspace");
            }
            return Err(Halt::Revoked);
        }

        let resume = if req.stateful {
            self.store
                .session(&req.token_id, self.agent)
                .map_err(|e| e.to_string())?
        } else {
            None
        };
        let resume = resume.filter(|s| agent::is_safe_arg(s));
        let inv = Invocation {
            prompt: &req.message,
            resume: resume.as_deref(),
            model: req.model.as_deref(),
        };

        let mut limits = self.cfg.limits.clone();
        limits.nice = self.guard.config().nice;
        limits.memory = Some(self.guard.watch());
        let out = agent::execute(self.agent, cfg, inv, &workdir, limits)
            .await
            .map_err(|e| e.to_string())?;

        if req.stateful
            && let Some(session) = out.session_id.as_deref().filter(|s| agent::is_safe_arg(s))
            && let Err(e) = self.store.set_session(&req.token_id, self.agent, session)
        {
            warn!(error = %e, "failed to persist session");
        }
        Ok(out)
    }
}

/// Who asked for a cancel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelBy<'a> {
    /// The token that sent the request.
    Owner,
    /// Someone else, such as an admin token or `dashboard`. Audited under
    /// this name.
    Other(&'a str),
}

/// What [`cancel`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum CancelOutcome {
    /// The request was queued or running and is now failed with
    /// [`CANCELLED_ERROR`].
    Cancelled(Request),
    /// The request had already finished; it is unchanged.
    Finished(Request),
    /// No such request.
    NotFound,
}

/// Cancels request `id`. A queued request fails at once. A running one is
/// failed, then stopped through the run registry (the path revoke uses),
/// which kills its whole process tree; this waits until it is gone. Callers
/// check that the asker may cancel it. A cancel by anyone but the owner is
/// audited.
///
/// # Errors
///
/// Fails on database errors.
pub async fn cancel(
    state: &crate::api::AppState,
    id: &str,
    by: CancelBy<'_>,
) -> Result<CancelOutcome, StoreError> {
    let store = &state.store;
    let Some(req) = store.get_request(id)? else {
        return Ok(CancelOutcome::NotFound);
    };
    if req.status.is_terminal() {
        return Ok(CancelOutcome::Finished(req));
    }
    if store.cancel_queued(id, CANCELLED_ERROR)? {
        state.bus.emit(Event::Finished {
            id: id.to_owned(),
            agent: req.agent,
            ok: false,
            duration_ms: 0,
            error: Some(CANCELLED_ERROR.to_owned()),
        });
    } else if store.cancel_running(id, CANCELLED_ERROR)? {
        // A worker registers its run right after claiming it; give one that
        // is between the two a moment. Without a registration the worker
        // still sees the failed row within `ROW_CHECK_EVERY`.
        for _ in 0..20 {
            if let Some(stopped) = state.dispatcher.cancel_run(id) {
                stopped.await;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    let Some(req) = store.get_request(id)? else {
        return Ok(CancelOutcome::NotFound);
    };
    // It finished on its own before the cancel took hold.
    if req.error.as_deref() != Some(CANCELLED_ERROR) || req.status != RequestStatus::Failed {
        return Ok(CancelOutcome::Finished(req));
    }
    if let CancelBy::Other(actor) = by {
        store.audit(actor, "request.cancel", &req.token_name, id)?;
    }
    state.bus.log(format!("request cancelled: {id}"));
    Ok(CancelOutcome::Cancelled(req))
}

/// A failed run's error as stored and returned to the token holder: secrets
/// and the owner's home directory (for example in the binary path of a
/// "failed to start" error) are redacted, then it is capped at 4000
/// characters. Redacting first means a cut cannot leave half a secret.
fn client_error(err: &str) -> String {
    truncate(&crate::diagnostics::redact(err), 4_000)
}

/// The part of an agent's error that goes into the log.
fn log_excerpt(err: &str) -> String {
    truncate(err, LOG_ERROR_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_errors_hide_the_owner_home_and_secrets() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let home = home.display().to_string();
        let err = format!(
            "failed to start `{home}/.local/bin/claude`: No such file or directory, key sk-ant-api03-AbCdEfGhIjKlMnOpQrSt"
        );
        let msg = client_error(&err);
        assert!(!msg.contains(&home), "{msg}");
        assert!(
            msg.starts_with("failed to start `~/.local/bin/claude`"),
            "{msg}"
        );
        assert!(!msg.contains("AbCdEfGhIjKlMnOpQrSt"), "{msg}");
        assert_eq!(client_error(&"x".repeat(5_000)).chars().count(), 4_000);
    }

    #[test]
    fn logged_errors_are_short_but_short_errors_are_whole() {
        let long = "x".repeat(4_000);
        assert_eq!(log_excerpt(&long).chars().count(), LOG_ERROR_CHARS);
        assert_eq!(log_excerpt("please sign in again"), "please sign in again");
    }

    #[tokio::test]
    async fn cancel_token_runs_stops_only_that_token_and_waits() {
        let d = Dispatcher::new();
        let a = d.register("r1", "tok-a");
        let b = d.register("r2", "tok-b");
        assert_eq!(d.running_for("tok-a"), 1);
        let wait = d.cancel_token_runs("tok-a");
        assert!(a.cancel.is_cancelled());
        assert!(!b.cancel.is_cancelled());
        let waiter = tokio::spawn(wait);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished(), "returned before the run was gone");
        drop(a);
        assert_eq!(waiter.await.unwrap(), 1);
        assert_eq!(d.running_for("tok-a"), 0);
        assert_eq!(d.running_for("tok-b"), 1);
        assert_eq!(d.cancel_token_runs("nobody").await, 0);
    }
}
