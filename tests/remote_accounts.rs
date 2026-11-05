//! Remote accounts end to end: two real Hashi servers on loopback ports in
//! one test. The far one ("bob's bridge") runs a fake Claude Code; the near
//! one has a remote account that forwards to it. No network beyond
//! 127.0.0.1, no real CLI, never the real `~/.hashi`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]

use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hashi::accounts::AccountKind;
use hashi::agent::{AgentConfig, AgentKind, Limits};
use hashi::events::Bus;
use hashi::remote::Remote;
use hashi::server::{self, Running, ServerConfig};
use hashi::usage::{Policy, Source, Strategy, UsageSettings, Window};
use hashi::util::now_ms;
use reqwest::StatusCode;
use serde_json::{Value, json};

/// The far side's agent. Knows its account from `$CLAUDE_CONFIG_DIR/name`;
/// a prompt containing `slow` sleeps first, so it can be cancelled, and one
/// containing `boom` fails with an error that names the far machine.
const FAR_CLAUDE: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "9.9.9 (Fake Claude)"; exit 0; fi
for last; do :; done
acct=default
dir="$CLAUDE_CONFIG_DIR"
if [ -n "$dir" ] && [ -f "$dir/name" ]; then acct=$(cat "$dir/name"); fi
case "$last" in *slow*) sleep 30 ;; esac
case "$last" in *boom*) echo '{"type":"result","is_error":true,"subtype":"error_during_execution","result":"bob-laptop: disk /Users/bob is full"}'; exit 1 ;; esac
echo '{"type":"system","subtype":"init","session_id":"s"}'
printf '{"type":"result","subtype":"success","is_error":false,"result":"far %s on %s","session_id":"sess","total_cost_usd":0.25,"usage":{"input_tokens":3,"output_tokens":4}}\n' "$last" "$acct"
"#;

/// The near side's own agent, for failover to a local account.
const NEAR_CLAUDE: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "9.9.9 (Fake Claude)"; exit 0; fi
for last; do :; done
echo '{"type":"system","subtype":"init","session_id":"s"}'
printf '{"type":"result","subtype":"success","is_error":false,"result":"near %s","session_id":"sess"}\n' "$last"
"#;

struct Server {
    dir: tempfile::TempDir,
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

async fn server(script: &str, timeout: Duration) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let mut claude = AgentConfig::default_for(AgentKind::Claude);
    claude.bin = write_script(&bin, "claude", script);
    let usage = UsageSettings {
        poll: false,
        claude_base_url: "http://127.0.0.1:1".to_owned(),
        security_bin: None,
        keychain_user: None,
        claude_home: Some(dir.path().join("no-claude-home")),
        codex_bin: Some(dir.path().join("no-codex")),
        codex_home: Some(dir.path().join("no-codex-home")),
        ..UsageSettings::default()
    };
    let cfg = ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        home: dir.path().join("home"),
        agents: HashMap::from([(AgentKind::Claude, claude)]),
        default_agent: AgentKind::Claude,
        concurrency: 2,
        resources: hashi::guard::ResourceSettings::with_probe(Arc::new(
            hashi::guard::ManualProbe::roomy(),
        )),
        limits: Limits {
            timeout,
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
        account_cooldown: Duration::from_secs(3600),
        usage,
    };
    let running = server::start(cfg, Bus::default()).await.unwrap();
    let base = running.state.runtime.local_url();
    let admin = running.bootstrap_token.clone().unwrap();
    Server {
        dir,
        running,
        base,
        admin,
        http: reqwest::Client::new(),
    }
}

