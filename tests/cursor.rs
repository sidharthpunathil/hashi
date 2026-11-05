//! End-to-end tests for the Cursor CLI backend: a real server on a random
//! port driving a fake `cursor-agent` that mimics the documented `json`
//! output format.

// Test helpers outside #[test] functions are not covered by clippy.toml's
// test exemptions; panicking on setup failure is the desired behaviour here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use hashi::agent::{AgentConfig, AgentKind, Limits};
use hashi::events::Bus;
use hashi::server::{self, Running, ServerConfig};
use reqwest::StatusCode;
use serde_json::{Value, json};

/// Succeeds with a Claude-shaped result, echoing how it was invoked. A prompt
/// starting with `fail` prints only to stderr and exits non-zero, as the docs
/// describe for real failures.
const FAKE_CURSOR: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "2026.11.05-fake"; exit 0; fi
for last; do :; done
case "$last" in
  fail*) echo 'Error: Authentication required. Run agent login or set CURSOR_API_KEY.' >&2; exit 1 ;;
esac
force=no
resumed=no
prev=""
for a in "$@"; do
  case "$a" in -f|--force|--yolo) force=yes ;; esac
  if [ "$prev" = "--resume" ]; then resumed="$a"; fi
  prev="$a"
done
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":10,"duration_api_ms":10,"result":"cursor says %s (mode=%s force=%s resume=%s)","session_id":"cursor-chat-1"}\n' "$last" "$1" "$force" "$resumed"
"#;

struct Harness {
    _dir: tempfile::TempDir,
    running: Running,
    base: String,
    admin: String,
    http: reqwest::Client,
}

async fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = bin.join("cursor-agent");
    std::fs::write(&script, FAKE_CURSOR).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut cfg = AgentConfig::default_for(AgentKind::Cursor);
    cfg.bin = script;
    let agents = HashMap::from([(AgentKind::Cursor, cfg)]);

    let cfg = ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        home: dir.path().join("home"),
        agents,
        default_agent: AgentKind::Cursor,
        concurrency: 1,
        resources: hashi::guard::ResourceSettings::with_probe(std::sync::Arc::new(
            hashi::guard::ManualProbe::roomy(),
        )),
        limits: Limits {
            timeout: Duration::from_secs(10),
            ..Limits::default()
        },
        rate_limit: 0,
        queue_limit: 100,
        retention_days: 14,
        tunnel: false,
        public_url: None,
        name: "Test Hashi".to_owned(),
        tls: None,
        tunnel_token: None,
        update_check: false,
        tailscale: None,
        helpers: Default::default(),
        account_cooldown: std::time::Duration::from_secs(3600),
    };

    let running = server::start(cfg, Bus::default()).await.unwrap();
    let base = running.state.runtime.local_url.clone();
    let admin = running
        .bootstrap_token
        .clone()
        .expect("first run mints an admin token");
    Harness {
        _dir: dir,
        running,
        base,
        admin,
        http: reqwest::Client::new(),
    }
}

impl Harness {
    async fn post(&self, path: &str, token: &str, body: Value) -> (StatusCode, Value) {
        let res = self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = res.status();
        let text = res.text().await.unwrap();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    async fn user_token(&self, extra: Value) -> String {
        let mut body = json!({ "name": "cursor-user" });
        if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
            b.extend(e.clone());
        }
        let (status, v) = self.post("/v1/tokens", &self.admin, body).await;
        assert_eq!(status, StatusCode::CREATED, "{v}");
        v["token"].as_str().unwrap().to_owned()
    }

    async fn ask(&self, token: &str, message: &str) -> (StatusCode, Value) {
        self.post(
            "/v1/ask",
            token,
            json!({"message": message, "agent": "cursor", "wait": 10}),
        )
        .await
    }
}

#[tokio::test]
async fn ask_returns_cursor_answer_without_force() {
    let h = harness().await;
    let token = h.user_token(json!({})).await;
    let (status, v) = h.ask(&token, "hello").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "completed", "{v}");
    assert_eq!(v["agent"], "cursor");
    assert_eq!(
        v["response"],
        "cursor says hello (mode=-p force=no resume=no)"
    );
    h.running.shutdown().await;
}

#[tokio::test]
async fn stateful_tokens_resume_the_cursor_chat() {
    let h = harness().await;
    let token = h.user_token(json!({"sessionMode": "stateful"})).await;
    let (_, first) = h.ask(&token, "one").await;
    assert_eq!(
        first["response"],
        "cursor says one (mode=-p force=no resume=no)"
    );
    let (_, second) = h.ask(&token, "two").await;
    assert_eq!(
        second["response"],
        "cursor says two (mode=-p force=no resume=cursor-chat-1)"
    );
    h.running.shutdown().await;
}

#[tokio::test]
async fn stderr_only_failure_is_reported() {
    let h = harness().await;
    let token = h.user_token(json!({})).await;
    let (status, v) = h.ask(&token, "fail please").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "failed", "{v}");
    assert!(
        v["error"]
            .as_str()
            .unwrap()
            .contains("Authentication required"),
        "{v}"
    );
    h.running.shutdown().await;
}
