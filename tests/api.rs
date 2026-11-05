//! End-to-end tests: a real server on a random port, driving fake agent
//! executables that mimic the Claude Code and Codex output formats.

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

/// Prints its arguments back so tests can assert on how it was invoked.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "9.9.9 (Fake Claude)"; exit 0; fi
for last; do :; done
case "$last" in
  fail*) echo '{"type":"result","is_error":true,"subtype":"error_during_execution","result":"simulated failure"}'; exit 1 ;;
  slow*) sleep 2 ;;
esac
resumed=no
prev=""
for a in "$@"; do
  if [ "$prev" = "--resume" ]; then resumed="$a"; fi
  prev="$a"
done
printf '{"type":"result","is_error":false,"result":"claude says %s (resume=%s)","session_id":"claude-session-1","total_cost_usd":0.01,"usage":{"input_tokens":5,"output_tokens":7}}\n' "$last" "$resumed"
"#;

const FAKE_CODEX: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "codex-cli 0.0.0-fake"; exit 0; fi
for last; do :; done
echo '{"type":"thread.started","thread_id":"codex-thread-1"}'
echo '{"type":"turn.started"}'
case "$last" in
  fail*) echo 'noisy log line' >&2; echo '{"type":"turn.failed","error":{"message":"please sign in again"}}'; exit 1 ;;
esac
printf '{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"codex says %s (args=%s)"}}\n' "$last" "$2"
echo '{"type":"turn.completed","usage":{"input_tokens":3,"output_tokens":4}}'
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
    harness_with(|_| {}).await
}

async fn harness_with(tweak: impl FnOnce(&mut ServerConfig)) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();

    let mut agents = HashMap::new();
    for (kind, script) in [
        (AgentKind::Claude, FAKE_CLAUDE),
        (AgentKind::Codex, FAKE_CODEX),
    ] {
        let mut cfg = AgentConfig::default_for(kind);
        cfg.bin = write_script(&bin, kind.as_str(), script);
        agents.insert(kind, cfg);
    }

    let mut cfg = ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        home: dir.path().join("home"),
        agents,
        default_agent: AgentKind::Claude,
        concurrency: 1,
        limits: Limits {
            timeout: Duration::from_secs(10),
            ..Limits::default()
        },
        rate_limit: 0,
        queue_limit: 100,
        retention_days: 14,
        tunnel: false,
    };
    tweak(&mut cfg);

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
        token: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut req = self.http.request(method, format!("{}{path}", self.base));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
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

    async fn post(&self, path: &str, token: &str, body: Value) -> (StatusCode, Value) {
        self.call(reqwest::Method::POST, path, Some(token), Some(body))
            .await
    }

    async fn get(&self, path: &str, token: Option<&str>) -> (StatusCode, Value) {
        self.call(reqwest::Method::GET, path, token, None).await
    }

    async fn user_token(&self, extra: Value) -> String {
        let mut body = json!({ "name": "test-user" });
        if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
            b.extend(e.clone());
        }
        let (status, v) = self.post("/v1/tokens", &self.admin, body).await;
        assert_eq!(status, StatusCode::CREATED, "{v}");
        v["token"].as_str().unwrap().to_owned()
    }
}

#[tokio::test]
async fn healthz_and_status_are_public() {
    let h = harness().await;
    assert_eq!(h.get("/healthz", None).await.0, StatusCode::OK);
    let (status, v) = h.get("/v1/status", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["agents"][0]["available"], true);
    h.running.shutdown().await;
}