impl Server {
    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn store(&self) -> &hashi::store::Store {
        &self.running.state.store
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        token: &str,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> (StatusCode, Value) {
        let mut req = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(token);
        for (k, v) in headers {
            req = req.header(*k, *v);
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

    async fn get(&self, path: &str, token: &str) -> (StatusCode, Value) {
        self.call(reqwest::Method::GET, path, token, None, &[])
            .await
    }

    async fn post(&self, path: &str, token: &str, body: Value) -> (StatusCode, Value) {
        self.call(reqwest::Method::POST, path, token, Some(body), &[])
            .await
    }

    async fn patch(&self, path: &str, token: &str, body: Value) -> (StatusCode, Value) {
        self.call(reqwest::Method::PATCH, path, token, Some(body), &[])
            .await
    }

    /// A new token and its id.
    async fn token(&self, body: Value) -> (String, String) {
        let (status, v) = self.post("/v1/tokens", &self.admin, body).await;
        assert_eq!(status, StatusCode::CREATED, "{v}");
        (
            v["token"].as_str().unwrap().to_owned(),
            v["record"]["id"].as_str().unwrap().to_owned(),
        )
    }

    async fn ask(&self, token: &str, message: &str) -> Value {
        let (status, v) = self
            .post("/v1/ask", token, json!({"message": message, "wait": 20}))
            .await;
        assert!(status.is_success(), "{v}");
        v
    }

    /// A login account whose fake agent answers with `name`.
    fn add_login(&self, name: &str) {
        let dir = hashi::accounts::create_dir(&self.home(), AgentKind::Claude, name).unwrap();
        std::fs::write(dir.join("name"), name).unwrap();
        self.store()
            .add_account(AgentKind::Claude, name, AccountKind::Login)
            .unwrap();
    }

    /// Saves `remotes` in this server's home, as `hashi connect` would.
    fn save_remotes(&self, remotes: &[(&str, &str, &str)]) {
        let list: Vec<Remote> = remotes
            .iter()
            .map(|(name, base, token)| Remote {
                name: (*name).to_owned(),
                base_url: (*base).to_owned(),
                token: (*token).to_owned(),
            })
            .collect();
        hashi::remote::save(&self.home(), &list).unwrap();
    }

    fn add_remote(&self, name: &str, remote: &str) {
        assert!(
            self.store()
                .add_remote_account(AgentKind::Claude, name, remote, AgentKind::Claude)
                .unwrap()
        );
    }

    fn account(&self, name: &str) -> hashi::store::AccountState {
        self.store()
            .get_account(AgentKind::Claude, name)
            .unwrap()
            .unwrap()
    }

    async fn account_json(&self, name: &str) -> Value {
        let (status, v) = self.get("/v1/accounts", &self.admin).await;
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

/// A far server and a near one whose `bob-claude` account forwards to it
/// with `far_token`.
async fn pair(far_token_body: Value) -> (Server, Server, String, String) {
    let far = server(FAR_CLAUDE, Duration::from_secs(60)).await;
    let near = server(NEAR_CLAUDE, Duration::from_secs(60)).await;
    let (far_token, far_id) = far.token(far_token_body).await;
    near.save_remotes(&[("bob", &far.base, &far_token)]);
    near.add_remote("bob-claude", "bob");
    (far, near, far_token, far_id)
}

async fn wait_for(mut check: impl FnMut() -> bool, what: &str) {
    for _ in 0..100 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_remote_account_forwards_with_usage_and_cost() {
    let (far, near, _, _) = pair(json!({"name": "near-owner"})).await;
    let (token, _) = near
        .token(json!({"name": "friend", "accounts": {"claude": ["bob-claude"]}}))
        .await;
    let v = near.ask(&token, "hello").await;
    assert_eq!(v["status"], "completed", "{v}");
    assert_eq!(v["response"], "far hello on default");
    assert_eq!(v["usage"]["inputTokens"], 3);
    assert_eq!(v["usage"]["outputTokens"], 4);
    assert_eq!(v["usage"]["costUsd"], 0.25);
    // Token holders never see which account ran it, so never that it is
    // remote.
    assert!(v.get("account").is_none(), "{v}");
    let (_, admin_view) = near
        .get(
            &format!("/v1/ask/{}", v["id"].as_str().unwrap()),
            &near.admin,
        )
        .await;
    assert_eq!(admin_view["account"], "bob-claude");
    // The cost counts against the near token's own budget.
    let (_, me) = near.get("/v1/me", &token).await;
    assert_eq!(me["usage"]["todayCostUsd"], 0.25);
    assert!(me.get("agentUsage").is_none(), "{me}");
    // The far side saw one hop.
    let far_req = far.store().recent_requests(5).unwrap();
    assert_eq!(far_req.len(), 1);
    assert_eq!(far_req[0].hops, 1);
    assert_eq!(far_req[0].message, "hello");
    // The account shows where it goes.
    let a = near.account_json("bob-claude").await;
    assert_eq!(a["kind"], "remote");
    assert_eq!(a["remote"], "bob");
    assert_eq!(a["state"], "ready");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_limited_remote_is_marked_until_its_reset_and_fails_over() {
    let (_far, near, _, _) =
        pair(json!({"name": "near-owner", "limits": {"dailyRequests": 1}})).await;
    let (token, _) = near
        .token(json!({"name": "friend", "accounts": {"claude": ["bob-claude", "default"]}}))
        .await;
    assert_eq!(
        near.ask(&token, "one").await["response"],
        "far one on default"
    );
    // The far token's daily cap is used up: the remote answers 429 with
    // resetAt, the account is limited until then and the prompt runs on
    // the next account.
    let v = near.ask(&token, "two").await;
    assert_eq!(v["response"], "near two", "{v}");
    let until = near.account("bob-claude").limited_until.unwrap();
    assert_eq!(until, hashi::util::next_day_utc(now_ms()));
    let a = near.account_json("bob-claude").await;
    assert_eq!(a["state"], "limited");
    assert_eq!(a["summary"]["source"], "error");
}

#[tokio::test(flavor = "multi_thread")]
async fn paused_and_revoked_remote_tokens_disable_the_account() {
    let far = server(FAR_CLAUDE, Duration::from_secs(60)).await;
    let near = server(NEAR_CLAUDE, Duration::from_secs(60)).await;
    let (paused, paused_id) = far.token(json!({"name": "p"})).await;
    let (revoked, revoked_id) = far.token(json!({"name": "r"})).await;
    near.save_remotes(&[
        ("bob-p", &far.base, &paused),
        ("bob-r", &far.base, &revoked),
    ]);
    near.add_remote("rp", "bob-p");
    near.add_remote("rr", "bob-r");
    let (status, v) = far
        .patch(
            &format!("/v1/tokens/{paused_id}"),
            &far.admin,
            json!({"paused": true}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (status, v) = far
        .call(
            reqwest::Method::DELETE,
            &format!("/v1/tokens/{revoked_id}"),
            &far.admin,
            None,
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");

    let (token, _) = near
        .token(json!({"name": "friend", "accounts": {"claude": ["rp", "rr", "default"]}}))
        .await;
    let v = near.ask(&token, "hi").await;
    assert_eq!(v["response"], "near hi", "{v}");
    let p = near.account("rp");
    assert!(p.disabled);
    assert!(
        p.disabled_reason.as_deref().unwrap().contains("paused"),
        "{p:?}"
    );
    let r = near.account("rr");
    assert!(r.disabled);
    assert!(
        r.disabled_reason.as_deref().unwrap().contains("revoked"),
        "{r:?}"
    );
    let a = near.account_json("rp").await;
    assert_eq!(a["state"], "disabled");
    assert!(a["disabledReason"].as_str().unwrap().contains("paused"));

    // With nothing left, the token holder hears nothing about remotes.
    let (only, _) = near
        .token(json!({"name": "only", "accounts": {"claude": ["rp"]}}))
        .await;
    let v = near.ask(&only, "x").await;
    assert_eq!(v["status"], "failed");
    let err = v["error"].as_str().unwrap();
    assert!(!err.contains("bob") && !err.contains(&far.base), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_network_failure_falls_over_to_the_next_account() {
    let near = server(NEAR_CLAUDE, Duration::from_secs(60)).await;
    near.save_remotes(&[(
        "gone",
        "http://127.0.0.1:1",
        "hashi_0123456789abcdef0123456789abcdef",
    )]);
    near.add_remote("gone-claude", "gone");
    let (token, _) = near
        .token(json!({"name": "friend", "accounts": {"claude": ["gone-claude", "default"]}}))
        .await;
    let v = near.ask(&token, "hi").await;
    assert_eq!(v["response"], "near hi", "{v}");
    let a = near.account("gone-claude");
    assert!(!a.disabled, "a network failure is not the token's fault");
    assert!(a.limited_until.is_none());

    // Alone, it fails without naming the bridge, and the account sits
    // out a few seconds rather than being tried on every prompt.
    let (alone, _) = near
        .token(json!({"name": "alone", "accounts": {"claude": ["gone-claude"]}}))
        .await;
    let v = near.ask(&alone, "hi").await;
    assert_eq!(v["status"], "failed", "{v}");
    let err = v["error"].as_str().unwrap();
    assert!(err.contains("try again in a minute"), "{err}");
    assert!(!err.contains("127.0.0.1") && !err.contains("gone"), "{err}");
    assert!(!err.contains("bridge"), "{err}");
}

/// A far token's maxPending is a transient refusal one holder here can
/// cause with a burst. It backs the remote account off for seconds, and
/// never marks it limited for the hour-long cooldown.
#[tokio::test(flavor = "multi_thread")]
async fn a_busy_far_token_backs_the_account_off_for_seconds_not_the_cooldown() {
    let (_far, near, _, _) = pair(json!({"name": "near-owner", "limits": {"maxPending": 1}})).await;
    let (token, _) = near
        .token(
            json!({"name": "friend", "accounts": {"claude": ["bob-claude"]},
                      "limits": {"maxPending": 3}}),
        )
        .await;
    // Two concurrent slow prompts: the far token allows one in flight.
    let (s1, one) = near
        .post("/v1/ask", &token, json!({"message": "slow one"}))
        .await;
    let (s2, two) = near
        .post("/v1/ask", &token, json!({"message": "slow two"}))
        .await;
    assert!(s1.is_success() && s2.is_success(), "{one} {two}");
    let ids = [
        one["id"].as_str().unwrap().to_owned(),
        two["id"].as_str().unwrap().to_owned(),
    ];
    let failed = || {
        ids.iter().find_map(|id| {
            near.store()
                .get_request(id)
                .unwrap()
                .filter(|r| r.status == hashi::store::RequestStatus::Failed)
        })
    };
    wait_for(|| failed().is_some(), "the refused prompt to fail").await;
    let err = failed().unwrap().error.unwrap_or_default();
    assert!(err.contains("try again in a minute"), "{err}");
    assert!(!err.contains("bridge") && !err.contains("bob"), "{err}");
    let a = near.account("bob-claude");
    assert!(
        a.limited_until.is_none(),
        "limited until {:?}",
        a.limited_until
    );
    assert!(!a.disabled);
    assert!(
        near.running
            .state
            .dispatcher
            .backed_off(AgentKind::Claude, "bob-claude")
    );
}

/// A run that fails on the far bridge reaches the token holder as neutral
/// text; the far side's words stay with the owner.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_far_run_shows_the_holder_neutral_text() {
    let (_far, near, _, _) = pair(json!({"name": "near-owner"})).await;
    let (token, _) = near
        .token(json!({"name": "friend", "accounts": {"claude": ["bob-claude"]}}))
        .await;
    let mut log = near.running.state.bus.subscribe();
    let v = near.ask(&token, "boom").await;
    assert_eq!(v["status"], "failed", "{v}");
    assert_eq!(v["error"], "the run failed", "{v}");
    // The owner still sees what happened, in the dashboard log.
    let mut owner_saw = false;
    loop {
        match log.try_recv() {
            Ok(hashi::events::Event::Log(line)) if line.contains("disk /Users/bob is full") => {
                owner_saw = true;
            }
            Ok(_) | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(_) => break,
        }
    }
    assert!(owner_saw, "the owner's log has the far error");
}

/// A prompt that fits this bridge's 64 KB body cap but not the far one's
/// once forwarded is refused here with the usual 413, never sent.
#[tokio::test(flavor = "multi_thread")]
async fn a_prompt_too_big_to_forward_is_refused_with_413() {
    let (_far, near, _, _) = pair(json!({"name": "near-owner"})).await;
    let (token, _) = near
        .token(json!({"name": "friend", "accounts": {"claude": ["bob-claude"]}}))
        .await;
    let msg = "\"".repeat(32_760);
    let body = format!("{{\"message\":{}}}", serde_json::to_string(&msg).unwrap());
    assert!(body.len() <= 65_536, "{}", body.len());
    let res = near
        .http
        .post(format!("{}/v1/ask", near.base))
        .bearer_auth(&token)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let v: Value = res.json().await.unwrap();
    assert_eq!(v["error"]["code"], "too_large", "{v}");
    let text = v["error"]["message"].as_str().unwrap();
    assert!(!text.contains("bridge") && !text.contains("remote"), "{v}");
    // A token without a remote account takes the same prompt.
    let (local, _) = near.token(json!({"name": "local"})).await;
    let (status, v) = near
        .post("/v1/ask", &local, json!({ "message": msg }))
        .await;
    assert!(status.is_success(), "{v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_locally_cancels_on_the_remote_bridge() {
    let (far, near, _, _) = pair(json!({"name": "near-owner"})).await;
    let (token, _) = near
        .token(json!({"name": "friend", "accounts": {"claude": ["bob-claude"]}}))
        .await;
    let (status, v) = near
        .post("/v1/ask", &token, json!({"message": "slow one"}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{v}");
    let id = v["id"].as_str().unwrap().to_owned();
    let far_store = Arc::clone(&far.running.state.store);
    wait_for(
        || {
            far_store
                .recent_requests(5)
                .unwrap()
                .iter()
                .any(|r| r.status == hashi::store::RequestStatus::Running)
        },
        "the far run to start",
    )
    .await;
    let (status, v) = near
        .call(
            reqwest::Method::DELETE,
            &format!("/v1/ask/{id}"),
            &token,
            None,
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    wait_for(
        || {
            far_store.recent_requests(5).unwrap().iter().all(|r| {
                r.status == hashi::store::RequestStatus::Failed
                    && r.error.as_deref() == Some("cancelled")
            })
        },
        "the far request to be cancelled",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_remote_hits_the_local_timeout_and_is_cancelled_there() {
    let far = server(FAR_CLAUDE, Duration::from_secs(60)).await;
    let near = server(NEAR_CLAUDE, Duration::from_secs(2)).await;
    let (far_token, _) = far.token(json!({"name": "near-owner"})).await;
    near.save_remotes(&[("bob", &far.base, &far_token)]);
    near.add_remote("bob-claude", "bob");
    let (token, _) = near
        .token(json!({"name": "friend", "accounts": {"claude": ["bob-claude"]}}))
        .await;
    let v = near.ask(&token, "slow two").await;
    assert_eq!(v["status"], "failed", "{v}");
    assert!(v["error"].as_str().unwrap().contains("timed out"), "{v}");
    let far_store = Arc::clone(&far.running.state.store);
    wait_for(
        || {
            far_store
                .recent_requests(5)
                .unwrap()
                .iter()
                .all(|r| r.status == hashi::store::RequestStatus::Failed)
        },
        "the far request to be cancelled after the timeout",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_remote_pointing_at_this_bridge_is_refused() {
    let near = server(NEAR_CLAUDE, Duration::from_secs(60)).await;
    let (own, _) = near.token(json!({"name": "me"})).await;
    near.save_remotes(&[("me", &near.base, &own)]);
    let home = near.home();
    let out = tokio::task::spawn_blocking(move || {
        std::process::Command::new(env!("CARGO_BIN_EXE_hashi"))
            .arg("--home")
            .arg(home)
            .args(["account", "add", "claude", "loop", "--remote", "me"])
            .env("HASHI_NO_UPDATE_CHECK", "1")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("this same Hashi"), "{err}");
    assert!(
        near.store()
            .get_account(AgentKind::Claude, "loop")
            .unwrap()
            .is_none()
    );

    // A prompt that names this bridge as its sender is refused too, so a
    // loop added some other way still stops at once.
    let instance = near.store().instance_id().unwrap();
    let (status, _) = near.get("/v1/status", "").await;
    assert_eq!(status, StatusCode::OK);
    let (_, st) = near.get("/v1/status", "").await;
    assert_eq!(st["instance"], instance.as_str());
    let (status, v) = near
        .call(
            reqwest::Method::POST,
            "/v1/ask",
            &own,
            Some(json!({"message": "hi"})),
            &[("X-Hashi-From", &instance)],
        )
        .await;
    assert_eq!(status, StatusCode::LOOP_DETECTED, "{v}");
    assert_eq!(v["error"]["code"], "loop");

    // A remote account added anyway disables itself on its first run.
    near.add_remote("loop", "me");
    let (token, _) = near
        .token(json!({"name": "friend", "accounts": {"claude": ["loop", "default"]}}))
        .await;
    let v = near.ask(&token, "hi").await;
    assert_eq!(v["response"], "near hi", "{v}");
    let a = near.account("loop");
    assert!(a.disabled);
    assert!(a.disabled_reason.unwrap().contains("this same Hashi"));
}

#[tokio::test(flavor = "multi_thread")]
async fn prompts_past_two_hops_are_refused_and_never_forwarded() {
    let (far, near, _, _) = pair(json!({"name": "near-owner"})).await;
    let (token, _) = near
        .token(json!({"name": "friend", "accounts": {"claude": ["bob-claude"]}}))
        .await;
    let (status, v) = near
        .call(
            reqwest::Method::POST,
            "/v1/ask",
            &token,
            Some(json!({"message": "hi"})),
            &[("X-Hashi-Hops", "3")],
        )
        .await;
    assert_eq!(status, StatusCode::LOOP_DETECTED, "{v}");
    assert_eq!(v["error"]["code"], "hop_limit");
    // Two hops are accepted here but not forwarded again.
    let (status, v) = near
        .call(
            reqwest::Method::POST,
            "/v1/ask",
            &token,
            Some(json!({"message": "hi", "wait": 20})),
            &[("X-Hashi-Hops", "2")],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "failed", "{v}");
    assert!(far.store().recent_requests(5).unwrap().is_empty());
    // One hop in goes out as two.
    let (status, v) = near
        .call(
            reqwest::Method::POST,
            "/v1/ask",
            &token,
            Some(json!({"message": "hi", "wait": 20})),
            &[("X-Hashi-Hops", "1")],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "completed", "{v}");
    assert_eq!(far.store().recent_requests(5).unwrap()[0].hops, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn shared_usage_shows_headroom_only_and_only_when_on() {
    let far = server(FAR_CLAUDE, Duration::from_secs(60)).await;
    far.add_login("x");
    far.add_login("y");
    let reset = now_ms() + 3_600_000;
    for (name, used) in [("x", 70.0), ("y", 40.0)] {
        far.store()
            .replace_usage(
                AgentKind::Claude,
                name,
                &[Window::new(
                    "five_hour",
                    "5 hours",
                    used,
                    Some(reset),
                    Some(300),
                    Source::Poll,
                )],
            )
            .unwrap();
    }
    let (shared, _) = far
        .token(
            json!({"name": "s", "agents": ["claude"], "accounts": {"claude": ["x", "y"]},
                      "shareUsage": true}),
        )
        .await;
    let (plain, _) = far
        .token(json!({"name": "p", "agents": ["claude"], "accounts": {"claude": ["x", "y"]}}))
        .await;
    let (_, me) = far.get("/v1/me", &shared).await;
    let usage = me["agentUsage"].as_object().unwrap();
    assert_eq!(usage.keys().collect::<Vec<_>>(), ["claude"]);
    let claude = usage["claude"].as_object().unwrap();
    let mut keys: Vec<&String> = claude.keys().collect();
    keys.sort();
    assert_eq!(keys, ["headroom", "nextReset", "state"]);
    assert_eq!(claude["headroom"], 60);
    assert_eq!(claude["state"], "ok");
    assert_eq!(
        claude["nextReset"],
        hashi::util::rfc3339(reset).unwrap().as_str()
    );
    let text = me.to_string();
    for hidden in ["\"x\"", "\"y\"", "5 hours", "five_hour", "accounts"] {
        assert!(!text.contains(hidden), "{hidden} in {text}");
    }
    let (_, me) = far.get("/v1/me", &plain).await;
    assert!(me.get("agentUsage").is_none(), "{me}");

    // Every account at its limit: limited, with when one comes back.
    let back = now_ms() + 7_200_000;
    far.store()
        .mark_account_limited(AgentKind::Claude, "x", back)
        .unwrap();
    far.store()
        .mark_account_limited(AgentKind::Claude, "y", back + 1000)
        .unwrap();
    let (_, me) = far.get("/v1/me", &shared).await;
    assert_eq!(me["agentUsage"]["claude"]["state"], "limited");
    assert_eq!(me["agentUsage"]["claude"]["headroom"], 0);
    assert_eq!(
        me["agentUsage"]["claude"]["nextReset"],
        hashi::util::rfc3339(back).unwrap().as_str()
    );

    // The flag is on tokens and invites, and an invite passes it on.
    let (status, inv) = far
        .post(
            "/v1/invites",
            &far.admin,
            json!({"label": "carol", "shareUsage": true}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{inv}");
    assert_eq!(inv["invite"]["shareUsage"], true);
    let code = inv["url"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned();
    let (_, preview) = far
        .post("/v1/claim/preview", "", json!({"code": code}))
        .await;
    assert_eq!(preview["shareUsage"], true);
    let (status, claimed) = far
        .post("/v1/claim/redeem", "", json!({"code": code}))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{claimed}");
    let (_, me) = far.get("/v1/me", claimed["token"].as_str().unwrap()).await;
    assert!(me["agentUsage"]["claude"].is_object(), "{me}");
}

#[tokio::test(flavor = "multi_thread")]
async fn strategies_use_the_headroom_a_remote_shares() {
    let far = server(FAR_CLAUDE, Duration::from_secs(60)).await;
    far.add_login("x");
    far.add_login("y");
    let reset = now_ms() + 3_600_000;
    for (name, used) in [("x", 80.0), ("y", 10.0)] {
        far.store()
            .replace_usage(
                AgentKind::Claude,
                name,
                &[Window::new(
                    "five_hour",
                    "5 hours",
                    used,
                    Some(reset),
                    Some(300),
                    Source::Poll,
                )],
            )
            .unwrap();
    }
    let (ta, _) = far
        .token(json!({"name": "a", "accounts": {"claude": ["x"]}, "shareUsage": true}))
        .await;
    let (tb, _) = far
        .token(json!({"name": "b", "accounts": {"claude": ["y"]}, "shareUsage": true}))
        .await;
    let (tc, _) = far
        .token(json!({"name": "c", "limits": {"dailyRequests": 4}}))
        .await;
    let near = server(NEAR_CLAUDE, Duration::from_secs(60)).await;
    near.save_remotes(&[
        ("bob-a", &far.base, &ta),
        ("bob-b", &far.base, &tb),
        ("bob-c", &far.base, &tc),
    ]);
    near.add_remote("ra", "bob-a");
    near.add_remote("rb", "bob-b");
    near.add_remote("rc", "bob-c");
    for name in ["ra", "rb", "rc"] {
        let (status, v) = near
            .post(
                &format!("/v1/accounts/claude/{name}/refresh"),
                &near.admin,
                json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert_eq!(v["ok"], true, "{v}");
    }
    let ra = near.account_json("ra").await;
    assert_eq!(ra["summary"]["source"], "remote");
    assert_eq!(ra["summary"]["headroom"], 20);
    assert_eq!(ra["summary"]["plan"], "via bob-a, shared");
    assert_eq!(near.account_json("rb").await["summary"]["headroom"], 90);
    // Without sharing, the token's own caps are the figures.
    let rc = near.account_json("rc").await;
    assert_eq!(rc["summary"]["headroom"], 100);
    assert_eq!(rc["usage"][0]["key"], "daily_requests");

    near.running.state.dispatcher.usage().set_policy(Policy {
        strategy: Strategy::MostLeft,
        rotate_at: 90,
    });
    let (token, _) = near
        .token(json!({"name": "friend", "accounts": {"claude": ["ra", "rb"]}}))
        .await;
    assert_eq!(near.ask(&token, "pick").await["response"], "far pick on y");

    // The caps running out make the account limited until their reset.
    let (ctoken, _) = near
        .token(json!({"name": "capped", "accounts": {"claude": ["rc", "default"]}}))
        .await;
    for i in 0..4 {
        assert_eq!(
            near.ask(&ctoken, &format!("c{i}")).await["response"],
            format!("far c{i} on default")
        );
    }
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    // Reads are at most once a minute; the stored figures are rewritten
    // the way a poll would.
    near.store()
        .set_poll_status(AgentKind::Claude, "rc", true, None, None)
        .unwrap();
    let target = hashi::forward::Target::new(
        &hashi::remote::load(&near.home())
            .unwrap()
            .into_iter()
            .find(|r| r.name == "bob-c")
            .unwrap(),
        AgentKind::Claude,
    );
    let me = tokio::task::spawn_blocking(move || hashi::forward::read_me(&target))
        .await
        .unwrap()
        .unwrap();
    let reading = hashi::forward::me_windows(&me, AgentKind::Claude, "bob-c");
    near.store()
        .replace_usage(AgentKind::Claude, "rc", &reading.windows)
        .unwrap();
    let a = near.account_json("rc").await;
    assert_eq!(a["summary"]["state"], "limited", "{a}");
    assert_eq!(
        a["summary"]["limitedUntil"],
        hashi::util::rfc3339(hashi::util::next_day_utc(now_ms()))
            .unwrap()
            .as_str()
    );
    assert_eq!(near.ask(&ctoken, "c5").await["response"], "near c5");
}

/// Captures every log line of this test binary.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_tokens_never_reach_logs_or_api_output() {
    let capture = Capture::default();
    let writer = capture.clone();
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .try_init();
    let (far, near, far_token, far_id) =
        pair(json!({"name": "near-owner", "shareUsage": true})).await;
    let mut events = near.running.state.bus.subscribe();
    let (token, _) = near
        .token(json!({"name": "friend", "accounts": {"claude": ["bob-claude", "default"]}}))
        .await;
    let ok = near.ask(&token, "hello").await;
    let (_, refreshed) = near
        .post(
            "/v1/accounts/claude/bob-claude/refresh",
            &near.admin,
            json!({}),
        )
        .await;
    // Then the far side pauses the token, so failure paths log too.
    far.patch(
        &format!("/v1/tokens/{far_id}"),
        &far.admin,
        json!({"paused": true}),
    )
    .await;
    let failed_over = near.ask(&token, "again").await;
    assert_eq!(failed_over["response"], "near again");

    let mut outputs = vec![
        ok.to_string(),
        failed_over.to_string(),
        refreshed.to_string(),
    ];
    for path in [
        "/v1/accounts",
        "/v1/tokens",
        "/v1/audit",
        "/v1/stats",
        "/v1/status",
    ] {
        outputs.push(near.get(path, &near.admin).await.1.to_string());
    }
    outputs.push(near.get("/v1/me", &token).await.1.to_string());
    while let Ok(ev) = events.try_recv() {
        outputs.push(format!("{ev:?}"));
    }
    let home = near.home();
    let cli = tokio::task::spawn_blocking(move || {
        let run = |args: &[&str]| {
            let out = std::process::Command::new(env!("CARGO_BIN_EXE_hashi"))
                .arg("--home")
                .arg(&home)
                .args(args)
                .env("HASHI_NO_UPDATE_CHECK", "1")
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        };
        vec![
            run(&["account", "list"]),
            run(&["account", "usage"]),
            run(&["account", "usage", "--json"]),
            run(&["remote", "list"]),
        ]
    })
    .await
    .unwrap();
    assert!(cli[0].contains("remote via bob"), "{}", cli[0]);
    assert!(cli[0].contains("paused"), "{}", cli[0]);
    assert!(cli[1].contains("via bob"), "{}", cli[1]);
    outputs.extend(cli);
    // The database holds the remote's name only.
    for file in ["hashi.db", "hashi.db-wal"] {
        let db = std::fs::read(near.home().join(file)).unwrap_or_default();
        outputs.push(String::from_utf8_lossy(&db).into_owned());
    }
    outputs.push(String::from_utf8_lossy(&capture.0.lock().unwrap()).into_owned());
    let secret = &far_token["hashi_".len()..];
    for out in &outputs {
        assert!(!out.contains(secret), "the remote token leaked: {out}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_remote_name_reused_for_another_bridge_does_not_take_the_account_over() {
    let (far, near, _, _) = pair(json!({"name": "near-owner"})).await;
    // As `hashi account add --remote` records it.
    let far_id = far.store().instance_id().unwrap();
    near.store()
        .set_remote_instance(AgentKind::Claude, "bob-claude", Some(&far_id))
        .unwrap();
    let (token, _) = near
        .token(json!({"name": "friend", "accounts": {"claude": ["bob-claude", "default"]}}))
        .await;
    let v = near.ask(&token, "one").await;
    assert_eq!(v["response"], "far one on default", "{v}");

    // `bob` now names another bridge, without `hashi remote remove`.
    let other = server(FAR_CLAUDE, Duration::from_secs(60)).await;
    let (other_token, _) = other.token(json!({"name": "stranger"})).await;
    near.save_remotes(&[("bob", &other.base, &other_token)]);
    let v = near.ask(&token, "two").await;
    assert_eq!(v["response"], "near two", "{v}");
    assert!(other.store().recent_requests(5).unwrap().is_empty());
    let a = near.account("bob-claude");
    assert!(a.disabled);
    assert!(
        a.disabled_reason
            .unwrap()
            .contains("no longer the bridge this account was added for")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_a_remote_switches_its_accounts_off() {
    let (_far, near, _, _) = pair(json!({"name": "near-owner"})).await;
    near.add_login("work");
    hashi::remote::forget(&near.home(), "bob").unwrap();
    let a = near.account("bob-claude");
    assert!(a.disabled);
    assert!(a.disabled_reason.unwrap().contains("was removed"));
    assert!(!near.account("work").disabled);
}
