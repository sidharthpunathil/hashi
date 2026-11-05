//! Hourly housekeeping while the server runs: prunes finished requests past
//! retention (never the current month), old audit records and spent invites,
//! and access requests decided or expired 30 days ago, and the rows of
//! accounts that no longer exist, then returns free pages to the file
//! system.
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
    // Usage, reads and conversations of accounts removed while a run or a
    // poll on them was still going.
    match store.prune_orphan_account_rows() {
        Ok(0) => {}
        Ok(n) => info!(rows = n, "maintenance removed rows of removed accounts"),
        Err(e) => warn!(error = %e, "could not remove rows of removed accounts"),
    }
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
                    access_requests = p.access,
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

    #[test]
    fn run_once_purges_access_requests_thirty_days_after_they_end() {
        use crate::access::{self, NewPerson, OpenLink};
        let store = Store::open_in_memory().unwrap();
        let ask = |name: &str| {
            let (_, code) = access::open_link(&store, "cli", OpenLink::default()).unwrap();
            access::submit_new(
                &store,
                NewPerson {
                    code,
                    name: name.into(),
                    message: None,
                    agents: None,
                },
            )
            .unwrap()
            .0
        };
        let declined = ask("declined");
        access::decline(&store, "cli", &declined.id, Some("no")).unwrap();
        ask("waiting");
        // 29 days on, nothing goes: the declined one is recent enough and the
        // waiting one expired only 22 days ago.
        store.backdate_access_for_tests(29);
        assert_eq!(run_once(&store, 14).unwrap().access, 0);
        // Two more days and the declined one is past 30 days.
        store.backdate_access_for_tests(2);
        assert_eq!(run_once(&store, 14).unwrap().access, 1);
        // The expired one goes 30 days after its 7-day expiry.
        store.backdate_access_for_tests(7);
        assert_eq!(run_once(&store, 14).unwrap().access, 1);
        assert!(store.list_access_requests().unwrap().is_empty());
        // Old closed links with no requests left are dropped too.
        assert!(store.latest_access_link().unwrap().is_none());
    }
}
