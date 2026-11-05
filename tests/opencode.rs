//! End-to-end tests for the opencode backend: a real server on a random port
//! driving a fake `opencode` executable that prints opencode V2's
//! `run --format json` event stream.

// Test helpers outside #[test] functions are not covered by clippy.toml's
// test exemptions; panicking on setup failure is the desired behaviour here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hashi::agent::{AgentConfig, AgentKind, Limits};
use hashi::events::Bus;
use hashi::server::{self, Running, ServerConfig};
use reqwest::StatusCode;
use serde_json::{Value, json};

/// Answers with the words after `--` joined back with spaces, the session it
/// was asked to resume, and whether `PWD` names its working directory. Two
/// steps report $0.0125 and $0.0025.
const FAKE_OPENCODE: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "2.2.5"; exit 0; fi
[ "$1" = "run" ] || { echo "expected run" >&2; exit 2; }
session=""
resume=no
prev=""
while [ $# -gt 0 ]; do
  case "$1" in
    --) shift; break ;;
    --auto|--yolo|--dangerously-skip-permissions) echo "auto-approve flag passed" >&2; exit 3 ;;
  esac
  if [ "$prev" = "-s" ]; then resume="$1"; session="$1"; fi
  prev="$1"
  shift
done
[ -n "$session" ] || session="ses_fake1"
prompt="$*"
pwdok=no
if [ "$(cd "$PWD" && pwd -P)" = "$(pwd -P)" ]; then pwdok=yes; fi
case "$prompt" in
  fail*)
    printf '{"type":"error","timestamp":1,"sessionID":"%s","error":{"type":"api","message":"Invalid API key","status":401}}\n' "$session"
    exit 1 ;;
esac
printf '{"type":"step_start","timestamp":1,"sessionID":"%s","part":{"type":"step-start"}}\n' "$session"
printf '{"type":"step_finish","timestamp":2,"sessionID":"%s","part":{"type":"step-finish","reason":"tool-calls","cost":0.0125,"tokens":{"input":10,"output":2,"reasoning":0,"cache":{"read":0,"write":0}}}}\n' "$session"
printf '{"type":"step_start","timestamp":3,"sessionID":"%s","part":{"type":"step-start"}}\n' "$session"
printf '{"type":"text","timestamp":4,"sessionID":"%s","part":{"type":"text","text":"opencode says %s (resume=%s, pwd=%s)"}}\n' "$session" "$prompt" "$resume" "$pwdok"
printf '{"type":"step_finish","timestamp":5,"sessionID":"%s","part":{"type":"step-finish","reason":"stop","cost":0.0025,"tokens":{"input":20,"output":5,"reasoning":1,"cache":{"read":4,"write":0}}}}\n' "$session"
"#;

struct Harness {
    _dir: tempfile::TempDir,
    running: Running,
    base: String,
    admin: String,
    http: reqwest::Client,
}

fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

async fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();

    let mut opencode = AgentConfig::default_for(AgentKind::Opencode);
    opencode.bin = write_script(&bin, "opencode", FAKE_OPENCODE);
    let agents = HashMap::from([(AgentKind::Opencode, opencode)]);

    let cfg = ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        home: dir.path().join("home"),
        agents,
        default_agent: AgentKind::Opencode,
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
    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        token: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut req = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = req.send().await.unwrap();
        let status = res.status();
        let text = res.text().await.unwrap();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    async fn ask(&self, token: &str, message: &str) -> Value {
        let (status, v) = self
            .call(
                reqwest::Method::POST,
                "/v1/ask",
                token,
                Some(json!({"message": message, "agent": "opencode", "wait": 10})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{v}");
        v
    }

    async fn user_token(&self, extra: Value) -> String {
        let mut body = json!({ "name": "test-user" });
        if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
            b.extend(e.clone());
        }
        let (status, v) = self
            .call(reqwest::Method::POST, "/v1/tokens", &self.admin, Some(body))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{v}");
        v["token"].as_str().unwrap().to_owned()
    }
}

#[tokio::test]
async fn ask_returns_the_opencode_answer() {
    let h = harness().await;
    let token = h.user_token(json!({})).await;
    let v = h.ask(&token, "explain  -- add -s x").await;
    assert_eq!(v["status"], "completed", "{v}");
    assert_eq!(v["agent"], "opencode");
    assert_eq!(
        v["response"],
        "opencode says explain  -- add -s x (resume=no, pwd=yes)"
    );
    assert_eq!(v["usage"]["inputTokens"], 34);
    assert_eq!(v["usage"]["outputTokens"], 8);
    h.running.shutdown().await;
}

#[tokio::test]
async fn reported_errors_fail_the_request() {
    let h = harness().await;
    let token = h.user_token(json!({})).await;
    let v = h.ask(&token, "fail please").await;
    assert_eq!(v["status"], "failed");
    assert_eq!(v["error"], "Invalid API key");
    h.running.shutdown().await;
}

#[tokio::test]
async fn stateful_token_resumes_its_session() {
    let h = harness().await;
    let token = h.user_token(json!({"sessionMode": "stateful"})).await;
    let first = h.ask(&token, "one").await;
    assert_eq!(first["response"], "opencode says one (resume=no, pwd=yes)");
    let second = h.ask(&token, "two").await;
    assert_eq!(
        second["response"],
        "opencode says two (resume=ses_fake1, pwd=yes)"
    );

    let (status, _) = h
        .call(
            reqwest::Method::DELETE,
            "/v1/session/opencode",
            &token,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let third = h.ask(&token, "three").await;
    assert_eq!(
        third["response"],
        "opencode says three (resume=no, pwd=yes)"
    );
    h.running.shutdown().await;
}

#[tokio::test]
async fn reported_cost_shows_up_in_me() {
    let h = harness().await;
    let token = h.user_token(json!({})).await;
    h.ask(&token, "hi").await;
    let (status, v) = h.call(reqwest::Method::GET, "/v1/me", &token, None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["usage"]["todayRequests"], 1);
    let cost = v["usage"]["todayCostUsd"].as_f64().unwrap();
    assert!((cost - 0.015).abs() < 1e-9, "{v}");
    h.running.shutdown().await;
}
