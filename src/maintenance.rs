//! Hourly housekeeping while the server runs: prunes finished requests past
//! retention (never the current month), old audit records and spent invites,
//! then returns free pages to the file system.
//!
//! Startup already prunes once, so the first pass runs an hour after start.

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::store::{Pruned, Store, StoreError};

/// Time between passes.
pub const INTERVAL: Duration = Duration::from_secs(3_600);

/// Runs one pass: prune, then compact. Blocking; call it off the async
/// workers.
///
/// # Errors
///
/// Fails on database errors. Pruning is kept even if compacting fails.
pub fn run_once(store: &Store, retention_days: u32) -> Result<Pruned, StoreError> {
    let pruned = store.prune_all(retention_days)?;
    if let Err(e) = store.compact() {
        warn!(error = %e, "could not compact the database");
    }
    Ok(pruned)
}

/// Spawns the hourly task on `tasks`. It stops when `cancel` fires.
pub fn spawn(
    tasks: &mut JoinSet<()>,
    store: Arc<Store>,
    retention_days: u32,
    cancel: CancellationToken,
) {
    tasks.spawn(async move {
        let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + INTERVAL, INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                _ = tick.tick() => {}
            }
            let store = Arc::clone(&store);
            match tokio::task::spawn_blocking(move || run_once(&store, retention_days)).await {
                Ok(Ok(p)) if p != Pruned::default() => info!(
                    requests = p.requests,
                    audit = p.audit,
                    invites = p.invites,
                    "maintenance pruned old records"
                ),
                Ok(Ok(_)) => {}
                Ok(Err(e)) => warn!(error = %e, "maintenance pass failed"),
                Err(e) => warn!(error = %e, "maintenance task stopped"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentKind;
    use crate::store::{NewRequest, NewToken};
    use crate::token::{Limits, Role, SessionMode};

    #[test]
    fn run_once_prunes_old_rows_and_keeps_recent_ones() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("hashi.db")).unwrap();
        let (t, _) = store
            .create_token(&NewToken {
                name: "u".into(),
                role: Role::User,
                session_mode: SessionMode::Stateless,
                agents: AgentKind::ALL.to_vec(),
                expires_in_days: None,
                note: None,
                limits: Limits::default(),
                accounts: Default::default(),
                project: None,
            })
            .unwrap();
        for id in ["old", "new"] {
            store
                .insert_request(NewRequest {
                    id,
                    token: &t,
                    agent: AgentKind::Claude,
                    model: None,
                    message: &"p".repeat(20_000),
                })
                .unwrap();
            store.claim_next(AgentKind::Claude).unwrap();
            store
                .complete_request(id, &"r".repeat(20_000), None)
                .unwrap();
        }
        store.audit("cli", "token.create", "u", "").unwrap();
        store.backdate_for_tests("old", 400);

        let pruned = run_once(&store, 14).unwrap();
        assert_eq!((pruned.requests, pruned.audit), (1, 1));
        assert!(store.get_request("old").unwrap().is_none());
        assert!(store.get_request("new").unwrap().is_some());
        // A fresh database is created with incremental auto-vacuum.
        assert!(store.compact().unwrap());
    }
}
