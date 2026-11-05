//! End-to-end tests for the Gemini CLI backend: a real server on a random
//! port, driving a fake `gemini` executable that prints `stream-json` events.

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

/// Mimics `gemini -o stream-json`. Echoes the prompt, the resumed session and
/// the approval mode so tests can assert on how it was invoked. A prompt
/// starting with `blocked` ends with `status:"error"` but exit code 0, like a
/// safety-blocked response.
const FAKE_GEMINI: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "0.0.0-fake"; exit 0; fi
prompt=""
resume=no
mode=""
prev=""
for a in "$@"; do
  case "$a" in
    --prompt=*) prompt="${a#--prompt=}" ;;
    --resume=*) resume="${a#--resume=}" ;;
  esac
  if [ "$prev" = "--approval-mode" ]; then mode="$a"; fi
  prev="$a"
done
if [ "$GEMINI_CLI_NO_RELAUNCH" != "1" ]; then mode="relaunch-not-disabled"; fi
if [ "$resume" = "no" ]; then session="gemini-session-1"; else session="$resume"; fi
printf '{"type":"init","timestamp":"t","session_id":"%s","model":"gemini-2.5-pro"}\n' "$session"
case "$prompt" in
  blocked*)
    echo '{"type":"error","timestamp":"t","severity":"error","message":"The model response was blocked due to safety settings."}'
    echo '{"type":"result","timestamp":"t","status":"error","stats":{"input_tokens":1,"output_tokens":0}}'
    exit 0 ;;
esac
echo '{"type":"message","timestamp":"t","role":"assistant","content":"thinking","delta":true}'
echo '{"type":"tool_use","timestamp":"t","tool_name":"read_file","tool_id":"x","parameters":{}}'
echo '{"type":"tool_result","timestamp":"t","tool_id":"x","status":"success","output":""}'
printf '{"type":"message","timestamp":"t","role":"assistant","content":"gemini says %s","delta":true}\n' "$prompt"
printf '{"type":"message","timestamp":"t","role":"assistant","content":" (resume=%s, mode=%s)","delta":true}\n' "$resume" "$mode"
echo '{"type":"result","timestamp":"t","status":"success","stats":{"total_tokens":20,"input_tokens":12,"output_tokens":8}}'
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

    let mut agents = HashMap::new();
    for kind in AgentKind::ALL {
        let mut cfg = AgentConfig::default_for(kind);
        cfg.bin = if kind == AgentKind::Gemini {
            write_script(&bin, "gemini", FAKE_GEMINI)
        } else {
            PathBuf::from("/nonexistent")
        };
        agents.insert(kind, cfg);
    }

    let cfg = ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        home: dir.path().join("home"),
        agents,
        default_agent: AgentKind::Gemini,
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
        let mut body = json!({ "name": "test-user" });
        if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
            b.extend(e.clone());
        }
        let (status, v) = self.post("/v1/tokens", &self.admin, body).await;
        assert_eq!(status, StatusCode::CREATED, "{v}");
        v["token"].as_str().unwrap().to_owned()
    }

    async fn ask(&self, token: &str, message: &str) -> Value {
        let (status, v) = self
            .post(
                "/v1/ask",
                token,
                json!({"message": message, "agent": "gemini", "wait": 10}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{v}");
        v
    }
}

#[tokio::test]
async fn ask_gemini_returns_final_answer_and_usage() {
    let h = harness().await;
    let token = h.user_token(json!({})).await;
    let v = h.ask(&token, "hello").await;
    assert_eq!(v["status"], "completed", "{v}");
    assert_eq!(v["agent"], "gemini");
    assert_eq!(v["response"], "gemini says hello (resume=no, mode=default)");
    assert_eq!(v["usage"]["inputTokens"], 12);
    assert_eq!(v["usage"]["outputTokens"], 8);
    h.running.shutdown().await;
}

#[tokio::test]
async fn gemini_prompt_that_looks_like_a_flag_is_just_text() {
    let h = harness().await;
    let token = h.user_token(json!({})).await;
    let v = h.ask(&token, "--yolo").await;
    assert_eq!(
        v["response"],
        "gemini says --yolo (resume=no, mode=default)"
    );
    h.running.shutdown().await;
}

#[tokio::test]
async fn gemini_blocked_response_fails_despite_exit_zero() {
    let h = harness().await;
    let token = h.user_token(json!({})).await;
    let v = h.ask(&token, "blocked please").await;
    assert_eq!(v["status"], "failed", "{v}");
    assert_eq!(
        v["error"],
        "The model response was blocked due to safety settings."
    );
    h.running.shutdown().await;
}

#[tokio::test]
async fn stateful_gemini_token_resumes_its_session() {
    let h = harness().await;
    let token = h.user_token(json!({"sessionMode": "stateful"})).await;
    let first = h.ask(&token, "one").await;
    assert_eq!(
        first["response"],
        "gemini says one (resume=no, mode=default)"
    );
    let second = h.ask(&token, "two").await;
    assert_eq!(
        second["response"],
        "gemini says two (resume=gemini-session-1, mode=default)"
    );
    h.running.shutdown().await;
}