#[tokio::test]
async fn ask_requires_a_token() {
    let h = harness().await;
    let (status, v) = h
        .call(
            reqwest::Method::POST,
            "/v1/ask",
            None,
            Some(json!({"message": "hi"})),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(v["error"]["code"], "unauthorized");
    let (status, _) = h
        .post(
            "/v1/ask",
            "hashi_not-a-real-token",
            json!({"message": "hi"}),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    h.running.shutdown().await;
}

#[tokio::test]
async fn ask_with_wait_returns_claude_answer() {
    let h = harness().await;
    let token = h.user_token(json!({})).await;
    let (status, v) = h
        .post("/v1/ask", &token, json!({"message": "hello", "wait": 10}))
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "completed");
    assert_eq!(v["response"], "claude says hello (resume=no)");
    assert_eq!(v["usage"]["outputTokens"], 7);
    h.running.shutdown().await;
}

#[tokio::test]
async fn ask_routes_to_codex_with_read_only_sandbox() {
    let h = harness().await;
    let token = h.user_token(json!({})).await;
    let (status, v) = h
        .post(
            "/v1/ask",
            &token,
            json!({"message": "yo", "agent": "codex", "wait": 10}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["agent"], "codex");
    assert_eq!(v["response"], "codex says yo (args=--json)");
    h.running.shutdown().await;
}

#[tokio::test]
async fn ask_without_wait_can_be_polled() {
    let h = harness().await;
    let token = h.user_token(json!({})).await;
    let (status, v) = h
        .post("/v1/ask", &token, json!({"message": "poll me"}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let poll = v["poll"].as_str().unwrap().to_owned();
    let (status, v) = h.get(&format!("{poll}?wait=10"), Some(&token)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "completed");
    h.running.shutdown().await;
}

#[tokio::test]
async fn agent_failures_are_reported() {
    let h = harness().await;
    let token = h.user_token(json!({})).await;
    let (status, v) = h
        .post(
            "/v1/ask",
            &token,
            json!({"message": "fail now", "wait": 10}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["status"], "failed");
    assert!(
        v["error"].as_str().unwrap().contains("simulated failure"),
        "{v}"
    );
    h.running.shutdown().await;
}

#[tokio::test]
async fn agent_reported_errors_win_over_stderr_noise() {
    let h = harness().await;
    let token = h.user_token(json!({})).await;
    let (_, v) = h
        .post(
            "/v1/ask",
            &token,
            json!({"message": "fail", "agent": "codex", "wait": 10}),
        )
        .await;
    assert_eq!(v["status"], "failed");
    assert_eq!(v["error"], "please sign in again");
    h.running.shutdown().await;
}

#[tokio::test]
async fn stateful_tokens_resume_their_session() {
    let h = harness().await;
    let token = h.user_token(json!({"sessionMode": "stateful"})).await;
    let (_, first) = h
        .post("/v1/ask", &token, json!({"message": "one", "wait": 10}))
        .await;
    assert_eq!(first["response"], "claude says one (resume=no)");
    let (_, second) = h
        .post("/v1/ask", &token, json!({"message": "two", "wait": 10}))
        .await;
    assert_eq!(
        second["response"],
        "claude says two (resume=claude-session-1)"
    );

    let (status, _) = h
        .call(
            reqwest::Method::DELETE,
            "/v1/session/claude",
            Some(&token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, third) = h
        .post("/v1/ask", &token, json!({"message": "three", "wait": 10}))
        .await;
    assert_eq!(third["response"], "claude says three (resume=no)");
    h.running.shutdown().await;
}

#[tokio::test]
async fn ask_rejects_bad_input() {
    let h = harness().await;
    let token = h.user_token(json!({})).await;
    let cases = [
        json!({"message": ""}),
        json!({"message": "hi", "agent": "gemini"}),
        json!({"message": "hi", "model": "--dangerously-skip-permissions"}),
        json!({"message": "hi", "surprise": true}),
        json!({"message": "x".repeat(33 * 1024)}),
    ];
    for body in cases {
        let (status, v) = h.post("/v1/ask", &token, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    }
    h.running.shutdown().await;
}

#[tokio::test]
async fn oversized_bodies_are_refused() {
    let h = harness().await;
    let token = h.user_token(json!({})).await;
    let (status, _) = h
        .post("/v1/ask", &token, json!({"message": "x".repeat(70 * 1024)}))
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    h.running.shutdown().await;
}

#[tokio::test]
async fn tokens_scoped_to_one_agent_cannot_use_the_other() {
    let h = harness().await;
    let token = h.user_token(json!({"agents": ["codex"]})).await;
    let (status, _) = h
        .post(
            "/v1/ask",
            &token,
            json!({"message": "hi", "agent": "claude"}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    h.running.shutdown().await;
}

#[tokio::test]
async fn requests_are_private_to_their_token() {
    let h = harness().await;
    let alice = h.user_token(json!({})).await;
    let bob = h.user_token(json!({})).await;
    let (_, v) = h
        .post("/v1/ask", &alice, json!({"message": "secret", "wait": 10}))
        .await;
    let poll = v["poll"].as_str().unwrap().to_owned();
    assert_eq!(h.get(&poll, Some(&bob)).await.0, StatusCode::NOT_FOUND);
    assert_eq!(h.get(&poll, Some(&h.admin)).await.0, StatusCode::OK);
    h.running.shutdown().await;
}

#[tokio::test]
async fn token_management_is_admin_only() {
    let h = harness().await;
    let user = h.user_token(json!({})).await;
    assert_eq!(
        h.get("/v1/tokens", Some(&user)).await.0,
        StatusCode::FORBIDDEN
    );
    let (status, v) = h.get("/v1/tokens", Some(&h.admin)).await;
    assert_eq!(status, StatusCode::OK);
    let tokens = v["tokens"].as_array().unwrap();
    assert_eq!(tokens.len(), 2);
    // Secrets and hashes never leave the server.
    assert!(!v.to_string().contains(&user));
    assert!(tokens.iter().all(|t| t.get("hash").is_none()));
    h.running.shutdown().await;
}

#[tokio::test]
async fn revoking_tokens() {
    let h = harness().await;
    let user = h.user_token(json!({})).await;
    let (_, v) = h.get("/v1/tokens", Some(&h.admin)).await;
    let ids: Vec<(String, String)> = v["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["id"].as_str().unwrap().to_owned(),
                t["role"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let admin_id = &ids.iter().find(|(_, r)| r == "admin").unwrap().0;
    let user_id = &ids.iter().find(|(_, r)| r == "user").unwrap().0;

    let (status, _) = h
        .call(
            reqwest::Method::DELETE,
            &format!("/v1/tokens/{admin_id}"),
            Some(&h.admin),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = h
        .call(
            reqwest::Method::DELETE,
            &format!("/v1/tokens/{user_id}"),
            Some(&h.admin),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = h.post("/v1/ask", &user, json!({"message": "hi"})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    h.running.shutdown().await;
}

#[tokio::test]
async fn rate_limit_applies_per_token() {
    let h = harness_with(|c| c.rate_limit = 2).await;
    let token = h.user_token(json!({})).await;
    let mut codes = Vec::new();
    for _ in 0..3 {
        codes.push(h.get("/v1/ask/none", Some(&token)).await.0);
    }
    assert_eq!(
        codes,
        [
            StatusCode::NOT_FOUND,
            StatusCode::NOT_FOUND,
            StatusCode::TOO_MANY_REQUESTS
        ]
    );
    h.running.shutdown().await;
}

#[tokio::test]
async fn claude_and_codex_run_in_parallel() {
    let h = harness().await;
    let token = h.user_token(json!({})).await;
    let start = std::time::Instant::now();
    let slow = h.post(
        "/v1/ask",
        &token,
        json!({"message": "slow one", "wait": 10}),
    );
    let quick = h.post(
        "/v1/ask",
        &token,
        json!({"message": "quick", "agent": "codex", "wait": 10}),
    );
    let ((_, a), (_, b)) = tokio::join!(slow, quick);
    assert_eq!(
        (a["status"].as_str(), b["status"].as_str()),
        (Some("completed"), Some("completed"))
    );
    assert!(
        start.elapsed() < Duration::from_secs(4),
        "codex should not wait behind claude"
    );
    h.running.shutdown().await;
}

#[tokio::test]
async fn data_directory_is_private() {
    let h = harness().await;
    let db = h._dir.path().join("home/hashi.db");
    let mode = std::fs::metadata(&db).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let dir_mode = std::fs::metadata(h._dir.path().join("home"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o700);
    h.running.shutdown().await;
}
