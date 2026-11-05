//! MCP endpoint tests: a real server on a random port with fake agent
//! executables, spoken to over JSON-RPC at `POST /mcp`.

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

    /// Sends one JSON-RPC request and returns the HTTP status and body.
    async fn rpc(&self, token: &str, method: &str, params: Value) -> (StatusCode, Value) {
        self.post(
            "/mcp",
            token,
            json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}),
        )
        .await
    }

    /// Calls a tool and returns its `result` object.
    async fn tool(&self, token: &str, name: &str, arguments: Value) -> Value {
        let (status, v) = self
            .rpc(
                token,
                "tools/call",
                json!({"name": name, "arguments": arguments}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{v}");
        v["result"].clone()
    }
}

fn text(result: &Value) -> &str {
    result["content"][0]["text"].as_str().unwrap()
}

#[tokio::test]
async fn initialize_returns_capabilities() {
    let h = harness().await;
    let token = h.user_token(json!({})).await;
    let (status, v) = h
        .rpc(
            &token,
            "initialize",
            json!({"protocolVersion": "2025-03-26", "capabilities": {},
                   "clientInfo": {"name": "test", "version": "0"}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["jsonrpc"], "2.0");
    assert_eq!(v["id"], 1);
    let r = &v["result"];
    assert_eq!(r["protocolVersion"], "2025-03-26");
    assert!(r["capabilities"]["tools"].is_object());
    assert_eq!(r["serverInfo"]["name"], "hashi");
    assert_eq!(r["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(r["instructions"].is_string());

    let (_, v) = h
        .rpc(
            &token,
            "initialize",
            json!({"protocolVersion": "1999-01-01"}),
        )
        .await;
    assert_eq!(v["result"]["protocolVersion"], "2025-06-18");
    let (_, v) = h.rpc(&token, "ping", json!({})).await;
    assert_eq!(v["result"], json!({}));
    h.running.shutdown().await;
}

#[tokio::test]
async fn tools_list_respects_agent_scope() {
    let h = harness().await;
    let token = h.user_token(json!({"agents": ["codex"]})).await;
    let (status, v) = h.rpc(&token, "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let names: Vec<&str> = v["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"ask_codex"), "{names:?}");
    assert!(!names.contains(&"ask_claude"), "{names:?}");
    assert!(names.contains(&"get_result") && names.contains(&"my_usage"));

    let r = h.tool(&token, "ask_claude", json!({"prompt": "hi"})).await;
    assert_eq!(r["isError"], true, "{r}");
    let r = h.tool(&token, "ask_codex", json!({"prompt": "hi"})).await;
    assert_eq!(r["isError"], false, "{r}");
    assert!(text(&r).contains("codex says hi"), "{r}");
    h.running.shutdown().await;
}

#[tokio::test]
async fn ask_claude_returns_answer() {
    let h = harness().await;
    let token = h.user_token(json!({})).await;
    let r = h
        .tool(&token, "ask_claude", json!({"prompt": "hello"}))
        .await;
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["content"][0]["type"], "text");
    assert!(text(&r).contains("claude says hello"), "{r}");

    let r = h
        .tool(&token, "ask_claude", json!({"prompt": "fail now"}))
        .await;
    assert_eq!(r["isError"], true, "{r}");

    let r = h.tool(&token, "my_usage", json!({})).await;
    assert_eq!(r["isError"], false, "{r}");
    let usage: Value = serde_json::from_str(text(&r)).unwrap();
    assert_eq!(usage["usage"]["todayRequests"], 2, "{usage}");

    let (_, v) = h
        .rpc(
            &token,
            "tools/call",
            json!({"name": "ask_claude", "arguments": {}}),
        )
        .await;
    assert_eq!(v["error"]["code"], -32602, "{v}");
    h.running.shutdown().await;
}

#[tokio::test]
async fn paused_token_cannot_call_tools() {
    let h = harness().await;
    let token = h.user_token(json!({"name": "pausable"})).await;
    let id = h.token_id("pausable").await;
    let (status, v) = h
        .patch(&format!("/v1/tokens/{id}"), json!({"paused": true}))
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");

    let r = h.tool(&token, "ask_claude", json!({"prompt": "hi"})).await;
    assert_eq!(r["isError"], true, "{r}");
    assert!(text(&r).contains("paused"), "{r}");
    h.running.shutdown().await;
}

#[tokio::test]
async fn unknown_method_is_jsonrpc_error() {
    let h = harness().await;
    let token = h.user_token(json!({})).await;
    let (status, v) = h.rpc(&token, "resources/list", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["error"]["code"], -32601, "{v}");
    assert_eq!(v["id"], 1);

    let (_, v) = h.post("/mcp", &token, json!([{"jsonrpc": "2.0"}])).await;
    assert_eq!(v["error"]["code"], -32600, "{v}");

    let res = h
        .http
        .post(format!("{}/mcp", h.base))
        .bearer_auth(&token)
        .header("content-type", "application/json")
        .body("{not json")
        .send()
        .await
        .unwrap();
    let v: Value = res.json().await.unwrap();
    assert_eq!(v["error"]["code"], -32700, "{v}");

    let res = h
        .http
        .get(format!("{}/mcp", h.base))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(res.headers()["allow"], "POST");
    h.running.shutdown().await;
}

#[tokio::test]
async fn notification_gets_202() {
    let h = harness().await;
    let token = h.user_token(json!({})).await;
    let res = h
        .http
        .post(format!("{}/mcp", h.base))
        .bearer_auth(&token)
        .json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    assert!(res.text().await.unwrap().is_empty());
    h.running.shutdown().await;
}

#[tokio::test]
async fn get_result_hides_other_tokens_requests() {
    let h = harness().await;
    let alice = h.user_token(json!({"name": "alice"})).await;
    let bob = h.user_token(json!({"name": "bob"})).await;
    let (status, v) = h
        .post("/v1/ask", &alice, json!({"message": "secret", "wait": 10}))
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let id = v["id"].as_str().unwrap();

    let r = h.tool(&alice, "get_result", json!({"id": id})).await;
    assert_eq!(r["isError"], false, "{r}");
    assert!(text(&r).contains("claude says secret"), "{r}");

    let r = h.tool(&bob, "get_result", json!({"id": id})).await;
    assert_eq!(r["isError"], true, "{r}");
    assert!(text(&r).contains("not found"), "{r}");
    assert!(!text(&r).contains("secret"), "{r}");
    h.running.shutdown().await;
}

#[tokio::test]
async fn missing_token_is_401() {
    let h = harness().await;
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
    let (status, v) = h
        .call(reqwest::Method::POST, "/mcp", None, Some(body.clone()))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(v["error"]["code"], "unauthorized");
    let (status, _) = h.post("/mcp", "hashi_not-a-real-token", body).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    h.running.shutdown().await;
}
