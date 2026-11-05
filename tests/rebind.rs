//! Moving a live server to another port: the new listener answers at once,
//! the old one stops taking connections but finishes the request it was
//! serving, and a port that cannot be bound changes nothing.

// Test helpers outside #[test] functions are not covered by clippy.toml's
// test exemptions; panicking on setup failure is the desired behaviour here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use hashi::agent::{AgentKind, Limits};
use hashi::events::Bus;
use hashi::guard::{GIB, ManualProbe, ResourceSettings};
use hashi::net::NetLimits;
use hashi::server::{self, RebindError, Running, ServerConfig};
use hashi::store::NewRequest;
use reqwest::StatusCode;

async fn start(dir: &tempfile::TempDir, bus: Bus) -> Running {
    let cfg = ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        home: dir.path().join("home"),
        agents: HashMap::new(),
        default_agent: AgentKind::Claude,
        concurrency: 1,
        // No memory free: queued work is held, so a request stays queued
        // and a long poll on it stays open.
        resources: ResourceSettings::with_probe(std::sync::Arc::new(ManualProbe::new(16 * GIB, 0))),
        limits: Limits::default(),
        rate_limit: 0,
        queue_limit: 10,
        retention_days: 14,
        tunnel: false,
        public_url: None,
        name: "Rebind test".into(),
        tls: None,
        tunnel_token: None,
        update_check: false,
        tailscale: None,
        helpers: Default::default(),
        account_cooldown: Duration::from_secs(3600),
        usage: Default::default(),
    };
    server::start_with(cfg, NetLimits::default(), bus)
        .await
        .unwrap()
}

fn port_of(url: &str) -> u16 {
    url.rsplit(':').next().unwrap().parse().unwrap()
}

/// Whether a new connection to `port` is refused within a second.
async fn refused(port: u16) -> bool {
    for _ in 0..20 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err()
        {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test]
async fn a_live_server_moves_and_finishes_the_request_in_flight() {
    let dir = tempfile::tempdir().unwrap();
    let bus = Bus::default();
    let running = start(&dir, bus.clone()).await;
    let rt = std::sync::Arc::clone(&running.state.runtime);
    let old = rt.local_url();
    let old_port = port_of(&old);
    let admin = running.bootstrap_token.clone().unwrap();

    // A queued request, so a long poll on it stays open.
    let token = running.state.store.list_tokens().unwrap().remove(0);
    let id = "00000000-0000-4000-8000-00000000beef";
    running
        .state
        .store
        .insert_request(NewRequest {
            id,
            token: &token,
            agent: AgentKind::Claude,
            model: None,
            message: "wait for me",
        })
        .unwrap();
    let http = reqwest::Client::new();
    let poll = {
        let http = http.clone();
        let url = format!("{old}/v1/ask/{id}?wait=2");
        let admin = admin.clone();
        tokio::spawn(async move { http.get(url).bearer_auth(admin).send().await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;

    let moved = server::rebind(&rt, &bus, "127.0.0.1:0".parse().unwrap()).unwrap();
    assert_ne!(moved.addr.port(), old_port);
    assert_eq!(rt.local_url(), moved.url);
    assert_eq!(moved.url, format!("http://{}", moved.addr));

    // The new port answers at once, with the same API.
    let res = http
        .get(format!("{}/v1/me", moved.url))
        .bearer_auth(&admin)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    // The old one takes no new connections.
    assert!(refused(old_port).await, "the old port still accepts");
    // But the request it was serving finishes there.
    let res = poll.await.unwrap().unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);

    running.shutdown().await;
    assert!(refused(moved.addr.port()).await);
}

#[tokio::test]
async fn a_port_in_use_is_refused_and_nothing_changes() {
    let dir = tempfile::tempdir().unwrap();
    let bus = Bus::default();
    let running = start(&dir, bus.clone()).await;
    let rt = std::sync::Arc::clone(&running.state.runtime);
    let before = rt.local_url();

    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let busy: SocketAddr = taken.local_addr().unwrap();
    let err = server::rebind(&rt, &bus, busy).unwrap_err();
    assert!(matches!(err, RebindError::Bind(_)), "{err:?}");
    assert!(err.to_string().contains("in use"), "{err}");
    assert_eq!(rt.local_url(), before);

    // An address this machine does not have.
    let err = server::rebind(&rt, &bus, "192.0.2.1:0".parse().unwrap()).unwrap_err();
    assert!(err.to_string().contains("192.0.2.1"), "{err}");

    // Still serving where it was.
    let res = reqwest::get(format!("{before}/healthz")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    running.shutdown().await;
}

#[tokio::test]
async fn moving_to_the_same_address_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let bus = Bus::default();
    let running = start(&dir, bus.clone()).await;
    let rt = std::sync::Arc::clone(&running.state.runtime);
    let here = rt.listen.addr().unwrap();
    let same = server::rebind(&rt, &bus, here).unwrap();
    assert_eq!(same.addr, here);
    assert!(same.notes.iter().any(|n| n.contains("already")));
    running.shutdown().await;
}
