//! Connection-level limits against a real server: the connection cap, the
//! request-head and request-body deadlines, per-client limits on
//! unauthenticated paths, CORS, the Host check and shutdown.

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
    start_full(rate_limit, net, None).await
}

async fn start_full(rate_limit: u32, net: NetLimits, public_url: Option<String>) -> Net {
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
        public_url,
        name: "Net test".into(),
        tls: None,
        tunnel_token: None,
        update_check: false,
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
    let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
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
            ..NetLimits::default()
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
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
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

/// Opens a connection that sends a claim request head and part of its body,
/// then goes quiet.
async fn stalled_claim(port: u16, step: &str) -> TcpStream {
    let mut tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let head = format!(
        "POST /v1/claim/{step} HTTP/1.1\r\nHost: localhost\r\n\
         Content-Type: application/json\r\nContent-Length: 64\r\n\r\n{{\"code\":"
    );
    tcp.write_all(head.as_bytes()).await.unwrap();
    tcp
}

/// Waits for the server to close `tcp`, returning whatever it sent first.
async fn closed_within(tcp: &mut TcpStream, within: Duration) -> Option<String> {
    let mut buf = Vec::new();
    match tokio::time::timeout(within, tcp.read_to_end(&mut buf)).await {
        Ok(_) => Some(String::from_utf8_lossy(&buf).into_owned()),
        Err(_) => None,
    }
}

#[tokio::test]
async fn a_stalled_request_body_is_cut_off() {
    let t = start(
        60,
        NetLimits {
            max_connections: 2,
            header_read_timeout: Duration::from_secs(1),
            body_read_timeout: Duration::from_secs(1),
            ..NetLimits::default()
        },
    )
    .await;
    let res = raw_get(t.port, "/healthz", Duration::from_secs(5)).await;
    assert!(res.starts_with("HTTP/1.1 200"), "{res}");

    let started = Instant::now();
    let mut a = stalled_claim(t.port, "redeem").await;
    let mut b = stalled_claim(t.port, "preview").await;
    assert!(
        closed_within(&mut a, Duration::from_secs(5))
            .await
            .is_some(),
        "a stalled redeem body must give up its slot"
    );
    assert!(
        closed_within(&mut b, Duration::from_secs(5))
            .await
            .is_some(),
        "a stalled preview body must give up its slot"
    );
    assert!(started.elapsed() < Duration::from_secs(5));

    // Both slots are free again.
    let res = raw_get(t.port, "/healthz", Duration::from_secs(5)).await;
    assert!(res.starts_with("HTTP/1.1 200"), "{res}");
    t.running.shutdown().await;
}

#[tokio::test]
async fn a_slow_but_steady_body_still_hits_the_deadline() {
    let t = start(
        0,
        NetLimits {
            body_read_timeout: Duration::from_secs(1),
            ..NetLimits::default()
        },
    )
    .await;
    let mut tcp = stalled_claim(t.port, "preview").await;
    // One byte at a time, each well inside the deadline, never finishing.
    let started = Instant::now();
    let mut closed = false;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if tcp.write_all(b" ").await.is_err() {
            closed = true;
            break;
        }
    }
    if !closed {
        closed = closed_within(&mut tcp, Duration::from_secs(1))
            .await
            .is_some();
    }
    assert!(closed, "a dribbled body must not hold the connection open");
    assert!(started.elapsed() < Duration::from_secs(6));
    t.running.shutdown().await;
}

#[tokio::test]
async fn a_valid_token_works_from_a_throttled_address() {
    // No tunnel and no proxy trust: every client here shares 127.0.0.1, as
    // they would behind a same-host reverse proxy.
    let t = start(3, NetLimits::default()).await;
    let get = |path: &str, token: &str| {
        t.http
            .get(format!("{}{path}", t.base))
            .bearer_auth(token)
            .send()
    };
    for _ in 0..3 {
        let res = get("/v1/me", "hashi_garbage").await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }
    let res = get("/v1/me", "hashi_garbage").await.unwrap();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);

    // The failures throttle further failures only.
    let res = get("/v1/me", &t.admin).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let res = get("/v1/tokens", &t.admin).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let res = get("/v1/me", "hashi_garbage").await.unwrap();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    t.running.shutdown().await;
}

