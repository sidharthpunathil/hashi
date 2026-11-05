//! End-to-end tests: a real server on a random port, driving fake agent
//! executables that mimic the Claude Code and Codex output formats.

// Test helpers outside #[test] functions are not covered by clippy.toml's
// test exemptions; panicking on setup failure is the desired behaviour here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use hashi::agent::{AgentConfig, AgentKind, Limits};
use hashi::events::Bus;
use hashi::guard::{GIB, ManualProbe, Probe, ResourceSettings, Size};
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
        usage: Default::default(),
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
    let (_, a) = h
        .post(
            "/v1/ask",
            &token,
            json!({"message": "where", "agent": "claude", "wait": 10}),
        )
        .await;
    let (_, b) = h
        .post(
            "/v1/ask",
            &token,
            json!({"message": "where", "agent": "codex", "wait": 10}),
        )
        .await;
    let (a, b) = (
        a["response"].as_str().unwrap().to_owned(),
        b["response"].as_str().unwrap().to_owned(),
    );
    assert_ne!(a, b, "agents must not share a working directory");
    assert!(a.ends_with("/claude") && b.ends_with("/codex"), "{a} {b}");
    h.running.shutdown().await;
}

// ── resource guard ─────────────────────────────────────────────────────────

fn guarded(
    probe: &Arc<ManualProbe>,
    tweak: impl FnOnce(&mut ResourceSettings),
) -> ResourceSettings {
    let p: Arc<dyn Probe> = probe.clone();
    let mut s = ResourceSettings {
        retry_after: Some(Duration::from_millis(100)),
        sample_every: Some(Duration::from_millis(100)),
        ..ResourceSettings::with_probe(p)
    };
    tweak(&mut s);
    s
}

