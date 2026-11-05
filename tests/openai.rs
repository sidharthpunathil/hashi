//! OpenAI-compatible endpoint tests: a real server on a random port with
//! fake agent executables, driven at `/v1/chat/completions` and `/v1/models`.

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

/// Echoes the prompt and reports usage, like Claude Code's JSON output.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "9.9.9 (Fake Claude)"; exit 0; fi
for last; do :; done
case "$last" in
  fail*) echo '{"type":"result","is_error":true,"subtype":"error_during_execution","result":"simulated failure"}'; exit 1 ;;
  slow*) sleep 2 ;;
esac
printf '{"type":"result","is_error":false,"result":"claude says %s","session_id":"s1","total_cost_usd":0.01,"usage":{"input_tokens":5,"output_tokens":7}}\n' "$last"
"#;

const FAKE_CODEX: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "codex-cli 0.0.0-fake"; exit 0; fi
for last; do :; done
echo '{"type":"thread.started","thread_id":"codex-thread-1"}'
printf '{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"codex says %s"}}\n' "$last"
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
    // Configured but not installed: never listed.
    let mut gemini = AgentConfig::default_for(AgentKind::Gemini);
    gemini.bin = bin.join("no-such-gemini");
    agents.insert(AgentKind::Gemini, gemini);

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
    };
    tweak(&mut cfg);

    let running = server::start(cfg, Bus::default()).await.unwrap();
    let base = running.state.runtime.local_url();
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

    async fn chat(&self, token: &str, body: Value) -> (StatusCode, Value) {
        self.post("/v1/chat/completions", token, body).await
    }

    /// Sends a streaming chat request and returns the status, the content
    /// type and the whole event stream once it closes.
    async fn chat_stream(&self, token: &str, body: Value) -> (StatusCode, String, String) {
        let res = self
            .http
            .post(format!("{}/v1/chat/completions", self.base))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = res.status();
        let kind = res
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        (status, kind, res.text().await.unwrap())
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

fn user_says(text: &str) -> Value {
    json!([{ "role": "user", "content": text }])
}

/// Asserts the OpenAI error shape and returns the error object.
fn openai_error(v: &Value, kind: &str, code: &str) -> Value {
    let e = &v["error"];
    assert!(e["message"].is_string(), "{v}");
    assert_eq!(e["type"], kind, "{v}");
    assert_eq!(e["code"], code, "{v}");
    e.clone()
}

#[tokio::test]
async fn chat_completion_returns_the_answer_and_usage() {
    let h = harness().await;
    let user = h.user_token(json!({})).await;
    let (status, v) = h
        .chat(
            &user,
            json!({ "model": "claude", "messages": user_says("hi"), "temperature": 0.2,
                    "tools": [], "max_tokens": 10 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["object"], "chat.completion");
    assert!(v["id"].as_str().unwrap().starts_with("chatcmpl-"), "{v}");
    assert!(v["created"].as_i64().unwrap() > 1_600_000_000, "{v}");
    assert_eq!(v["model"], "claude");
    let choice = &v["choices"][0];
    assert_eq!(choice["index"], 0);
    assert_eq!(choice["message"]["role"], "assistant");
    assert_eq!(choice["message"]["content"], "claude says hi");
    assert_eq!(choice["finish_reason"], "stop");
    assert_eq!(
        v["usage"],
        json!({ "prompt_tokens": 5, "completion_tokens": 7, "total_tokens": 12 })
    );

    // A system message and earlier turns become one prompt, ending with the
    // last user turn.
    let (status, v) = h
        .chat(
            &user,
            json!({ "model": "claude", "wait": 0, "messages": [
                { "role": "system", "content": "Be brief." },
                { "role": "user", "content": "first" },
                { "role": "assistant", "content": "ok" },
                { "role": "user", "content": [{ "type": "text", "text": "second" }] },
            ]}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{v}");
    let id = v["id"].as_str().unwrap();
    let sent = h.running.state.store.get_request(id).unwrap().unwrap();
    assert_eq!(
        sent.message,
        "Instructions:\nBe brief.\n\nConversation so far:\n\n\
         User: first\n\nAssistant: ok\n\nUser: second"
    );
    h.running.shutdown().await;
}

#[tokio::test]
async fn model_selects_the_agent_and_its_model() {
    let h = harness().await;
    let user = h.user_token(json!({})).await;
    let res = h
        .http
        .post(format!("{}/v1/chat/completions", h.base))
        .bearer_auth(&user)
        .json(&json!({ "model": "claude/opus", "messages": user_says("hi") }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let id = res.headers()["x-hashi-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let v: Value = res.json().await.unwrap();
    assert_eq!(v["model"], "claude/opus");
    let (_, r) = h.get(&format!("/v1/ask/{id}"), Some(&user)).await;
    assert_eq!(r["agent"], "claude", "{r}");
    assert_eq!(r["model"], "opus", "{r}");

    let (status, v) = h
        .chat(
            &user,
            json!({ "model": "codex", "messages": user_says("hi") }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["choices"][0]["message"]["content"], "codex says hi");

    let (status, v) = h
        .chat(
            &user,
            json!({ "model": "claude/op us;rm", "messages": user_says("hi") }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    openai_error(&v, "invalid_request_error", "bad_request");
    let (status, v) = h
        .chat(
            &user,
            json!({ "model": "gpt-4o", "messages": user_says("hi") }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    openai_error(&v, "invalid_request_error", "model_not_found");
    let (status, v) = h
        .chat(
            &user,
            json!({ "model": "claude/", "messages": user_says("hi") }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    h.running.shutdown().await;
}

#[tokio::test]
async fn non_text_content_is_refused() {
    let h = harness().await;
    let user = h.user_token(json!({})).await;
    let (status, v) = h
        .chat(
            &user,
            json!({ "model": "claude", "messages": [{ "role": "user", "content": [
                { "type": "text", "text": "what is this?" },
                { "type": "image_url", "image_url": { "url": "https://example.com/a.png" } },
            ]}]}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    openai_error(&v, "invalid_request_error", "unsupported_content");
    let (status, v) = h
        .chat(
            &user,
            json!({ "model": "claude", "messages": [{ "role": "tool", "content": "42" }]}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    openai_error(&v, "invalid_request_error", "unsupported_role");
    let (status, v) = h
        .chat(&user, json!({ "model": "claude", "messages": [] }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    openai_error(&v, "invalid_request_error", "bad_request");
    // Nothing was queued.
    let (_, me) = h.get("/v1/me", Some(&user)).await;
    assert_eq!(me["usage"]["todayRequests"], 0, "{me}");
    h.running.shutdown().await;
}

#[tokio::test]
async fn models_lists_allowed_installed_agents() {
    let h = harness().await;
    let user = h
        .user_token(json!({ "agents": ["claude", "gemini"] }))
        .await;
    let (status, v) = h.get("/v1/models", Some(&user)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["object"], "list");
    let ids: Vec<&str> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            assert_eq!(m["object"], "model");
            m["id"].as_str().unwrap()
        })
        .collect();
    assert_eq!(ids, ["claude"]);
    let (_, v) = h.get("/v1/models", Some(&h.admin)).await;
    let ids: Vec<&str> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["claude", "codex"]);

    let (status, v) = h.get("/v1/models", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    openai_error(&v, "authentication_error", "unauthorized");
    h.running.shutdown().await;
}

#[tokio::test]
async fn pause_scope_and_limits_apply() {
    let h = harness().await;
    let user = h
        .user_token(json!({ "name": "capped", "agents": ["claude"],
                             "limits": { "dailyRequests": 1 } }))
        .await;
    let (status, v) = h
        .chat(
            &user,
            json!({ "model": "codex", "messages": user_says("hi") }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    openai_error(&v, "permission_error", "forbidden");
    let (status, v) = h
        .chat(
            &user,
            json!({ "model": "gemini", "messages": user_says("hi") }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");

    let (status, _) = h
        .chat(
            &user,
            json!({ "model": "claude", "messages": user_says("one") }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, v) = h
        .chat(
            &user,
            json!({ "model": "claude", "messages": user_says("two") }),
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    let e = openai_error(&v, "rate_limit_error", "limit_reached");
    assert_eq!(e["limit"], "dailyRequests", "{v}");

    let paused = h.user_token(json!({ "name": "sleepy" })).await;
    let (_, tokens) = h.get("/v1/tokens", Some(&h.admin)).await;
    let id = tokens["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "sleepy")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    h.call(
        reqwest::Method::PATCH,
        &format!("/v1/tokens/{id}"),
        Some(&h.admin),
        Some(json!({ "paused": true })),
    )
    .await;
    let (status, v) = h
        .chat(
            &paused,
            json!({ "model": "claude", "messages": user_says("hi") }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    openai_error(&v, "permission_error", "token_paused");
    let (status, v) = h
        .chat(
            "hashi_not-a-real-token",
            json!({ "model": "claude", "messages": user_says("hi") }),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    openai_error(&v, "authentication_error", "unauthorized");
    h.running.shutdown().await;
}

/// The `data:` payloads and comment lines of an event stream, in order.
fn sse_lines(body: &str) -> Vec<String> {
    body.lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn streaming_sends_chunks_keepalives_and_done() {
    let h = harness().await;
    let user = h.user_token(json!({})).await;
    let (status, kind, body) = h
        .chat_stream(
            &user,
            json!({ "model": "claude", "messages": user_says("slow hello"), "stream": true,
                    "stream_options": { "include_usage": true } }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(kind.starts_with("text/event-stream"), "{kind}");
    let lines = sse_lines(&body);
    let data: Vec<&str> = lines
        .iter()
        .filter_map(|l| l.strip_prefix("data: "))
        .collect();
    let comments: Vec<&str> = lines.iter().filter_map(|l| l.strip_prefix(": ")).collect();
    assert!(comments.contains(&"running"), "{body}");
    assert!(
        comments.iter().all(|c| *c == "queued" || *c == "running"),
        "{body}"
    );
    assert_eq!(data.last(), Some(&"[DONE]"), "{body}");

    let chunks: Vec<Value> = data[..data.len() - 1]
        .iter()
        .map(|d| serde_json::from_str(d).unwrap())
        .collect();
    assert_eq!(chunks.len(), 4, "{body}");
    for c in &chunks {
        assert_eq!(c["object"], "chat.completion.chunk", "{c}");
        assert_eq!(c["id"], chunks[0]["id"]);
        assert_eq!(c["model"], "claude");
    }
    assert_eq!(chunks[0]["choices"][0]["delta"]["role"], "assistant");
    assert!(chunks[0]["choices"][0]["finish_reason"].is_null());
    assert_eq!(
        chunks[1]["choices"][0]["delta"]["content"],
        "claude says slow hello"
    );
    assert_eq!(chunks[2]["choices"][0]["delta"], json!({}));
    assert_eq!(chunks[2]["choices"][0]["finish_reason"], "stop");
    assert_eq!(chunks[3]["choices"], json!([]));
    assert_eq!(chunks[3]["usage"]["total_tokens"], 12);

    // Keep-alives come after the role chunk and before the answer.
    let pos = |needle: &str| lines.iter().position(|l| l.contains(needle)).unwrap();
    assert!(pos("\"role\"") < pos(": running"));
    assert!(pos(": running") < pos("claude says slow hello"));
    h.running.shutdown().await;
}

#[tokio::test]
async fn streaming_failure_sends_an_error_event() {
    let h = harness().await;
    let user = h.user_token(json!({})).await;
    let (status, _, body) = h
        .chat_stream(
            &user,
            json!({ "model": "claude", "messages": user_says("fail now"), "stream": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let data: Vec<String> = sse_lines(&body)
        .into_iter()
        .filter_map(|l| l.strip_prefix("data: ").map(str::to_owned))
        .collect();
    assert!(!data.iter().any(|d| d == "[DONE]"), "{body}");
    let last: Value = serde_json::from_str(data.last().unwrap()).unwrap();
    openai_error(&last, "server_error", "agent_failed");
    assert!(
        last["error"]["message"]
            .as_str()
            .unwrap()
            .contains("simulated failure"),
        "{body}"
    );

    // Errors before the stream starts are plain OpenAI errors.
    let (status, _, body) = h
        .chat_stream(
            &user,
            json!({ "model": "gpt-4o", "messages": user_says("hi"), "stream": true }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    h.running.shutdown().await;
}

#[tokio::test]
async fn slow_answers_fall_back_to_202() {
    let h = harness().await;
    let user = h.user_token(json!({})).await;
    let started = std::time::Instant::now();
    let (status, v) = h
        .chat(
            &user,
            json!({ "model": "claude", "messages": user_says("slow one"), "wait": 1 }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{v}");
    assert!(started.elapsed() < Duration::from_secs(2));
    let id = v["id"].as_str().unwrap();
    assert_eq!(v["poll"], format!("/v1/ask/{id}"));
    assert!(v["message"].as_str().unwrap().contains("stream"), "{v}");
    let (status, r) = h.get(&format!("/v1/ask/{id}?wait=10"), Some(&user)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(r["response"], "claude says slow one");
    h.running.shutdown().await;
}

#[tokio::test]
async fn the_long_poll_cap_applies() {
    let h = harness().await;
    let user = h
        .user_token(json!({ "limits": { "maxPending": 10 } }))
        .await;
    // Four streams hold every wait slot this token has.
    let streams = futures::future::join_all((0..4).map(|i| {
        h.chat_stream(
            &user,
            json!({ "model": "claude", "messages": user_says(&format!("slow {i}")),
                    "stream": true }),
        )
    }));
    let extra = async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let started = std::time::Instant::now();
        let plain = h
            .chat(
                &user,
                json!({ "model": "claude", "messages": user_says("slow plain") }),
            )
            .await;
        let took = started.elapsed();
        let stream = h
            .chat_stream(
                &user,
                json!({ "model": "claude", "messages": user_says("slow more"), "stream": true }),
            )
            .await;
        (plain, took, stream)
    };
    let (streams, ((status, v), took, (s_status, _, s_body))) = tokio::join!(streams, extra);
    assert_eq!(status, StatusCode::ACCEPTED, "{v}");
    assert!(took < Duration::from_secs(1), "held for {took:?}");
    assert_eq!(s_status, StatusCode::TOO_MANY_REQUESTS, "{s_body}");
    let e: Value = serde_json::from_str(&s_body).unwrap();
    openai_error(&e, "rate_limit_error", "too_many_waits");
    for (status, _, body) in streams {
        assert_eq!(status, StatusCode::OK);
        assert!(body.ends_with("data: [DONE]\n\n"), "{body}");
    }
    h.running.shutdown().await;
}
