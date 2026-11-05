//! End-to-end tests for the Qwen Code backend: a real server on a random
//! port, driving a fake `qwen` executable that prints `stream-json` output.

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

/// Echoes how it was invoked: the prompt, the resumed session, the approval
/// mode and whether self-relaunch was disabled.
const FAKE_QWEN: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "0.26.7"; exit 0; fi
prompt=""
resumed=no
mode=none
prev=""
for a in "$@"; do
  case "$a" in --prompt=*) prompt="${a#--prompt=}" ;; esac
  if [ "$prev" = "--resume" ]; then resumed="$a"; fi
  if [ "$prev" = "--approval-mode" ]; then mode="$a"; fi
  prev="$a"
done
echo '{"type":"system","subtype":"init","session_id":"qwen-session-1","model":"fake"}'
case "$prompt" in
  fail*) echo 'stack trace noise' >&2; echo '{"type":"result","subtype":"error_during_execution","session_id":"qwen-session-1","is_error":true,"usage":{"input_tokens":0,"output_tokens":0},"error":{"message":"401 Incorrect API key provided"}}'; exit 1 ;;
esac
printf '{"type":"result","subtype":"success","session_id":"qwen-session-1","is_error":false,"result":"qwen says %s (resume=%s mode=%s norelaunch=%s)","usage":{"input_tokens":11,"output_tokens":6,"cache_read_input_tokens":4}}\n' "$prompt" "$resumed" "$mode" "${QWEN_CODE_NO_RELAUNCH:-unset}"
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

    let mut qwen = AgentConfig::default_for(AgentKind::Qwen);
    qwen.bin = write_script(&bin, "qwen", FAKE_QWEN);
    let agents = HashMap::from([(AgentKind::Qwen, qwen)]);

    let cfg = ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        home: dir.path().join("home"),
        agents,
        default_agent: AgentKind::Qwen,
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
                Some(json!({"message": message, "agent": "qwen", "wait": 10})),
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
async fn ask_returns_qwen_answer() {
    let h = harness().await;
    let token = h.user_token(json!({})).await;
    let v = h.ask(&token, "hello").await;
    assert_eq!(v["status"], "completed");
    assert_eq!(v["agent"], "qwen");
    assert_eq!(
        v["response"],
        "qwen says hello (resume=no mode=default norelaunch=1)"
    );
    assert_eq!(v["usage"]["inputTokens"], 11);
    assert_eq!(v["usage"]["outputTokens"], 6);
    h.running.shutdown().await;
}

#[tokio::test]
async fn flag_looking_prompt_reaches_qwen_as_text() {
    let h = harness().await;
    let token = h.user_token(json!({})).await;
    let v = h.ask(&token, "--yolo please").await;
    assert_eq!(
        v["response"],
        "qwen says --yolo please (resume=no mode=default norelaunch=1)"
    );
    h.running.shutdown().await;
}

#[tokio::test]
async fn qwen_reported_errors_win_over_stderr_noise() {
    let h = harness().await;
    let token = h.user_token(json!({})).await;
    let v = h.ask(&token, "fail now").await;
    assert_eq!(v["status"], "failed");
    assert_eq!(v["error"], "401 Incorrect API key provided");
    h.running.shutdown().await;
}

#[tokio::test]
async fn stateful_tokens_resume_their_qwen_session() {
    let h = harness().await;
    let token = h.user_token(json!({"sessionMode": "stateful"})).await;
    let first = h.ask(&token, "one").await;
    assert_eq!(
        first["response"],
        "qwen says one (resume=no mode=default norelaunch=1)"
    );
    let second = h.ask(&token, "two").await;
    assert_eq!(
        second["response"],
        "qwen says two (resume=qwen-session-1 mode=default norelaunch=1)"
    );

    let (status, _) = h
        .call(reqwest::Method::DELETE, "/v1/session/qwen", &token, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let third = h.ask(&token, "three").await;
    assert_eq!(
        third["response"],
        "qwen says three (resume=no mode=default norelaunch=1)"
    );
    h.running.shutdown().await;
}