async fn new_invite(t: &Net) -> String {
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
    v["url"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn a_valid_invite_works_from_a_throttled_address() {
    let t = start(3, NetLimits::default()).await;
    let code = new_invite(&t).await;
    for _ in 0..3 {
        let (status, _) = claim(&t, "preview", "hashi_inv_bogus", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    let (status, _) = claim(&t, "preview", "hashi_inv_bogus", None).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

    let (status, v) = claim(&t, "preview", &code, None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (status, v) = claim(&t, "redeem", &code, None).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    assert!(v["token"].as_str().unwrap().starts_with("hashi_"));

    // A used code is a failure again, and the address is still throttled.
    let (status, _) = claim(&t, "redeem", &code, None).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    t.running.shutdown().await;
}

#[tokio::test]
async fn trust_proxy_keys_clients_by_the_last_forwarded_hop() {
    let t = start(
        3,
        NetLimits {
            trust_proxy: true,
            ..NetLimits::default()
        },
    )
    .await;
    let me = |token: &str, xff: &str| {
        t.http
            .get(format!("{}/v1/me", t.base))
            .bearer_auth(token)
            .header("X-Forwarded-For", xff)
            .send()
    };
    // The proxy appends the real client last; anything before it is the
    // client's own claim and must not buy a new budget.
    for spoof in ["1.1.1.1", "2.2.2.2", "3.3.3.3"] {
        let res = me("hashi_garbage", &format!("{spoof}, 203.0.113.1"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }
    let res = me("hashi_garbage", "4.4.4.4, 203.0.113.1").await.unwrap();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    // A different client behind the same proxy has its own budget.
    let res = me("hashi_garbage", "198.51.100.2").await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    t.running.shutdown().await;
}

async fn preflight(t: &Net, origin: &str) -> reqwest::Response {
    t.http
        .request(reqwest::Method::OPTIONS, format!("{}/v1/me", t.base))
        .header("Origin", origin)
        .header("Access-Control-Request-Method", "GET")
        .header("Access-Control-Request-Headers", "authorization")
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn cross_origin_access_is_off_by_default() {
    let t = start(0, NetLimits::default()).await;
    let res = preflight(&t, "https://attacker.example").await;
    assert!(
        res.headers().get("access-control-allow-origin").is_none(),
        "{:?}",
        res.headers()
    );
    assert!(res.headers().get("access-control-allow-headers").is_none());
    let res = t
        .http
        .get(format!("{}/v1/status", t.base))
        .header("Origin", "https://attacker.example")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.headers().get("access-control-allow-origin").is_none());
    t.running.shutdown().await;
}

#[tokio::test]
async fn an_allowlisted_origin_gets_cross_origin_access() {
    let t = start(
        0,
        NetLimits {
            cors_origins: vec!["https://app.example".into()],
            ..NetLimits::default()
        },
    )
    .await;
    let res = preflight(&t, "https://app.example").await;
    assert_eq!(
        res.headers()
            .get("access-control-allow-origin")
            .map(|v| v.to_str().unwrap()),
        Some("https://app.example")
    );
    let allowed = res
        .headers()
        .get("access-control-allow-headers")
        .unwrap()
        .to_str()
        .unwrap()
        .to_ascii_lowercase();
    assert!(allowed.contains("authorization"), "{allowed}");
    let res = preflight(&t, "https://attacker.example").await;
    assert!(res.headers().get("access-control-allow-origin").is_none());
    t.running.shutdown().await;
}

/// Sends `GET /healthz` with the given Host header and returns the raw response.
async fn with_host(port: u16, host: &str) -> String {
    let mut tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let req = format!("GET /healthz HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    tcp.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), tcp.read_to_end(&mut buf)).await;
    String::from_utf8_lossy(&buf).into_owned()
}

#[tokio::test]
async fn unknown_host_headers_are_refused() {
    let t = start_full(
        0,
        NetLimits {
            allowed_hosts: vec!["box.lan".into()],
            ..NetLimits::default()
        },
        Some("https://hashi.example.com".into()),
    )
    .await;
    let port = t.port;
    for host in [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        "LOCALHOST".to_owned(),
        format!("[::1]:{port}"),
        "192.168.1.20:7878".to_owned(),
        "hashi.example.com".to_owned(),
        "box.lan:7878".to_owned(),
    ] {
        let res = with_host(port, &host).await;
        assert!(res.starts_with("HTTP/1.1 200"), "{host}: {res}");
    }
    for host in ["rebind.attacker.example", "hashi.example.com.evil", "x"] {
        let res = with_host(port, host).await;
        assert!(res.starts_with("HTTP/1.1 421"), "{host}: {res}");
    }
    t.running.shutdown().await;
}

#[tokio::test]
async fn shutdown_drops_stalled_connections_after_the_grace_period() {
    let t = start(
        0,
        NetLimits {
            body_read_timeout: Duration::from_secs(60),
            shutdown_grace: Duration::from_secs(1),
            ..NetLimits::default()
        },
    )
    .await;
    let mut stalled = stalled_claim(t.port, "redeem").await;
    // Let the server read the head and start waiting on the body.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(5), t.running.shutdown())
        .await
        .expect("shutdown must not wait on a stalled body");
    assert!(started.elapsed() < Duration::from_secs(4));
    assert!(
        closed_within(&mut stalled, Duration::from_secs(2))
            .await
            .is_some()
    );
}
