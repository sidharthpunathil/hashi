//! Connection-level limits against a real server: the connection cap, the
//! request-head deadline, and per-client limits on unauthenticated paths.

// Test helpers outside #[test] functions are not covered by clippy.toml's
// test exemptions; panicking on setup failure is the desired behaviour here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::time::{Duration, Instant};

use hashi::agent::{AgentConfig, AgentKind, Limits};
use hashi::events::Bus;
use hashi::net::NetLimits;
use hashi::server::{self, Running, ServerConfig};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

struct Net {
    _dir: tempfile::TempDir,
    running: Running,
    port: u16,
    base: String,
    admin: String,
    http: reqwest::Client,
}

async fn start(rate_limit: u32, net: NetLimits) -> Net {
    let dir = tempfile::tempdir().unwrap();
    let mut agents = HashMap::new();
    for kind in AgentKind::ALL {
        let mut cfg = AgentConfig::default_for(kind);
        cfg.bin = "/nonexistent".into();
        agents.insert(kind, cfg);
    }
    let cfg = ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        home: dir.path().join("home"),
        agents,
        default_agent: AgentKind::Claude,
        concurrency: 1,
        resources: hashi::guard::ResourceSettings::with_probe(std::sync::Arc::new(
            hashi::guard::ManualProbe::roomy(),
        )),
        limits: Limits::default(),
        rate_limit,
        queue_limit: 10,
        retention_days: 14,
        tunnel: false,
        public_url: None,
        name: "Net test".into(),
        tls: None,
        tunnel_token: None,
    };
    let running = server::start_with(cfg, net, Bus::default()).await.unwrap();
    let base = running.state.runtime.local_url.clone();
    let port = base.rsplit(':').next().unwrap().parse().unwrap();
    let admin = running.bootstrap_token.clone().unwrap();
    Net {
        _dir: dir,
        running,
        port,
        base,
        admin,
        http: reqwest::Client::new(),
    }
}

/// Sends one request on a fresh connection and returns the raw response, or
/// an empty string if nothing arrives within `within`.
async fn raw_get(port: u16, path: &str, within: Duration) -> String {
    let Ok(Ok(mut tcp)) =
        tokio::time::timeout(within, TcpStream::connect(("127.0.0.1", port))).await
    else {
        return String::new();
    };
    let req = format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    tcp.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(within, tcp.read_to_end(&mut buf)).await;
    String::from_utf8_lossy(&buf).into_owned()
}

#[tokio::test]
async fn a_connection_past_the_cap_waits_for_a_free_slot() {
    let t = start(
        0,
        NetLimits {
            max_connections: 2,
            header_read_timeout: Duration::from_secs(30),
            trust_forwarded: false,
        },
    )
    .await;
    let idle_a = TcpStream::connect(("127.0.0.1", t.port)).await.unwrap();
    let _idle_b = TcpStream::connect(("127.0.0.1", t.port)).await.unwrap();
    // Let the server accept both, so they hold the two slots.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // The third connection sits in the backlog: it is not served.
    let mut third = TcpStream::connect(("127.0.0.1", t.port)).await.unwrap();
    third
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut buf = Vec::new();
    let early = tokio::time::timeout(Duration::from_millis(700), third.read_to_end(&mut buf)).await;
    assert!(early.is_err(), "served past the cap: {buf:?}");

    // Closing one idle connection frees a slot and the waiting one is served.
    drop(idle_a);
    tokio::time::timeout(Duration::from_secs(5), third.read_to_end(&mut buf))
        .await
        .expect("served once a slot frees")
        .unwrap();
    let res = String::from_utf8_lossy(&buf);
    assert!(res.starts_with("HTTP/1.1 200"), "{res}");

    // The server keeps answering health checks with one slot still taken.
    for _ in 0..3 {
        let res = raw_get(t.port, "/healthz", Duration::from_secs(5)).await;
        assert!(res.starts_with("HTTP/1.1 200"), "{res}");
    }
    t.running.shutdown().await;
}

#[tokio::test]
async fn a_stalled_request_head_is_cut_off() {
    let t = start(
        0,
        NetLimits {
            header_read_timeout: Duration::from_secs(1),
            ..NetLimits::default()
        },
    )
    .await;
    let mut slow = TcpStream::connect(("127.0.0.1", t.port)).await.unwrap();
    slow.write_all(b"GET /heal").await.unwrap();
    let started = Instant::now();
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), slow.read_to_end(&mut buf))
        .await
        .expect("the server must close a stalled connection")
        .ok();
    assert!(started.elapsed() < Duration::from_secs(4));
    assert!(!String::from_utf8_lossy(&buf).contains("200"));

    // A prompt client is unaffected.
    let res = raw_get(t.port, "/healthz", Duration::from_secs(5)).await;
    assert!(res.starts_with("HTTP/1.1 200"), "{res}");
    t.running.shutdown().await;
}

async fn claim(t: &Net, step: &str, code: &str, client: Option<&str>) -> (StatusCode, Value) {
    let mut req = t
        .http
        .post(format!("{}/v1/claim/{step}", t.base))
        .json(&json!({ "code": code }));
    if let Some(ip) = client {
        req = req.header("CF-Connecting-IP", ip);
    }
    let res = req.send().await.unwrap();
    let status = res.status();
    (status, res.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn claim_limits_are_per_client() {
    let t = start(
        3,
        NetLimits {
            trust_forwarded: true,
            ..NetLimits::default()
        },
    )
    .await;
    let res = t
        .http
        .post(format!("{}/v1/invites", t.base))
        .bearer_auth(&t.admin)
        .json(&json!({ "label": "bob" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let v: Value = res.json().await.unwrap();
    let code = v["url"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned();

    let attacker = Some("203.0.113.1");
    for _ in 0..3 {
        let (status, _) = claim(&t, "preview", "hashi_inv_bogus", attacker).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    let (status, v) = claim(&t, "redeem", "hashi_inv_bogus", attacker).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{v}");
    let (status, _) = claim(&t, "preview", &code, attacker).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

    // Someone else can still use their invite.
    let invitee = Some("198.51.100.2");
    let (status, v) = claim(&t, "preview", &code, invitee).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (status, v) = claim(&t, "redeem", &code, invitee).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    assert!(v["token"].as_str().unwrap().starts_with("hashi_"));
    t.running.shutdown().await;
}

#[tokio::test]
async fn the_forwarded_header_is_ignored_without_a_tunnel() {
    let t = start(2, NetLimits::default()).await;
    for ip in ["203.0.113.1", "203.0.113.2"] {
        let (status, _) = claim(&t, "preview", "hashi_inv_bogus", Some(ip)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    // A new header value does not buy a new budget: the peer is the key.
    let (status, _) = claim(&t, "preview", "hashi_inv_bogus", Some("203.0.113.3")).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    t.running.shutdown().await;
}

#[tokio::test]
async fn unknown_tokens_are_throttled_per_client() {
    let t = start(
        3,
        NetLimits {
            trust_forwarded: true,
            ..NetLimits::default()
        },
    )
    .await;
    let me = |token: &str, ip: &str| {
        t.http
            .get(format!("{}/v1/me", t.base))
            .bearer_auth(token)
            .header("CF-Connecting-IP", ip)
            .send()
    };
    for _ in 0..3 {
        let res = me("hashi_garbage", "203.0.113.1").await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }
    let res = me("hashi_garbage", "203.0.113.1").await.unwrap();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    // Another client, and the real admin elsewhere, are unaffected.
    let res = me("hashi_garbage", "198.51.100.2").await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let res = me(&t.admin, "198.51.100.3").await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    t.running.shutdown().await;
}
