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
        public_url: None,
        name: "Test Hashi".to_owned(),
        tls: None,
        tunnel_token: None,
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
        json!({"message": "hi", "agent": "copilot"}),
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

// ── sharing: limits, pause, /v1/me, audit ─────────────────────────────────

impl Harness {
    async fn patch(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.call(reqwest::Method::PATCH, path, Some(&self.admin), Some(body))
            .await
    }

    async fn token_id(&self, name: &str) -> String {
        let (_, v) = self.get("/v1/tokens", Some(&self.admin)).await;
        v["tokens"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}

#[tokio::test]
async fn paused_token_cannot_ask_but_can_read_me() {
    let h = harness().await;
    let token = h.user_token(json!({"name": "pausable"})).await;
    let id = h.token_id("pausable").await;
    let (status, v) = h
        .patch(&format!("/v1/tokens/{id}"), json!({"paused": true}))
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["record"]["status"], "paused");

    let (status, v) = h.post("/v1/ask", &token, json!({"message": "hi"})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(v["error"]["code"], "token_paused");
    let (status, v) = h.get("/v1/me", Some(&token)).await;
    assert_eq!(
        (status, v["status"].as_str()),
        (StatusCode::OK, Some("paused"))
    );

    h.patch(&format!("/v1/tokens/{id}"), json!({"paused": false}))
        .await;
    let (status, _) = h
        .post("/v1/ask", &token, json!({"message": "hi", "wait": 10}))
        .await;
    assert_eq!(status, StatusCode::OK);
    h.running.shutdown().await;
}

#[tokio::test]
async fn daily_request_cap_is_exact() {
    let h = harness().await;
    let token = h.user_token(json!({"limits": {"dailyRequests": 2}})).await;
    for _ in 0..2 {
        let (status, v) = h
            .post("/v1/ask", &token, json!({"message": "hi", "wait": 10}))
            .await;
        assert_eq!(status, StatusCode::OK, "{v}");
    }
    let (status, v) = h.post("/v1/ask", &token, json!({"message": "hi"})).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(v["error"]["code"], "limit_reached");
    assert_eq!(v["error"]["limit"], "dailyRequests");
    assert!(
        v["error"]["resetAt"]
            .as_str()
            .unwrap()
            .ends_with("T00:00:00Z")
    );
    h.running.shutdown().await;
}

#[tokio::test]
async fn max_pending_blocks_extra_prompts() {
    let h = harness().await;
    let token = h.user_token(json!({"limits": {"maxPending": 1}})).await;
    let (status, _) = h
        .post("/v1/ask", &token, json!({"message": "slow one"}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, v) = h
        .post("/v1/ask", &token, json!({"message": "another"}))
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(v["error"]["code"], "too_many_pending");
    h.running.shutdown().await;
}

#[tokio::test]
async fn budget_cap_counts_reported_cost() {
    let h = harness().await;
    // The fake Claude reports $0.01 per answer.
    let token = h
        .user_token(json!({"limits": {"dailyBudgetUsd": 0.01}}))
        .await;
    let (status, _) = h
        .post("/v1/ask", &token, json!({"message": "hi", "wait": 10}))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, v) = h.post("/v1/ask", &token, json!({"message": "again"})).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(v["error"]["limit"], "dailyBudgetUsd");
    h.running.shutdown().await;
}

#[tokio::test]
async fn patch_token_updates_limits_and_audits() {
    let h = harness().await;
    h.user_token(json!({"name": "ci"})).await;
    let id = h.token_id("ci").await;
    let (status, v) = h
        .patch(
            &format!("/v1/tokens/{id}"),
            json!({"limits": {"monthlyRequests": 50}, "note": "CI bot"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["record"]["limits"]["monthlyRequests"], 50);
    assert_eq!(v["record"]["note"], "CI bot");
    let (_, audit) = h.get("/v1/audit", Some(&h.admin)).await;
    let first = &audit["entries"][0];
    assert_eq!(
        (first["action"].as_str(), first["target"].as_str()),
        (Some("token.update"), Some("ci"))
    );
    h.running.shutdown().await;
}

#[tokio::test]
async fn patch_rejects_bad_input() {
    let h = harness().await;
    h.user_token(json!({"name": "x"})).await;
    let id = h.token_id("x").await;
    for body in [
        json!({}),
        json!({"limits": {"maxPending": 0}}),
        json!({"limits": {"dailyBudgetUsd": -1}}),
        json!({"expiresInDays": 99999}),
        json!({"surprise": 1}),
    ] {
        let (status, v) = h.patch(&format!("/v1/tokens/{id}"), body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    }
    let (status, _) = h.patch("/v1/tokens/nope", json!({"paused": true})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    h.running.shutdown().await;
}

#[tokio::test]
async fn admin_cannot_pause_itself() {
    let h = harness().await;
    let (_, v) = h.get("/v1/tokens", Some(&h.admin)).await;
    let admin_id = v["tokens"][0]["id"].as_str().unwrap().to_owned();
    let (status, _) = h
        .patch(&format!("/v1/tokens/{admin_id}"), json!({"paused": true}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    h.running.shutdown().await;
}

#[tokio::test]
async fn expired_token_sees_status_in_me() {
    let h = harness().await;
    let token = h
        .user_token(json!({"name": "brief", "expiresInDays": 1}))
        .await;
    let id = h.token_id("brief").await;
    // Expire it by hand.
    let mut t = h.running.state.store.get_token(&id).unwrap().unwrap();
    t.expires_at = Some(1);
    h.running.state.store.save_token_policy(&t).unwrap();

    let (status, _) = h.post("/v1/ask", &token, json!({"message": "hi"})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, v) = h.get("/v1/me", Some(&token)).await;
    assert_eq!(
        (status, v["status"].as_str()),
        (StatusCode::OK, Some("expired"))
    );
    h.running.shutdown().await;
}

#[tokio::test]
async fn me_reports_usage() {
    let h = harness().await;
    let token = h.user_token(json!({"limits": {"dailyRequests": 10}})).await;
    h.post("/v1/ask", &token, json!({"message": "hi", "wait": 10}))
        .await;
    let (status, v) = h.get("/v1/me", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["usage"]["todayRequests"], 1);
    assert_eq!(v["limits"]["dailyRequests"], 10);
    assert_eq!(v["server"]["name"], "Test Hashi");
    assert!((v["usage"]["todayCostUsd"].as_f64().unwrap() - 0.01).abs() < 1e-9);
    h.running.shutdown().await;
}

// ── sharing: invites and claim pages ──────────────────────────────────────

async fn new_invite(h: &Harness, body: Value) -> (String, Value) {
    let (status, v) = h.post("/v1/invites", &h.admin, body).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    let url = v["url"].as_str().unwrap().to_owned();
    let code = url.rsplit('/').next().unwrap().to_owned();
    (code, v)
}

async fn claim(h: &Harness, step: &str, code: &str) -> (StatusCode, Value) {
    h.call(
        reqwest::Method::POST,
        &format!("/v1/claim/{step}"),
        None,
        Some(json!({"code": code})),
    )
    .await
}

#[tokio::test]
async fn invite_flow_creates_scoped_token() {
    let h = harness().await;
    let (code, _) = new_invite(
        &h,
        json!({"label": "alice", "agents": ["codex"], "limits": {"dailyRequests": 5}}),
    )
    .await;
    let (status, preview) = claim(&h, "preview", &code).await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["label"], "alice");
    assert_eq!(preview["server"], "Test Hashi");
    assert!(
        !preview.to_string().contains("hashi_"),
        "preview must not leak secrets"
    );

    let (status, r) = claim(&h, "redeem", &code).await;
    assert_eq!(status, StatusCode::CREATED, "{r}");
    let token = r["token"].as_str().unwrap().to_owned();
    assert!(r["snippets"]["curl"].as_str().unwrap().contains(&token));
    assert_eq!(
        r["limits"]["maxPending"], 3,
        "invites default to 3 in flight"
    );

    let (_, me) = h.get("/v1/me", Some(&token)).await;
    assert_eq!(
        (me["role"].as_str(), me["agents"].clone()),
        (Some("user"), json!(["codex"]))
    );
    let (status, _) = h
        .post(
            "/v1/ask",
            &token,
            json!({"message": "hi", "agent": "claude"}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = h
        .post(
            "/v1/ask",
            &token,
            json!({"message": "hi", "agent": "codex", "wait": 10}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = h.get("/v1/tokens", Some(&token)).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "invited tokens are never admins"
    );

    let (_, audit) = h.get("/v1/audit", Some(&h.admin)).await;
    let actions: Vec<&str> = audit["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert!(actions.contains(&"invite.create") && actions.contains(&"invite.claim"));
    h.running.shutdown().await;
}

#[tokio::test]
async fn second_redeem_is_409() {
    let h = harness().await;
    let (code, _) = new_invite(&h, json!({"label": "bob"})).await;
    assert_eq!(claim(&h, "redeem", &code).await.0, StatusCode::CREATED);
    let (status, v) = claim(&h, "redeem", &code).await;
    assert_eq!(
        (status, v["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("already_claimed"))
    );
    assert_eq!(claim(&h, "preview", &code).await.0, StatusCode::CONFLICT);
    h.running.shutdown().await;
}

#[tokio::test]
async fn invite_without_public_url_is_local_only() {
    let h = harness().await;
    let (_, v) = new_invite(&h, json!({"label": "carol"})).await;
    assert_eq!(v["localOnly"], true);
    assert!(
        v["url"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{}/claim/hashi_inv_", h.base))
    );
    h.running.shutdown().await;
}

#[tokio::test]
async fn invite_uses_configured_public_url() {
    let h = harness_with(|c| c.public_url = Some("https://hashi.example.com".into())).await;
    let (_, v) = new_invite(&h, json!({"label": "dan"})).await;
    assert_eq!(v["localOnly"], false);
    assert!(
        v["url"]
            .as_str()
            .unwrap()
            .starts_with("https://hashi.example.com/claim/hashi_inv_")
    );
    h.running.shutdown().await;
}

#[tokio::test]
async fn cancelled_invite_is_404() {
    let h = harness().await;
    let (code, v) = new_invite(&h, json!({"label": "erin"})).await;
    let id = v["invite"]["id"].as_str().unwrap().to_owned();
    let (status, _) = h
        .call(
            reqwest::Method::DELETE,
            &format!("/v1/invites/{id}"),
            Some(&h.admin),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, v) = claim(&h, "redeem", &code).await;
    assert_eq!(
        (status, v["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("invite_not_found"))
    );
    h.running.shutdown().await;
}

#[tokio::test]
async fn invite_rejects_bad_input() {
    let h = harness().await;
    for body in [
        json!({"label": ""}),
        json!({"label": "x", "linkHours": 5}),
        json!({"label": "x", "role": "admin"}),
        json!({"label": "x", "limits": {"maxPending": 0}}),
    ] {
        let (status, v) = h.post("/v1/invites", &h.admin, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    }
    let (status, _) = claim(&h, "redeem", "not-a-code").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    h.running.shutdown().await;
}

#[tokio::test]
async fn claim_page_has_strict_csp() {
    let h = harness().await;
    for path in ["/claim/hashi_inv_whatever", "/me", "/assets/hashi.js"] {
        let res = h
            .http
            .get(format!("{}{path}", h.base))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "{path}");
        let csp = res.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .to_owned();
        assert!(csp.contains("default-src 'none'") && csp.contains("script-src 'self'"));
        assert_eq!(res.headers()["referrer-policy"], "no-referrer");
        assert_eq!(res.headers()["cache-control"], "no-store");
    }
    h.running.shutdown().await;
}

#[tokio::test]
async fn each_agent_gets_its_own_workspace() {
    const PWD_CLAUDE: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "9.9.9 (Fake Claude)"; exit 0; fi
printf '{"type":"result","is_error":false,"result":"%s"}\n' "$(pwd)"
"#;
    const PWD_CODEX: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "codex-cli 0.0.0-fake"; exit 0; fi
printf '{"type":"item.completed","item":{"type":"agent_message","text":"%s"}}\n' "$(pwd)"
"#;
    let bins = tempfile::tempdir().unwrap();
    let claude = write_script(bins.path(), "claude", PWD_CLAUDE);
    let codex = write_script(bins.path(), "codex", PWD_CODEX);
    let h = harness_with(|c| {
        c.agents.get_mut(&AgentKind::Claude).unwrap().bin = claude;
        c.agents.get_mut(&AgentKind::Codex).unwrap().bin = codex;
    })
    .await;
    let token = h.user_token(json!({})).await;
    let (_, a) = h.post("/v1/ask", &token, json!({"message": "where", "agent": "claude", "wait": 10})).await;
    let (_, b) = h.post("/v1/ask", &token, json!({"message": "where", "agent": "codex", "wait": 10})).await;
    let (a, b) = (a["response"].as_str().unwrap().to_owned(), b["response"].as_str().unwrap().to_owned());
    assert_ne!(a, b, "agents must not share a working directory");
    assert!(a.ends_with("/claude") && b.ends_with("/codex"), "{a} {b}");
    h.running.shutdown().await;
}
