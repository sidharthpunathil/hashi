//! Durable work queue with an independent worker pool per agent.
//!
//! Requests live in SQLite, so a crash never loses work: on startup anything
//! left `running` is re-queued. Workers drain their agent's queue, then park
//! on a [`Notify`] until the API enqueues more (with a periodic fallback poll).
//!
//! Before claiming anything, a worker takes a slot from the shared
//! [`ResourceGuard`] and checks memory and disk. Held work stays `queued`, so
//! it keeps its place in line and runs once the machine has room.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::agent::{self, AgentConfig, AgentKind, Invocation, Limits};
use crate::events::{Bus, Event};
use crate::guard::ResourceGuard;
use crate::store::{Request, Store};
use crate::util::{now_ms, truncate};

/// Characters of an agent's error written to the log. The database row keeps
/// the full text, so a noisy failure cannot grow the log quickly.
pub const LOG_ERROR_CHARS: usize = 500;

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

/// Wakes workers when new work arrives.
#[derive(Debug, Clone, Default)]
pub struct Dispatcher {
    notify: Arc<HashMap<AgentKind, Arc<Notify>>>,
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
        }
    }

    /// Signals that `agent` has queued work.
    pub fn wake(&self, agent: AgentKind) {
        if let Some(n) = self.notify.get(&agent) {
            n.notify_one();
        }
    }

    fn handle(&self, agent: AgentKind) -> Arc<Notify> {
        self.notify.get(&agent).cloned().unwrap_or_default()
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
}

impl Worker {
    async fn run(self, cancel: CancellationToken) {
        loop {
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
                            let id = req.id.clone();
                            tokio::select! {
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
                                () = self.process(req) => {}
                            }
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
            Err(err) => {
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

    async fn execute(&self, req: &Request) -> Result<agent::AgentOutput, String> {
        let cfg = self
            .cfg
            .agents
            .get(&self.agent)
            .ok_or("agent is not configured")?;

        // Each token gets a private working directory per agent. Agents key
        // their session history by cwd, so this keeps resumes consistent. It
        // also stops one agent with write access from planting project config
        // (for example `.claude/settings.json` or `.gemini/settings.json`,
        // which can define hooks) that a different agent would then load.
        let workdir = self
            .cfg
            .workspace
            .join(&req.token_id)
            .join(self.agent.as_str());
        tokio::fs::create_dir_all(&workdir)
            .await
            .map_err(|e| format!("cannot create workspace: {e}"))?;

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
}
