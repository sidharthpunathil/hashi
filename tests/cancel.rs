//! Cancel tests: `DELETE /v1/ask/{id}` and the MCP `cancel` tool against a
//! real server on a random port with fake agent executables.

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

/// Answers at once, or after two seconds for prompts starting with `slow`.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "9.9.9 (Fake Claude)"; exit 0; fi
for last; do :; done
case "$last" in
  slow*) sleep 2 ;;
esac
printf '{"type":"result","is_error":false,"result":"claude says %s","session_id":"s1","usage":{"input_tokens":5,"output_tokens":7}}\n' "$last"
"#;

/// A fake Claude that records its pid, then beats every 100 ms for a minute.
fn heartbeat_script(dir: &Path) -> String {
    format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"--version\" ]; then echo '9.9.9 (Fake Claude)'; exit 0; fi\n\
         echo $$ > '{pid}'\n\
         i=0\n\
         while [ $i -lt 600 ]; do echo $i >> '{beats}'; i=$((i+1)); sleep 0.1; done\n",
        pid = dir.join("agent.pid").display(),
        beats = dir.join("beats").display()
    )
}

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
    harness_with(FAKE_CLAUDE).await
}

async fn harness_with(claude: &str) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let mut cfg = AgentConfig::default_for(AgentKind::Claude);
    cfg.bin = write_script(&bin, "claude", claude);
    let agents = HashMap::from([(AgentKind::Claude, cfg)]);

    let cfg = ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        home: dir.path().join("home"),
        agents,
        default_agent: AgentKind::Claude,
        concurrency: 1,
        resources: hashi::guard::ResourceSettings::with_probe(std::sync::Arc::new(
            hashi::guard::ManualProbe::roomy(),
        )),
        limits: Limits {
            timeout: Duration::from_secs(60),
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

    async fn ask(&self, token: &str, message: &str) -> String {
        let (status, v) = self
            .call(
                reqwest::Method::POST,
                "/v1/ask",
                token,
                Some(json!({ "message": message })),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{v}");
        v["id"].as_str().unwrap().to_owned()
    }

    async fn cancel(&self, token: &str, id: &str) -> (StatusCode, Value) {
        self.call(
            reqwest::Method::DELETE,
            &format!("/v1/ask/{id}"),
            token,
            None,
        )
        .await
    }

    async fn status_of(&self, id: &str) -> Value {
        self.call(
            reqwest::Method::GET,
            &format!("/v1/ask/{id}"),
            &self.admin,
            None,
        )
        .await
        .1
    }

    async fn wait_for(&self, id: &str, want: &str) -> Value {
        for _ in 0..100 {
            let v = self.status_of(id).await;
            if v["status"] == want {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("request {id} never reached {want}");
    }

    async fn user_token(&self, name: &str) -> String {
        let (status, v) = self
            .call(
                reqwest::Method::POST,
                "/v1/tokens",
                &self.admin,
                Some(json!({ "name": name })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{v}");
        v["token"].as_str().unwrap().to_owned()
    }

    async fn audit_cancels(&self) -> Vec<Value> {
        let (_, v) = self
            .call(reqwest::Method::GET, "/v1/audit", &self.admin, None)
            .await;
        v["entries"]
            .as_array()
            .or_else(|| v.as_array())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e["action"] == "request.cancel")
            .collect()
    }

    async fn tool(&self, token: &str, name: &str, arguments: Value) -> Value {
        let (status, v) = self
            .call(
                reqwest::Method::POST,
                "/mcp",
                token,
                Some(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                             "params": { "name": name, "arguments": arguments } })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{v}");
        v["result"].clone()
    }
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

#[tokio::test]
async fn owner_cancels_a_queued_request() {
    let h = harness().await;
    let user = h.user_token("alice").await;
    let first = h.ask(&user, "slow one").await;
    let second = h.ask(&user, "slow two").await;
    h.wait_for(&first, "running").await;
    assert_eq!(h.status_of(&second).await["status"], "queued");

    let (status, v) = h.cancel(&user, &second).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "failed");
    assert_eq!(v["error"], "cancelled");
    // The running one is untouched, and the cancelled one never runs.
    let v = h.wait_for(&first, "completed").await;
    assert_eq!(v["response"], "claude says slow one");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let v = h.status_of(&second).await;
    assert_eq!(v["status"], "failed", "{v}");
    assert!(v.get("response").is_none(), "{v}");
    // A cancel by the owner is not audited.
    assert!(h.audit_cancels().await.is_empty());
    h.running.shutdown().await;
}

#[tokio::test]
async fn cancelling_a_running_request_kills_the_agent() {
    let probe = tempfile::tempdir().unwrap();
    let h = harness_with(&heartbeat_script(probe.path())).await;
    let user = h.user_token("alice").await;
    let id = h.ask(&user, "job").await;
    let pid = read_pid(&probe.path().join("agent.pid")).await;
    assert_eq!(h.status_of(&id).await["status"], "running");

    let (status, v) = h.cancel(&user, &id).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "failed");
    assert_eq!(v["error"], "cancelled");
    // The cancel waits for the kill, so the agent is gone almost at once.
    let mut gone = false;
    for _ in 0..10 {
        if !alive(&pid) {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if !gone {
        let _ = std::process::Command::new("kill")
            .args(["-9", &pid])
            .status();
    }
    assert!(gone, "agent {pid} kept running after cancel");
    let beats = probe.path().join("beats");
    let count = std::fs::read_to_string(&beats).unwrap().lines().count();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let after = std::fs::read_to_string(&beats).unwrap().lines().count();
    assert_eq!(count, after, "heartbeats continued after cancel");
    let v = h.status_of(&id).await;
    assert_eq!(v["status"], "failed", "{v}");
    assert_eq!(v["error"], "cancelled", "{v}");
    h.running.shutdown().await;
}

#[tokio::test]
async fn finished_requests_answer_409_and_others_404() {
    let h = harness().await;
    let alice = h.user_token("alice").await;
    let bob = h.user_token("bob").await;
    let done = h.ask(&alice, "hi").await;
    h.wait_for(&done, "completed").await;
    let (status, v) = h.cancel(&alice, &done).await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["error"]["code"], "conflict");
    assert_eq!(h.status_of(&done).await["status"], "completed");

    let pending = h.ask(&alice, "slow hi").await;
    let (status, v) = h.cancel(&bob, &pending).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{v}");
    let (status, _) = h.cancel(&bob, "no-such-request").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_ne!(h.status_of(&pending).await["status"], "failed");
    h.running.shutdown().await;
}

#[tokio::test]
async fn admin_cancels_any_request_and_is_audited() {
    let h = harness().await;
    let alice = h.user_token("alice").await;
    let first = h.ask(&alice, "slow one").await;
    let second = h.ask(&alice, "slow two").await;
    h.wait_for(&first, "running").await;
    let (status, v) = h.cancel(&h.admin, &second).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["error"], "cancelled");
    let entries = h.audit_cancels().await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0]["actor"], "admin");
    assert_eq!(entries[0]["target"], "alice");
    assert_eq!(entries[0]["detail"], second);
    h.running.shutdown().await;
}

#[tokio::test]
async fn library_cancel_is_what_the_dashboard_calls() {
    let h = harness().await;
    let alice = h.user_token("alice").await;
    let first = h.ask(&alice, "slow one").await;
    let second = h.ask(&alice, "slow two").await;
    h.wait_for(&first, "running").await;
    use hashi::queue::{CancelBy, CancelOutcome, cancel};
    let out = cancel(&h.running.state, &second, CancelBy::Other("dashboard"))
        .await
        .unwrap();
    assert!(matches!(out, CancelOutcome::Cancelled(_)), "{out:?}");
    let out = cancel(&h.running.state, &second, CancelBy::Other("dashboard"))
        .await
        .unwrap();
    assert!(matches!(out, CancelOutcome::Finished(_)), "{out:?}");
    let out = cancel(&h.running.state, "missing", CancelBy::Owner)
        .await
        .unwrap();
    assert_eq!(out, CancelOutcome::NotFound);
    let entries = h.audit_cancels().await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0]["actor"], "dashboard");
    h.running.shutdown().await;
}

#[tokio::test]
async fn mcp_cancel_tool_checks_ownership() {
    let h = harness().await;
    let alice = h.user_token("alice").await;
    let bob = h.user_token("bob").await;
    let (_, v) = h
        .call(
            reqwest::Method::POST,
            "/mcp",
            &alice,
            Some(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })),
        )
        .await;
    let names: Vec<&str> = v["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"cancel"), "{names:?}");

    let first = h.ask(&alice, "slow one").await;
    let second = h.ask(&alice, "slow two").await;
    h.wait_for(&first, "running").await;
    let r = h.tool(&bob, "cancel", json!({ "id": second })).await;
    assert_eq!(r["isError"], true, "{r}");
    assert_eq!(r["content"][0]["text"], "request not found");
    let r = h.tool(&alice, "cancel", json!({ "id": second })).await;
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(h.status_of(&second).await["error"], "cancelled");
    let r = h.tool(&alice, "cancel", json!({ "id": second })).await;
    assert_eq!(r["isError"], true, "{r}");
    assert_eq!(r["content"][0]["text"], "request already finished");
    h.running.shutdown().await;
}
