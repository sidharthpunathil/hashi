//! Durable work queue with an independent worker pool per agent.
//!
//! Requests live in SQLite, so a crash never loses work: on startup anything
//! left `running` is re-queued. Workers drain their agent's queue, then park
//! on a [`Notify`] until the API enqueues more (with a periodic fallback poll).

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
use crate::store::{Request, Store};
use crate::util::{now_ms, truncate};

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

/// Spawns `concurrency` workers per agent. They stop when `cancel` fires.
pub fn spawn_workers(
    store: Arc<Store>,
    bus: Bus,
    dispatcher: &Dispatcher,
    cfg: Arc<QueueConfig>,
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
    notify: Arc<Notify>,
}

impl Worker {
    async fn run(self, cancel: CancellationToken) {
        loop {
            match self.store.claim_next(self.agent) {
                Ok(Some(req)) => {
                    tokio::select! {
                        () = cancel.cancelled() => {
                            // Leave it `running`; recovery re-queues it on next start.
                            return;
                        }
                        () = self.process(req) => {}
                    }
                    continue;
                }
                Ok(None) => {}
                Err(e) => warn!(agent = %self.agent, error = %e, "failed to claim work"),
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
                let msg = truncate(&err, 4_000);
                let error_id = crate::diagnostics::new_error_id();
                warn!(
                    id = %req.id,
                    agent = %self.agent,
                    error_id = %error_id,
                    error = %msg,
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

        // Each token gets a private working directory. Agents key their
        // session history by cwd, so this also keeps resumes consistent.
        let workdir = self.cfg.workspace.join(&req.token_id);
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

        let out = agent::execute(self.agent, cfg, inv, &workdir, self.cfg.limits)
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