#[tokio::test]
async fn low_memory_holds_work_in_the_queue_until_it_frees() {
    // A 16 GiB machine keeps 3.2 GiB free; only 1 GiB is available.
    let probe = Arc::new(ManualProbe::new(16 * GIB, GIB));
    let settings = guarded(&probe, |_| {});
    let h = harness_with(|c| c.resources = settings).await;
    let token = h.user_token(json!({})).await;
    let (status, v) = h.post("/v1/ask", &token, json!({"message": "held"})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{v}");
    let poll = v["poll"].as_str().unwrap().to_owned();

    tokio::time::sleep(Duration::from_millis(600)).await;
    let (_, v) = h.get(&poll, Some(&token)).await;
    assert_eq!(v["status"], "queued", "{v}");
    assert_eq!(v["position"], 1, "{v}");
    let (_, stats) = h.get("/v1/stats", Some(&h.admin)).await;
    assert_eq!(stats["resources"]["held"], "memory", "{stats}");
    assert_eq!(stats["resources"]["running"], 0, "{stats}");

    probe.set_available(10 * GIB);
    let (status, v) = h.get(&format!("{poll}?wait=10"), Some(&token)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "completed", "{v}");
    let (_, stats) = h.get("/v1/stats", Some(&h.admin)).await;
    assert!(stats["resources"]["held"].is_null(), "{stats}");
    h.running.shutdown().await;
}

#[tokio::test]
async fn low_disk_holds_work_too() {
    let probe = Arc::new(ManualProbe::roomy());
    probe.set_disk_free(100 * 1024 * 1024);
    let settings = guarded(&probe, |_| {});
    let h = harness_with(|c| c.resources = settings).await;
    let token = h.user_token(json!({})).await;
    let (_, v) = h.post("/v1/ask", &token, json!({"message": "held"})).await;
    let poll = v["poll"].as_str().unwrap().to_owned();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (_, v) = h.get(&poll, Some(&token)).await;
    assert_eq!(v["status"], "queued", "{v}");
    let (_, stats) = h.get("/v1/stats", Some(&h.admin)).await;
    assert_eq!(stats["resources"]["held"], "disk", "{stats}");
    probe.set_disk_free(100 * GIB);
    let (_, v) = h.get(&format!("{poll}?wait=10"), Some(&token)).await;
    assert_eq!(v["status"], "completed", "{v}");
    h.running.shutdown().await;
}

/// Runs a slow Claude prompt and a quick Codex prompt together and reports
/// whether their run intervals overlapped.
async fn claude_and_codex_overlap(max_running: usize) -> bool {
    let probe = Arc::new(ManualProbe::roomy());
    let settings = guarded(&probe, |s| s.max_running = Some(max_running));
    let h = harness_with(|c| c.resources = settings).await;
    let token = h.user_token(json!({})).await;
    let slow = h.post(
        "/v1/ask",
        &token,
        json!({"message": "slow one", "wait": 15}),
    );
    let quick = h.post(
        "/v1/ask",
        &token,
        json!({"message": "quick", "agent": "codex", "wait": 15}),
    );
    let ((_, a), (_, b)) = tokio::join!(slow, quick);
    assert_eq!(
        (a["status"].as_str(), b["status"].as_str()),
        (Some("completed"), Some("completed"))
    );
    let store = &h.running.state.store;
    let a = store
        .get_request(a["id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    let b = store
        .get_request(b["id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    let span = |r: &hashi::store::Request| (r.started_at.unwrap(), r.completed_at.unwrap());
    let ((a0, a1), (b0, b1)) = (span(&a), span(&b));
    h.running.shutdown().await;
    a0 < b1 && b0 < a1
}

#[tokio::test]
async fn max_running_one_serialises_agents() {
    assert!(
        !claude_and_codex_overlap(1).await,
        "runs overlapped with max_running = 1"
    );
}

#[tokio::test]
async fn max_running_two_lets_agents_overlap() {
    assert!(
        claude_and_codex_overlap(2).await,
        "runs did not overlap with max_running = 2"
    );
}

#[tokio::test]
async fn watchdog_stops_a_run_over_its_memory_limit() {
    let probe = Arc::new(ManualProbe::roomy());
    probe.set_tree_memory(Some(5 * GIB));
    let settings = guarded(&probe, |s| {
        s.run_memory_limit = Some(Size::Bytes(2 * GIB));
    });
    let h = harness_with(|c| c.resources = settings).await;
    let token = h.user_token(json!({})).await;
    let started = std::time::Instant::now();
    let (_, v) = h
        .post(
            "/v1/ask",
            &token,
            json!({"message": "slow one", "wait": 10}),
        )
        .await;
    assert_eq!(v["status"], "failed", "{v}");
    assert_eq!(
        v["error"], "run exceeded its memory limit (5.0 GiB of 2.0 GiB)",
        "{v}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the run was not stopped early"
    );
    h.running.shutdown().await;
}

#[tokio::test]
async fn timeout_kills_the_whole_process_tree() {
    let mut pidfile = PathBuf::new();
    let h = harness_with(|c| {
        let dir = c.home.parent().unwrap().join("orphan");
        std::fs::create_dir_all(&dir).unwrap();
        pidfile = dir.join("grandchild.pid");
        let script = format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"--version\" ]; then echo '9.9.9 (Fake Claude)'; exit 0; fi\n\
             sleep 600 >/dev/null 2>&1 &\n\
             echo $! > '{}'\n\
             sleep 600\n",
            pidfile.display()
        );
        if let Some(claude) = c.agents.get_mut(&AgentKind::Claude) {
            claude.bin = write_script(&dir, "claude", &script);
        }
        c.limits.timeout = Duration::from_secs(1);
    })
    .await;
    let token = h.user_token(json!({})).await;
    let (_, v) = h
        .post("/v1/ask", &token, json!({"message": "spawn", "wait": 10}))
        .await;
    assert_eq!(v["status"], "failed", "{v}");
    assert!(v["error"].as_str().unwrap().contains("timed out"), "{v}");
    let pid = std::fs::read_to_string(&pidfile).unwrap();
    let pid = pid.trim();
    let mut gone = false;
    for _ in 0..50 {
        let alive = std::process::Command::new("kill")
            .args(["-0", pid])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !alive {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if !gone {
        // Do not leave a stray process behind on the test machine.
        let _ = std::process::Command::new("kill")
            .args(["-9", pid])
            .status();
    }
    assert!(gone, "grandchild {pid} outlived its run");
    h.running.shutdown().await;
}

#[tokio::test]
async fn machine_figures_are_for_admins_only() {
    let h = harness().await;
    let (status, v) = h.get("/v1/stats", Some(&h.admin)).await;
    assert_eq!(status, StatusCode::OK);
    let r = &v["resources"];
    for key in [
        "memoryTotal",
        "memoryAvailable",
        "reserve",
        "running",
        "maxRunning",
        "held",
        "diskFree",
    ] {
        assert!(r.get(key).is_some(), "resources.{key} missing: {v}");
    }
    assert_eq!(r["memoryTotal"], 64 * GIB);
    // The fake Claude reports 9.9.9, newer than any verified release; the
    // fake Codex reports 0.0.0, which is older and says nothing.
    let agent = |name: &str| {
        v["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["agent"] == name)
            .cloned()
            .unwrap()
    };
    let claude = agent("claude");
    assert_eq!(claude["verified"], "2.1.318");
    if claude["version"].is_string() {
        assert_eq!(claude["newerThanVerified"], true, "{claude}");
    }
    assert_eq!(agent("codex")["newerThanVerified"], false);

    let token = h.user_token(json!({})).await;
    assert_eq!(
        h.get("/v1/stats", Some(&token)).await.0,
        StatusCode::FORBIDDEN
    );
    let (_, status) = h.get("/v1/status", None).await;
    let (_, me) = h.get("/v1/me", Some(&token)).await;
    for body in [status, me] {
        let text = body.to_string();
        assert!(body.get("resources").is_none(), "{text}");
        for needle in ["memory", "Memory", "diskFree", "reserve"] {
            assert!(!text.contains(needle), "`{needle}` leaked: {text}");
        }
    }
    h.running.shutdown().await;
}

// ── storage and queue exhaustion ───────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_burst_cannot_exceed_max_pending() {
    let h = harness().await;
    let token = h.user_token(json!({"limits": {"maxPending": 3}})).await;
    let posts = (0..50).map(|i| {
        let token = token.clone();
        let h = &h;
        async move {
            h.post("/v1/ask", &token, json!({"message": format!("slow {i}")}))
                .await
        }
    });
    let results = futures::future::join_all(posts).await;
    let accepted = results
        .iter()
        .filter(|(s, _)| *s == StatusCode::ACCEPTED)
        .count();
    let busy = results
        .iter()
        .filter(|(s, v)| {
            *s == StatusCode::TOO_MANY_REQUESTS && v["error"]["code"] == "too_many_pending"
        })
        .count();
    assert_eq!((accepted, busy), (3, 47), "{results:?}");
    h.running.shutdown().await;
}

#[tokio::test]
async fn new_user_tokens_default_to_three_pending_and_admins_to_unlimited() {
    let h = harness().await;
    let user = h.user_token(json!({})).await;
    let (_, me) = h.get("/v1/me", Some(&user)).await;
    assert_eq!(me["limits"]["maxPending"], 3, "{me}");
    let (status, v) = h
        .post(
            "/v1/tokens",
            &h.admin,
            json!({"name": "ops", "role": "admin"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(v["record"]["limits"]["maxPending"].is_null(), "{v}");
    for i in 0..3 {
        let (status, _) = h
            .post("/v1/ask", &user, json!({"message": format!("slow {i}")}))
            .await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }
    let (status, v) = h.post("/v1/ask", &user, json!({"message": "slow"})).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(v["error"]["code"], "too_many_pending");
    h.running.shutdown().await;
}

#[tokio::test]
async fn users_cannot_fill_the_slots_reserved_for_admins() {
    // A queue of 6 keeps 3 slots for admins.
    let h = harness_with(|c| c.queue_limit = 6).await;
    let user = h.user_token(json!({"limits": {"maxPending": 50}})).await;
    for i in 0..3 {
        let (status, _) = h
            .post("/v1/ask", &user, json!({"message": format!("slow {i}")}))
            .await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }
    let (status, _) = h.post("/v1/ask", &user, json!({"message": "slow"})).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    let (status, _) = h
        .post("/v1/ask", &h.admin, json!({"message": "slow admin"}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    h.running.shutdown().await;
}

#[tokio::test]
async fn a_token_holds_at_most_four_long_polls() {
    let h = harness().await;
    let user = h.user_token(json!({})).await;
    let (status, v) = h.post("/v1/ask", &user, json!({"message": "slow"})).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let id = v["id"].as_str().unwrap().to_owned();
    let path = format!("/v1/ask/{id}?wait=10");
    let held = futures::future::join_all((0..4).map(|_| h.get(&path, Some(&user))));
    let fifth = async {
        // Let the four waits register first.
        tokio::time::sleep(Duration::from_millis(400)).await;
        let started = std::time::Instant::now();
        let (status, v) = h.get(&path, Some(&user)).await;
        (status, v, started.elapsed())
    };
    let (held, (status, v, took)) = tokio::join!(held, fifth);
    assert_eq!(status, StatusCode::ACCEPTED, "{v}");
    assert!(
        took < Duration::from_secs(1),
        "fifth wait was held {took:?}"
    );
    assert_ne!(v["status"], "completed");
    for (status, v) in held {
        assert_eq!(status, StatusCode::OK, "{v}");
    }
    h.running.shutdown().await;
}

async fn user_id(h: &Harness) -> String {
    let (_, v) = h.get("/v1/tokens", Some(&h.admin)).await;
    v["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["role"] == "user")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn revoking_a_token_deletes_its_workspace() {
    let h = harness().await;
    let user = h.user_token(json!({})).await;
    let (status, _) = h
        .post("/v1/ask", &user, json!({"message": "hi", "wait": 10}))
        .await;
    assert_eq!(status, StatusCode::OK);
    let id = user_id(&h).await;
    let workspace = h._dir.path().join("home/workspaces").join(&id);
    assert!(workspace.join("claude").is_dir());
    let (status, _) = h
        .call(
            reqwest::Method::DELETE,
            &format!("/v1/tokens/{id}"),
            Some(&h.admin),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!workspace.exists());
    h.running.shutdown().await;
}

#[test]
fn remove_workspace_never_leaves_the_workspace_root() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(home.join("workspaces/tok-1/claude")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("keep.txt"), "precious").unwrap();
    // A link inside a workspace, and a workspace that is itself a link.
    std::os::unix::fs::symlink(&outside, home.join("workspaces/tok-1/claude/escape")).unwrap();
    std::os::unix::fs::symlink(&outside, home.join("workspaces/tok-2")).unwrap();

    assert!(server::remove_workspace(&home, "tok-1").unwrap());
    assert!(server::remove_workspace(&home, "tok-2").unwrap());
    assert!(!home.join("workspaces/tok-1").exists());
    assert!(home.join("workspaces/tok-2").symlink_metadata().is_err());
    assert_eq!(
        std::fs::read_to_string(outside.join("keep.txt")).unwrap(),
        "precious"
    );

    // Missing is fine; anything that is not a plain id is refused.
    assert!(!server::remove_workspace(&home, "tok-3").unwrap());
    assert!(server::remove_workspace(&home, "../outside").is_err());
    assert!(server::remove_workspace(&home, "").is_err());
    assert!(outside.join("keep.txt").exists());
}

// ── token lifecycle: pause, expiry and revoke reach queued and running work ──

/// Polls `GET /v1/ask/{id}` as admin until `want` or a timeout.
async fn wait_for_status(h: &Harness, id: &str, want: &str) -> Value {
    for _ in 0..100 {
        let (_, v) = h.get(&format!("/v1/ask/{id}"), Some(&h.admin)).await;
        if v["status"] == want {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("request {id} never reached {want}");
}

#[tokio::test]
async fn paused_token_queued_prompt_waits_until_resume() {
    let h = harness().await;
    let token = h.user_token(json!({"name": "pausable"})).await;
    let tid = h.token_id("pausable").await;
    let (_, one) = h
        .post("/v1/ask", &token, json!({"message": "slow one"}))
        .await;
    let (_, two) = h
        .post("/v1/ask", &token, json!({"message": "slow two"}))
        .await;
    let one = one["id"].as_str().unwrap().to_owned();
    let two = two["id"].as_str().unwrap().to_owned();
    wait_for_status(&h, &one, "running").await;
    let (status, _) = h
        .patch(&format!("/v1/tokens/{tid}"), json!({"paused": true}))
        .await;
    assert_eq!(status, StatusCode::OK);
    // The first run finishes after 2 s; the second must not start while paused.
    wait_for_status(&h, &one, "completed").await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let (_, v) = h.get(&format!("/v1/ask/{two}"), Some(&h.admin)).await;
    assert_eq!(v["status"], "queued", "{v}");

    h.patch(&format!("/v1/tokens/{tid}"), json!({"paused": false}))
        .await;
    let (_, v) = h
        .get(&format!("/v1/ask/{two}?wait=10"), Some(&h.admin))
        .await;
    assert_eq!(v["status"], "completed", "{v}");
    h.running.shutdown().await;
}

#[tokio::test]
async fn expired_token_queued_prompt_fails_without_running() {
    let h = harness().await;
    let token = h
        .user_token(json!({"name": "brief", "expiresInDays": 1}))
        .await;
    let tid = h.token_id("brief").await;
    let (_, one) = h
        .post("/v1/ask", &token, json!({"message": "slow one"}))
        .await;
    let (_, two) = h
        .post("/v1/ask", &token, json!({"message": "slow two"}))
        .await;
    let one = one["id"].as_str().unwrap().to_owned();
    let two = two["id"].as_str().unwrap().to_owned();
    wait_for_status(&h, &one, "running").await;
    let mut t = h.running.state.store.get_token(&tid).unwrap().unwrap();
    t.expires_at = Some(1);
    h.running.state.store.save_token_policy(&t).unwrap();
    let v = wait_for_status(&h, &two, "failed").await;
    assert!(
        v["error"].as_str().unwrap().contains("expired"),
        "unexpected error: {v}"
    );
    assert!(v.get("response").is_none(), "{v}");
    h.running.shutdown().await;
}

/// A fake Claude that records its pid, then beats every 100 ms for a minute.
fn heartbeat_agent(dir: &Path) -> (PathBuf, PathBuf) {
    let pidfile = dir.join("agent.pid");
    let beats = dir.join("beats");
    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"--version\" ]; then echo '9.9.9 (Fake Claude)'; exit 0; fi\n\
         echo $$ > '{pid}'\n\
         i=0\n\
         while [ $i -lt 600 ]; do echo $i >> '{beats}'; i=$((i+1)); sleep 0.1; done\n",
        pid = pidfile.display(),
        beats = beats.display()
    );
    write_script(dir, "claude", &script);
    (pidfile, beats)
}

fn alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

async fn read_pid(pidfile: &Path) -> String {
    for _ in 0..100 {
        if let Ok(p) = std::fs::read_to_string(pidfile)
            && !p.trim().is_empty()
        {
            return p.trim().to_owned();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the agent never started");
}

async fn heartbeat_harness() -> (Harness, PathBuf, PathBuf) {
    let mut files = (PathBuf::new(), PathBuf::new());
    let h = harness_with(|c| {
        let dir = c.home.parent().unwrap().join("beat");
        std::fs::create_dir_all(&dir).unwrap();
        files = heartbeat_agent(&dir);
        if let Some(claude) = c.agents.get_mut(&AgentKind::Claude) {
            claude.bin = dir.join("claude");
        }
        c.limits.timeout = Duration::from_secs(60);
    })
    .await;
    (h, files.0, files.1)
}

fn kill_stray(pid: &str) {
    let _ = std::process::Command::new("kill")
        .args(["-9", pid])
        .status();
}

#[tokio::test]
async fn revoking_a_token_kills_its_running_agent() {
    let (h, pidfile, beats) = heartbeat_harness().await;
    let user = h.user_token(json!({})).await;
    let (status, _) = h.post("/v1/ask", &user, json!({"message": "job"})).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let pid = read_pid(&pidfile).await;
    let id = user_id(&h).await;
    let (status, _) = h
        .call(
            reqwest::Method::DELETE,
            &format!("/v1/tokens/{id}"),
            Some(&h.admin),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    // Revoke waits for the kill, so the agent is gone almost at once.
    let mut gone = false;
    for _ in 0..10 {
        if !alive(&pid) {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if !gone {
        kill_stray(&pid);
    }
    assert!(gone, "agent {pid} kept running after revoke");
    let count = std::fs::read_to_string(&beats).unwrap().lines().count();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let after = std::fs::read_to_string(&beats).unwrap().lines().count();
    assert_eq!(count, after, "heartbeats continued after revoke");
    let workspace = h._dir.path().join("home/workspaces").join(&id);
    assert!(!workspace.exists(), "the workspace survived the revoke");
    h.running.shutdown().await;
}

#[tokio::test]
async fn revoke_from_another_process_stops_the_run_within_seconds() {
    let (h, pidfile, _beats) = heartbeat_harness().await;
    let user = h.user_token(json!({})).await;
    let (status, _) = h.post("/v1/ask", &user, json!({"message": "job"})).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let pid = read_pid(&pidfile).await;
    let id = user_id(&h).await;
    // A second connection to the same database, as `hashi token revoke` uses.
    let other = hashi::store::Store::open(&h._dir.path().join("home/hashi.db")).unwrap();
    assert!(matches!(
        other.revoke_token(&id).unwrap(),
        hashi::store::Revoke::Revoked(_)
    ));
    let mut gone = false;
    for _ in 0..80 {
        if !alive(&pid) {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if !gone {
        kill_stray(&pid);
    }
    assert!(gone, "agent {pid} outlived an offline revoke");
    h.running.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_submits_never_exceed_request_caps() {
    let h = harness().await;
    for (round, limits) in [
        json!({"dailyRequests": 3}),
        json!({"monthlyRequests": 3}),
        json!({"dailyRequests": 3, "maxPending": 3}),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("burst-{round}");
        h.user_token(json!({"name": name, "limits": limits})).await;
        let tid = h.token_id(&name).await;
        let token = h.running.state.store.get_token(&tid).unwrap().unwrap();
        let barrier = Arc::new(tokio::sync::Barrier::new(32));
        let mut set = tokio::task::JoinSet::new();
        for i in 0..32 {
            let state = h.running.state.clone();
            let token = token.clone();
            let barrier = Arc::clone(&barrier);
            set.spawn(async move {
                barrier.wait().await;
                hashi::api::submit(
                    &state,
                    &token,
                    hashi::api::SubmitInput {
                        message: format!("hi {i}"),
                        agent: None,
                        model: None,
                    },
                )
                .await
            });
        }
        let mut accepted = 0;
        let mut limited = 0;
        while let Some(r) = set.join_next().await {
            match r.unwrap() {
                Ok(_) => accepted += 1,
                Err(e) if format!("{e:?}").contains("limit_reached") => limited += 1,
                Err(_) => {}
            }
        }
        let usage = h
            .running
            .state
            .store
            .token_usage(&tid, hashi::util::now_ms())
            .unwrap();
        assert_eq!(accepted, 3, "round {round}");
        assert_eq!(limited, 29, "round {round}");
        assert_eq!(usage.today_requests, 3, "round {round}");
    }
    h.running.shutdown().await;
}

#[tokio::test]
async fn cap_errors_keep_the_limit_name_and_reset_time() {
    let h = harness().await;
    let token = h
        .user_token(json!({"limits": {"monthlyRequests": 1}}))
        .await;
    let (status, _) = h
        .post("/v1/ask", &token, json!({"message": "hi", "wait": 10}))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, v) = h.post("/v1/ask", &token, json!({"message": "hi"})).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(v["error"]["limit"], "monthlyRequests", "{v}");
    assert!(
        v["error"]["resetAt"]
            .as_str()
            .unwrap()
            .ends_with("-01T00:00:00Z"),
        "{v}"
    );
    h.running.shutdown().await;
}

#[tokio::test]
async fn patch_limits_keeps_max_pending_unless_set() {
    let h = harness().await;
    h.user_token(json!({"name": "ci"})).await;
    let id = h.token_id("ci").await;
    let (status, v) = h
        .patch(
            &format!("/v1/tokens/{id}"),
            json!({"limits": {"dailyRequests": 5}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["record"]["limits"]["dailyRequests"], 5, "{v}");
    assert_eq!(v["record"]["limits"]["maxPending"], 3, "{v}");
    let (_, v) = h
        .patch(
            &format!("/v1/tokens/{id}"),
            json!({"limits": {"maxPending": 7}}),
        )
        .await;
    assert_eq!(v["record"]["limits"]["maxPending"], 7, "{v}");
    assert!(v["record"]["limits"]["dailyRequests"].is_null(), "{v}");
    let (_, v) = h
        .patch(
            &format!("/v1/tokens/{id}"),
            json!({"limits": {"maxPending": null}}),
        )
        .await;
    assert!(v["record"]["limits"]["maxPending"].is_null(), "{v}");
    h.running.shutdown().await;
}

#[tokio::test]
async fn paused_admin_cannot_read_other_tokens_requests() {
    let h = harness().await;
    let user = h.user_token(json!({})).await;
    let (status, v) = h
        .post(
            "/v1/tokens",
            &h.admin,
            json!({"name": "ops", "role": "admin"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let ops = v["token"].as_str().unwrap().to_owned();
    let ops_id = v["record"]["id"].as_str().unwrap().to_owned();
    let (_, v) = h
        .post("/v1/ask", &user, json!({"message": "hi", "wait": 10}))
        .await;
    let rid = v["id"].as_str().unwrap().to_owned();
    assert_eq!(
        h.get(&format!("/v1/ask/{rid}"), Some(&ops)).await.0,
        StatusCode::OK
    );
    h.patch(&format!("/v1/tokens/{ops_id}"), json!({"paused": true}))
        .await;
    assert_eq!(
        h.get(&format!("/v1/ask/{rid}"), Some(&ops)).await.0,
        StatusCode::NOT_FOUND
    );
    h.running.shutdown().await;
}

#[tokio::test]
async fn invites_cannot_exceed_the_token_cap() {
    let h = harness().await;
    let (code, _) = new_invite(&h, json!({"label": "late"})).await;
    let have = h.running.state.store.list_tokens().unwrap().len();
    for i in have..hashi::api::MAX_TOKENS {
        h.running
            .state
            .store
            .create_token(&hashi::store::NewToken {
                name: format!("filler-{i}"),
                role: hashi::token::Role::User,
                session_mode: hashi::token::SessionMode::Stateless,
                agents: vec![AgentKind::Claude],
                expires_in_days: None,
                note: None,
                limits: hashi::token::Limits::default(),
                accounts: Default::default(),
                project: None,
            })
            .unwrap();
    }
    let (status, v) = claim(&h, "redeem", &code).await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert_eq!(
        h.running.state.store.list_tokens().unwrap().len(),
        hashi::api::MAX_TOKENS
    );
    h.running.shutdown().await;
}

// ── several accounts per agent ─────────────────────────────────────────────

/// A fake Claude Code that knows which account it runs on: the name file in
/// `CLAUDE_CONFIG_DIR`, or `default`. A `limited` file there makes it print
/// a session limit message (its contents, when there are any).
const FAKE_ACCOUNT_CLAUDE: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "9.9.9 (Fake Claude)"; exit 0; fi
for last; do :; done
acct=default
if [ -n "$CLAUDE_CONFIG_DIR" ] && [ -f "$CLAUDE_CONFIG_DIR/name" ]; then acct=$(cat "$CLAUDE_CONFIG_DIR/name"); fi
if [ "$acct" != default ] && [ -f "$CLAUDE_CONFIG_DIR/limited" ]; then
  msg=$(cat "$CLAUDE_CONFIG_DIR/limited")
  [ -n "$msg" ] || msg="Claude AI usage limit reached: you have hit your session limit"
  printf '{"type":"result","is_error":true,"subtype":"error_during_execution","result":"%s"}\n' "$msg"
  exit 1
fi
resumed=no
prev=""
for a in "$@"; do
  if [ "$prev" = "--resume" ]; then resumed="$a"; fi
  prev="$a"
done
printf '{"type":"result","is_error":false,"result":"%s on %s (resume=%s)","session_id":"sess-%s","usage":{"input_tokens":1,"output_tokens":1}}\n' "$last" "$acct" "$resumed" "$acct"
"#;

async fn accounts_harness() -> Harness {
    harness_with(|cfg| {
        let bin = cfg.home.parent().unwrap().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        if let Some(claude) = cfg.agents.get_mut(&AgentKind::Claude) {
            claude.bin = write_script(&bin, "claude-accounts", FAKE_ACCOUNT_CLAUDE);
        }
    })
    .await
}

impl Harness {
    fn home(&self) -> PathBuf {
        self._dir.path().join("home")
    }

    /// Adds a Claude login account the fake agent can recognise.
    fn add_login(&self, name: &str) -> PathBuf {
        let dir = hashi::accounts::create_dir(&self.home(), AgentKind::Claude, name).unwrap();
        std::fs::write(dir.join("name"), name).unwrap();
        self.running
            .state
            .store
            .add_account(AgentKind::Claude, name, hashi::accounts::AccountKind::Login)
            .unwrap();
        dir
    }

    async fn ask(&self, token: &str, message: &str) -> Value {
        let (status, v) = self
            .post("/v1/ask", token, json!({"message": message, "wait": 10}))
            .await;
        assert!(status.is_success(), "{v}");
        v
    }

    async fn account(&self, name: &str) -> Value {
        let (status, v) = self.get("/v1/accounts", Some(&self.admin)).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        v["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["name"] == name)
            .cloned()
            .unwrap_or(Value::Null)
    }
}

#[tokio::test]
async fn a_limited_account_fails_over_to_the_next_and_only_admins_see_which() {
    let h = accounts_harness().await;
    let work = h.add_login("work");
    h.add_login("personal");
    std::fs::write(work.join("limited"), "").unwrap();
    let token = h
        .user_token(json!({"accounts": {"claude": ["work", "personal"]}}))
        .await;

    let v = h.ask(&token, "hi").await;
    assert_eq!(v["status"], "completed", "{v}");
    assert_eq!(v["response"], "hi on personal (resume=no)");
    assert!(
        v.get("account").is_none(),
        "holders never see accounts: {v}"
    );
    let id = v["id"].as_str().unwrap().to_owned();
    let (_, own) = h.get(&format!("/v1/ask/{id}"), Some(&token)).await;
    assert!(own.get("account").is_none(), "{own}");
    let (_, admin) = h.get(&format!("/v1/ask/{id}"), Some(&h.admin)).await;
    assert_eq!(admin["account"], "personal");

    // Not an interruption: the request remembers the account it left.
    let r = h.running.state.store.get_request(&id).unwrap().unwrap();
    assert_eq!(r.accounts_tried, ["work"]);
    assert_eq!(r.account.as_deref(), Some("personal"));

    // Marked limited for the default cooldown, since the message names no time.
    let w = h.account("work").await;
    assert_eq!(w["state"], "limited", "{w}");
    let until = time::OffsetDateTime::parse(
        w["limitedUntil"].as_str().unwrap(),
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    let left = (until - time::OffsetDateTime::now_utc()).whole_seconds();
    assert!((3_500..=3_600).contains(&left), "{left}");
    assert_eq!(h.account("personal").await["runsToday"], 1);

    // The next prompt skips the limited account without running it.
    let v = h.ask(&token, "again").await;
    assert_eq!(v["response"], "again on personal (resume=no)");
    let id = v["id"].as_str().unwrap();
    let r = h.running.state.store.get_request(id).unwrap().unwrap();
    assert!(r.accounts_tried.is_empty());

    // A token without a list keeps using the default account.
    let plain = h.user_token(json!({"name": "plain"})).await;
    assert_eq!(
        h.ask(&plain, "x").await["response"],
        "x on default (resume=no)"
    );
    h.running.shutdown().await;
}

#[tokio::test]
async fn every_account_at_its_limit_fails_with_the_earliest_reset() {
    let h = accounts_harness().await;
    let work = h.add_login("work");
    let personal = h.add_login("personal");
    std::fs::write(work.join("limited"), "").unwrap();
    std::fs::write(
        personal.join("limited"),
        "you have hit your session limit, try again in 2 hours",
    )
    .unwrap();
    let token = h
        .user_token(json!({"accounts": {"claude": ["work", "personal"]}}))
        .await;
    let v = h.ask(&token, "hi").await;
    assert_eq!(v["status"], "failed", "{v}");
    let err = v["error"].as_str().unwrap();
    let prefix = "every account for Claude Code this token can use is at its limit until ";
    assert!(err.starts_with(prefix), "{err}");
    let until = time::OffsetDateTime::parse(
        &err[prefix.len()..],
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    // The earliest of the two: work's hour of cooldown, not personal's two hours.
    let left = (until - time::OffsetDateTime::now_utc()).whole_seconds();
    assert!((3_500..=3_600).contains(&left), "{left}");
    let p = h.account("personal").await;
    let until = time::OffsetDateTime::parse(
        p["limitedUntil"].as_str().unwrap(),
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    let left = (until - time::OffsetDateTime::now_utc()).whole_seconds();
    assert!(
        (7_100..=7_200).contains(&left),
        "the message's own reset: {left}"
    );
    assert!(!err.contains("work") && !err.contains("personal"));

    // While both stay limited, a new prompt fails without running at all.
    let v = h.ask(&token, "later").await;
    assert_eq!(v["status"], "failed", "{v}");
    assert!(v["error"].as_str().unwrap().starts_with(prefix));
    h.running.shutdown().await;
}

#[tokio::test]
async fn a_success_clears_the_limit_and_disabled_accounts_are_skipped() {
    let h = accounts_harness().await;
    h.add_login("work");
    h.add_login("personal");
    let store = &h.running.state.store;
    // An expired mark: the account is usable, and a success clears it.
    store
        .mark_account_limited(AgentKind::Claude, "work", hashi::util::now_ms() - 1_000)
        .unwrap();
    let token = h
        .user_token(json!({"accounts": {"claude": ["work", "personal"]}}))
        .await;
    assert_eq!(
        h.ask(&token, "a").await["response"],
        "a on work (resume=no)"
    );
    let w = store
        .get_account(AgentKind::Claude, "work")
        .unwrap()
        .unwrap();
    assert_eq!(w.limited_until, None);
    assert!(w.last_used.is_some());

    store
        .set_account_disabled(AgentKind::Claude, "work", true)
        .unwrap();
    assert_eq!(
        h.ask(&token, "b").await["response"],
        "b on personal (resume=no)"
    );
    assert_eq!(h.account("work").await["state"], "disabled");
    h.running.shutdown().await;
}

#[tokio::test]
async fn stateful_sessions_are_kept_per_account() {
    let h = accounts_harness().await;
    let work = h.add_login("work");
    h.add_login("personal");
    let token = h
        .user_token(json!({
            "sessionMode": "stateful",
            "accounts": {"claude": ["work", "personal"]},
        }))
        .await;
    assert_eq!(
        h.ask(&token, "1").await["response"],
        "1 on work (resume=no)"
    );
    assert_eq!(
        h.ask(&token, "2").await["response"],
        "2 on work (resume=sess-work)"
    );
    // Failing over starts the conversation fresh on the next account.
    std::fs::write(work.join("limited"), "").unwrap();
    assert_eq!(
        h.ask(&token, "3").await["response"],
        "3 on personal (resume=no)"
    );
    assert_eq!(
        h.ask(&token, "4").await["response"],
        "4 on personal (resume=sess-personal)"
    );
    h.running.shutdown().await;
}

#[tokio::test]
async fn account_bindings_are_checked_and_hidden_from_holders() {
    let h = accounts_harness().await;
    h.add_login("work");

    let (status, v) = h
        .post(
            "/v1/tokens",
            &h.admin,
            json!({"name": "x", "accounts": {"claude": ["ghost"]}}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("claude:ghost")
    );
    let (status, _) = h
        .post(
            "/v1/tokens",
            &h.admin,
            json!({"name": "x", "accounts": {"copilot": ["work"]}}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, v) = h
        .post(
            "/v1/tokens",
            &h.admin,
            json!({"name": "bot", "accounts": {"claude": ["work", "default"]}}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    assert_eq!(
        v["record"]["accounts"],
        json!({"claude": ["work", "default"]})
    );
    let token = v["token"].as_str().unwrap().to_owned();
    let id = h.token_id("bot").await;
    let (_, list) = h.get("/v1/tokens", Some(&h.admin)).await;
    let bot = list["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "bot")
        .unwrap()
        .clone();
    assert_eq!(bot["accounts"]["claude"][0], "work");

    // The holder's own views say nothing about accounts.
    let (status, me) = h.get("/v1/me", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        me.get("accounts").is_none() && !me.to_string().contains("work"),
        "{me}"
    );
    for method in ["tools/list", "tools/call"] {
        let params = if method == "tools/call" {
            json!({"name": "my_usage", "arguments": {}})
        } else {
            json!({})
        };
        let (status, v) = h
            .post(
                "/mcp",
                &token,
                json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert!(
            !v.to_string().contains("\"work\"") && !v.to_string().contains("accounts"),
            "{v}"
        );
    }
    let (status, _) = h.get("/v1/accounts", Some(&token)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // PATCH replaces the map, checking names; {} goes back to default.
    let (status, _) = h
        .patch(
            &format!("/v1/tokens/{id}"),
            json!({"accounts": {"claude": ["nope"]}}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, v) = h
        .patch(&format!("/v1/tokens/{id}"), json!({"accounts": {}}))
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["record"]["accounts"], json!({}));

    // Invites carry the map to the token they create, unseen by the guest.
    let (status, v) = h
        .post(
            "/v1/invites",
            &h.admin,
            json!({"label": "carol", "accounts": {"claude": ["ghost"]}}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    let (code, v) = new_invite(
        &h,
        json!({"label": "carol", "accounts": {"claude": ["work"]}}),
    )
    .await;
    assert_eq!(v["invite"]["accounts"], json!({"claude": ["work"]}));
    let (status, preview) = claim(&h, "preview", &code).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        preview.get("accounts").is_none() && !preview.to_string().contains("work"),
        "{preview}"
    );
    let (status, redeemed) = claim(&h, "redeem", &code).await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(!redeemed.to_string().contains("\"work\""), "{redeemed}");
    let carol = h.token_id("carol").await;
    let t = h.running.state.store.get_token(&carol).unwrap().unwrap();
    assert_eq!(t.accounts[&AgentKind::Claude], ["work"]);
    h.running.shutdown().await;
}

#[tokio::test]
async fn accounts_endpoint_lists_state_and_never_keys() {
    let h = accounts_harness().await;
    h.add_login("work");
    hashi::accounts::write_key(&h.home(), AgentKind::Codex, "ci", "sk-secret-key-123").unwrap();
    h.running
        .state
        .store
        .add_account(AgentKind::Codex, "ci", hashi::accounts::AccountKind::ApiKey)
        .unwrap();
    h.user_token(json!({"accounts": {"claude": ["work"], "codex": ["ci"]}}))
        .await;
    let (status, v) = h.get("/v1/accounts", Some(&h.admin)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(!v.to_string().contains("sk-secret-key-123"));
    assert_eq!(v["perAgent"]["claude"], 1);
    assert_eq!(v["perAgent"]["codex"], 1);
    assert_eq!(v["perAgent"]["gemini"], 0);
    let ci = h.account("ci").await;
    assert_eq!(
        (
            ci["kind"].as_str(),
            ci["state"].as_str(),
            ci["tokens"].as_u64()
        ),
        (Some("api-key"), Some("ready"), Some(1))
    );
    let (status, _) = h.get("/v1/accounts", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    h.running.shutdown().await;
}
