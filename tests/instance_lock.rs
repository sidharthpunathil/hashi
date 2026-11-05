//! One server per home: a second `hashi serve` on a home in use refuses to
//! start before it touches the first one's requests or pid file.

// Test helpers outside #[test] functions are not covered by clippy.toml's
// test exemptions; panicking on setup failure is the desired behaviour here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use hashi::agent::{AgentKind, Limits};
use hashi::events::Bus;
use hashi::guard::{GIB, ManualProbe, ResourceSettings};
use hashi::net::NetLimits;
use hashi::server::{self, ServerConfig};
use hashi::store::{NewRequest, RequestStatus};

fn config(home: &Path) -> ServerConfig {
    ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        home: home.to_path_buf(),
        agents: HashMap::new(),
        default_agent: AgentKind::Claude,
        concurrency: 1,
        // No memory free, so the server's own workers never pick work up.
        resources: ResourceSettings::with_probe(std::sync::Arc::new(ManualProbe::new(16 * GIB, 0))),
        limits: Limits::default(),
        rate_limit: 0,
        queue_limit: 10,
        retention_days: 14,
        tunnel: false,
        public_url: None,
        name: "Lock test".into(),
        tls: None,
        tunnel_token: None,
        update_check: false,
        tailscale: None,
        helpers: Default::default(),
        account_cooldown: Duration::from_secs(3600),
        usage: Default::default(),
    }
}

#[tokio::test]
async fn a_second_server_on_the_same_home_refuses_and_leaves_the_first_alone() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let first = server::start_with(config(&home), NetLimits::default(), Bus::default())
        .await
        .unwrap();
    let store = &first.state.store;
    let token = store.list_tokens().unwrap().remove(0);
    let id = "00000000-0000-4000-8000-00000000cafe";
    store
        .insert_request(NewRequest {
            id,
            token: &token,
            agent: AgentKind::Claude,
            model: None,
            message: "in flight",
        })
        .unwrap();
    store.claim_next(AgentKind::Claude).unwrap().unwrap();
    assert!(store.is_running(id).unwrap());
    assert_eq!(server::read_server_pid(&home), Some(std::process::id()));
    assert!(server::home_locked(&home));

    let second = server::start_with(config(&home), NetLimits::default(), Bus::default()).await;
    let Err(e) = second else {
        panic!("the second server started")
    };
    let e = format!("{e:#}");
    assert!(e.contains("already running"), "{e}");
    // The first server's run is still running, not requeued.
    let r = store.get_request(id).unwrap().unwrap();
    assert_eq!(r.status, RequestStatus::Running);
    assert_eq!(server::read_server_pid(&home), Some(std::process::id()));

    first.shutdown().await;
    assert_eq!(server::read_server_pid(&home), None);
    assert!(!server::home_locked(&home));
}
