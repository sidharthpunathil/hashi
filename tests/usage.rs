//! Usage limits end to end: a real server on a random port, a fake Claude
//! Code that prints `stream-json` (with `rate_limit_event` messages when
//! asked), account strategies, sticky conversations, the refresh endpoint
//! against a local stand-in for the usage endpoint, and what token holders
//! never see. No network, no real Keychain, no real CLI.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hashi::agent::{AgentConfig, AgentKind, Limits};
use hashi::events::{Bus, Event};
use hashi::server::{self, Running, ServerConfig};
use hashi::usage::{Policy, Source, Strategy, UsageSettings, UsageState, Window};
use hashi::util::now_ms;
use reqwest::StatusCode;
use serde_json::{Value, json};

/// Knows its account from `$CLAUDE_CONFIG_DIR/name`. With an `event` file
/// there it prints that `rate_limit_info` first; with `rejected` it prints
/// a rejected limit resetting at the file's Unix seconds and fails.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "9.9.9 (Fake Claude)"; exit 0; fi
for last; do :; done
acct=default
dir="$CLAUDE_CONFIG_DIR"
if [ -n "$dir" ] && [ -f "$dir/name" ]; then acct=$(cat "$dir/name"); fi
echo '{"type":"system","subtype":"init","session_id":"s"}'
if [ -n "$dir" ] && [ -f "$dir/rejected" ]; then
  printf '{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":%s,"rateLimitType":"five_hour","utilization":1}}\n' "$(cat "$dir/rejected")"
  echo '{"type":"assistant","message":{"content":[]},"error":"rate_limit","session_id":"s"}'
  echo '{"type":"result","subtype":"success","is_error":true,"result":"Claude AI usage limit reached","session_id":"s"}'
  exit 1
fi
if [ -n "$dir" ] && [ -f "$dir/event" ]; then
  printf '{"type":"rate_limit_event","rate_limit_info":%s,"session_id":"s"}\n' "$(cat "$dir/event")"
fi
resumed=no
prev=""
for a in "$@"; do
  if [ "$prev" = "--resume" ]; then resumed="$a"; fi
  prev="$a"
done
printf '{"type":"result","subtype":"success","is_error":false,"result":"%s on %s (resume=%s)","session_id":"sess-%s","total_cost_usd":0.25,"usage":{"input_tokens":3,"output_tokens":4}}\n' "$last" "$acct" "$resumed" "$acct"
"#;

struct Harness {
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

/// Usage settings that reach nothing real.
fn safe_usage(dir: &Path) -> UsageSettings {
    UsageSettings {
        poll: false,
        claude_base_url: "http://127.0.0.1:1".to_owned(),
        security_bin: None,
        keychain_user: None,
        claude_home: Some(dir.join("no-claude-home")),
        codex_bin: Some(dir.join("no-codex")),
        codex_home: Some(dir.join("no-codex-home")),
        ..UsageSettings::default()
    }
}

async fn harness_with(tweak: impl FnOnce(&Path, &mut UsageSettings)) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let mut claude = AgentConfig::default_for(AgentKind::Claude);
    claude.bin = write_script(&bin, "claude", FAKE_CLAUDE);
    let mut usage = safe_usage(dir.path());
    tweak(dir.path(), &mut usage);
    let cfg = ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        home: dir.path().join("home"),
        agents: HashMap::from([(AgentKind::Claude, claude)]),
        default_agent: AgentKind::Claude,
        concurrency: 1,
        resources: hashi::guard::ResourceSettings::with_probe(Arc::new(
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
        account_cooldown: Duration::from_secs(3600),
        usage,
    };
    let running = server::start(cfg, Bus::default()).await.unwrap();
    let base = running.state.runtime.local_url();
    let admin = running.bootstrap_token.clone().unwrap();
    Harness {
        dir,
        running,
        base,
        admin,
        http: reqwest::Client::new(),
    }
}

async fn harness() -> Harness {
    harness_with(|_, _| {}).await
}

impl Harness {
    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn store(&self) -> &hashi::store::Store {
        &self.running.state.store
    }

    fn add_login(&self, name: &str) -> PathBuf {
        let dir = hashi::accounts::create_dir(&self.home(), AgentKind::Claude, name).unwrap();
        std::fs::write(dir.join("name"), name).unwrap();
        self.store()
            .add_account(AgentKind::Claude, name, hashi::accounts::AccountKind::Login)
            .unwrap();
        hashi::accounts::account_dir(&self.home(), AgentKind::Claude, name)
    }

    /// Sets an account's five-hour window to `used` percent.
    fn set_used(&self, name: &str, used: f64) {
        self.store()
            .replace_usage(
                AgentKind::Claude,
                name,
                &[Window::new(
                    "five_hour",
                    "5 hours",
                    used,
                    Some(now_ms() + 3_600_000),
                    Some(300),
                    Source::Poll,
                )],
            )
            .unwrap();
    }

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

    async fn get(&self, path: &str, token: &str) -> (StatusCode, Value) {
        self.call(reqwest::Method::GET, path, token, None).await
    }

    async fn post(&self, path: &str, token: &str, body: Value) -> (StatusCode, Value) {
        self.call(reqwest::Method::POST, path, token, Some(body))
            .await
    }

    async fn token(&self, body: Value) -> String {
        let (status, v) = self.post("/v1/tokens", &self.admin, body).await;
        assert_eq!(status, StatusCode::CREATED, "{v}");
        v["token"].as_str().unwrap().to_owned()
    }

    async fn ask(&self, token: &str, message: &str) -> Value {
        let (status, v) = self
            .post("/v1/ask", token, json!({"message": message, "wait": 10}))
            .await;
        assert!(status.is_success(), "{v}");
        v
    }

    async fn answer(&self, token: &str, message: &str) -> String {
        let v = self.ask(token, message).await;
        assert_eq!(v["status"], "completed", "{v}");
        v["response"].as_str().unwrap().to_owned()
    }

    async fn account(&self, name: &str) -> Value {
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

    fn project_dir(&self) -> String {
        let p = self.dir.path().join("project");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::canonicalize(p).unwrap().display().to_string()
    }
}

#[tokio::test]
async fn in_order_skips_an_account_over_the_threshold() {
    let h = harness().await;
    h.add_login("work");
    h.add_login("personal");
    let token = h
        .token(json!({"name": "a", "accounts": {"claude": ["work", "personal"]}}))
        .await;
    h.set_used("work", 85.0);
    h.set_used("personal", 10.0);
    assert_eq!(h.answer(&token, "1").await, "1 on work (resume=no)");
    h.set_used("work", 90.0);
    assert_eq!(h.answer(&token, "2").await, "2 on personal (resume=no)");
    // Every account over: the most headroom runs rather than none.
    h.set_used("personal", 97.0);
    assert_eq!(h.answer(&token, "3").await, "3 on work (resume=no)");
    h.running.shutdown().await;
}

#[tokio::test]
async fn a_project_sets_its_own_strategy_and_threshold() {
    let h = harness().await;
    h.add_login("work");
    h.add_login("personal");
    h.set_used("work", 70.0);
    h.set_used("personal", 20.0);
    let (status, v) = h
        .post(
            "/v1/projects",
            &h.admin,
            json!({
                "name": "api",
                "path": h.project_dir(),
                "agents": ["claude"],
                "accounts": {"claude": ["work", "personal"]},
                "accountStrategy": "most-left",
                "rotateAt": 80,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    assert_eq!(v["project"]["accountStrategy"], "most-left");
    assert_eq!(v["project"]["rotateAt"], 80);
    let bound = h
        .token(json!({"name": "b", "agents": ["claude"], "project": "api"}))
        .await;
    assert_eq!(h.answer(&bound, "x").await, "x on personal (resume=no)");

    // Back to the server's in-order: the first under its 90%.
    let (status, v) = h
        .call(
            reqwest::Method::PATCH,
            "/v1/projects/api",
            &h.admin,
            Some(json!({"accountStrategy": null})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["project"]["accountStrategy"], Value::Null);
    assert_eq!(h.answer(&bound, "y").await, "y on work (resume=no)");
    // The project's 80% still applies: work at 82% is skipped.
    h.set_used("work", 82.0);
    assert_eq!(h.answer(&bound, "z").await, "z on personal (resume=no)");

    let (status, v) = h
        .call(
            reqwest::Method::PATCH,
            "/v1/projects/api",
            &h.admin,
            Some(json!({"rotateAt": 30})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    let (status, _) = h
        .call(
            reqwest::Method::PATCH,
            "/v1/projects/api",
            &h.admin,
            Some(json!({"accountStrategy": "round-robin"})),
        )
        .await;
    assert!(status.is_client_error());
    h.running.shutdown().await;
}

#[tokio::test]
async fn the_server_default_strategy_applies_without_a_project() {
    let h = harness_with(|_, u| {
        u.policy = Policy {
            strategy: Strategy::MostLeft,
            rotate_at: 90,
        };
    })
    .await;
    h.add_login("work");
    h.add_login("personal");
    h.set_used("work", 60.0);
    h.set_used("personal", 30.0);
    let token = h
        .token(json!({"name": "a", "accounts": {"claude": ["work", "personal"]}}))
        .await;
    assert_eq!(h.answer(&token, "m").await, "m on personal (resume=no)");
    let (_, v) = h.get("/v1/accounts", &h.admin).await;
    assert_eq!(
        (v["strategy"].clone(), v["rotateAt"].clone()),
        (json!("most-left"), json!(90))
    );
    h.running.shutdown().await;
}

#[tokio::test]
async fn a_conversation_stays_on_its_account_until_the_threshold() {
    let h = harness().await;
    h.add_login("work");
    h.add_login("personal");
    let token = h
        .token(json!({
            "name": "s",
            "sessionMode": "stateful",
            "accounts": {"claude": ["work", "personal"]},
        }))
        .await;
    assert_eq!(h.answer(&token, "1").await, "1 on work (resume=no)");
    h.set_used("work", 95.0);
    // Over the threshold: the next message starts fresh elsewhere.
    assert_eq!(h.answer(&token, "2").await, "2 on personal (resume=no)");
    h.set_used("work", 10.0);
    // In-order alone would go back to work; the conversation stays.
    assert_eq!(
        h.answer(&token, "3").await,
        "3 on personal (resume=sess-personal)"
    );
    // Admins see which account it is on.
    let (_, v) = h.get("/v1/tokens", &h.admin).await;
    let t = v["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "s")
        .unwrap();
    assert_eq!(t["conversations"][0]["account"], "personal", "{t}");
    assert_eq!(t["conversations"][0]["agent"], "claude");
    // Moving back does not resume the old conversation on work.
    h.set_used("personal", 92.0);
    assert_eq!(h.answer(&token, "4").await, "4 on work (resume=no)");
    assert_eq!(h.answer(&token, "5").await, "5 on work (resume=sess-work)");
    h.running.shutdown().await;
}

#[tokio::test]
async fn a_new_conversation_starts_on_the_strategys_choice() {
    let h = harness_with(|_, u| u.policy.strategy = Strategy::MostLeft).await;
    h.add_login("work");
    h.add_login("personal");
    h.set_used("work", 40.0);
    h.set_used("personal", 10.0);
    let token = h
        .token(json!({
            "name": "s",
            "sessionMode": "stateful",
            "accounts": {"claude": ["work", "personal"]},
        }))
        .await;
    assert_eq!(h.answer(&token, "1").await, "1 on personal (resume=no)");
    // More left elsewhere does not move a conversation under the threshold.
    h.set_used("personal", 60.0);
    assert_eq!(
        h.answer(&token, "2").await,
        "2 on personal (resume=sess-personal)"
    );
    // A stateless token always follows the strategy.
    let plain = h
        .token(json!({"name": "p", "accounts": {"claude": ["work", "personal"]}}))
        .await;
    assert_eq!(h.answer(&plain, "3").await, "3 on work (resume=no)");
    h.running.shutdown().await;
}

#[tokio::test]
async fn rate_limit_events_from_a_run_are_recorded_and_published() {
    let h = harness().await;
    let work = h.add_login("work");
    std::fs::write(
        work.join("event"),
        format!(
            r#"{{"status":"allowed_warning","resetsAt":{},"rateLimitType":"five_hour","utilization":0.93}}"#,
            now_ms() / 1000 + 7_200
        ),
    )
    .unwrap();
    let mut events = h.running.state.bus.subscribe();
    let token = h
        .token(json!({"name": "a", "accounts": {"claude": ["work"]}}))
        .await;
    assert_eq!(h.answer(&token, "hi").await, "hi on work (resume=no)");
    let a = h.account("work").await;
    let usage = a["usage"].as_array().unwrap();
    assert_eq!(usage.len(), 1, "{a}");
    assert_eq!(usage[0]["key"], "five_hour");
    assert_eq!(usage[0]["source"], "run");
    assert_eq!(a["summary"]["headroom"], 7, "{a}");
    assert_eq!(a["summary"]["state"], "warning");
    assert_eq!(a["summary"]["source"], "run");
    assert_eq!(a["summary"]["binding"], "5 hours");
    assert!((a["summary"]["costToday"].as_f64().unwrap() - 0.25).abs() < 1e-9);
    assert_eq!(a["summary"]["tokensToday"], 7);

    let mut seen = None;
    for _ in 0..100 {
        match events.try_recv() {
            Ok(Event::AccountUsage { account, state, .. }) if account == "work" => {
                seen = Some(state);
                break;
            }
            Ok(_) => {}
            Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
    assert_eq!(seen, Some(UsageState::Warning));
    h.running.shutdown().await;
}

#[tokio::test]
async fn a_rejected_run_marks_the_account_until_its_reset_and_fails_over() {
    let h = harness().await;
    let work = h.add_login("work");
    h.add_login("personal");
    let reset = now_ms() / 1000 + 2 * 3_600;
    std::fs::write(work.join("rejected"), reset.to_string()).unwrap();
    let token = h
        .token(json!({"name": "a", "accounts": {"claude": ["work", "personal"]}}))
        .await;
    assert_eq!(h.answer(&token, "hi").await, "hi on personal (resume=no)");
    let w = h
        .store()
        .get_account(AgentKind::Claude, "work")
        .unwrap()
        .unwrap();
    // The event's reset, not the hour of cooldown.
    assert_eq!(w.limited_until, Some(reset * 1000));
    let a = h.account("work").await;
    assert_eq!(a["state"], "limited");
    assert_eq!(a["summary"]["state"], "limited");
    let sources: Vec<&str> = a["usage"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["source"].as_str().unwrap())
        .collect();
    assert!(
        sources.contains(&"run") && sources.contains(&"error"),
        "{a}"
    );

    // Recovering: a successful run forgets the error record.
    std::fs::remove_file(work.join("rejected")).unwrap();
    h.store()
        .mark_account_used(AgentKind::Claude, "work", true)
        .unwrap();
    h.store()
        .replace_usage(AgentKind::Claude, "work", &[])
        .unwrap();
    assert_eq!(h.answer(&token, "again").await, "again on work (resume=no)");
    assert_eq!(h.account("work").await["summary"]["state"], "unknown");
    h.running.shutdown().await;
}

#[tokio::test]
async fn usage_is_hidden_from_token_holders() {
    let h = harness().await;
    h.add_login("work");
    h.set_used("work", 40.0);
    let token = h
        .token(json!({"name": "a", "accounts": {"claude": ["work"]}}))
        .await;
    let (status, _) = h.get("/v1/accounts", &token).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = h
        .post("/v1/accounts/claude/work/refresh", &token, json!({}))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, me) = h.get("/v1/me", &token).await;
    assert_eq!(status, StatusCode::OK);
    let me = me.to_string();
    for word in [
        "usedPercent",
        "headroom",
        "five_hour",
        "\"work\"",
        "resetsAt",
    ] {
        assert!(!me.contains(word), "{word} in {me}");
    }
    let v = h.ask(&token, "hi").await;
    let text = v.to_string();
    assert!(
        !text.contains("headroom") && !text.contains("usedPercent"),
        "{text}"
    );
    // The OpenAI model list carries no usage either.
    let (_, models) = h.get("/v1/models", &token).await;
    assert!(!models.to_string().contains("headroom"));
    h.running.shutdown().await;
}

/// A stand-in for the usage endpoint: answers `body`, counts calls.
async fn usage_endpoint(body: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let app = axum::Router::new().route(
        "/api/oauth/usage",
        axum::routing::get(move |headers: axum::http::HeaderMap| {
            let log = Arc::clone(&log);
            async move {
                let auth = headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_owned();
                log.lock().unwrap().push(auth);
                body
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

#[tokio::test]
async fn the_refresh_endpoint_reads_usage_once_a_minute() {
    let (base, seen) = usage_endpoint(
        r#"{"five_hour":{"utilization":12,"resets_at":"2099-01-01T00:00:00Z"},"seven_day":{"utilization":48,"resets_at":"2099-01-04T00:00:00Z"}}"#,
    )
    .await;
    let h = harness_with(|dir, u| {
        let security = write_script(
            dir,
            "security",
            r#"#!/bin/sh
printf '%s' '{"claudeAiOauth":{"accessToken":"tok-refresh","expiresAt":99999999999999,"subscriptionType":"pro"}}'
"#,
        );
        u.claude_base_url = base;
        u.security_bin = Some(security);
        u.keychain_user = Some("owner".to_owned());
    })
    .await;
    h.add_login("work");
    let (status, v) = h
        .post("/v1/accounts/claude/work/refresh", &h.admin, json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["ok"], true);
    assert_eq!(v["account"]["usage"].as_array().unwrap().len(), 2, "{v}");
    assert_eq!(v["account"]["summary"]["headroom"], 52);
    assert_eq!(v["account"]["summary"]["plan"], "pro");
    assert_eq!(v["account"]["summary"]["binding"], "week");
    assert!(!v.to_string().contains("tok-refresh"), "{v}");
    assert_eq!(seen.lock().unwrap().as_slice(), ["Bearer tok-refresh"]);

    let (status, v) = h
        .post("/v1/accounts/claude/work/refresh", &h.admin, json!({}))
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{v}");
    assert_eq!(seen.lock().unwrap().len(), 1);

    // API-key accounts and agents without live usage are refused.
    h.store()
        .add_account(
            AgentKind::Claude,
            "ci",
            hashi::accounts::AccountKind::ApiKey,
        )
        .unwrap();
    let (status, _) = h
        .post("/v1/accounts/claude/ci/refresh", &h.admin, json!({}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = h
        .post("/v1/accounts/gemini/default/refresh", &h.admin, json!({}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = h
        .post("/v1/accounts/copilot/x/refresh", &h.admin, json!({}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    h.running.shutdown().await;
}

#[tokio::test]
async fn a_default_account_with_usage_is_listed() {
    let h = harness().await;
    h.store()
        .replace_usage(
            AgentKind::Claude,
            "default",
            &[Window::new(
                "seven_day",
                "week",
                33.0,
                None,
                Some(10_080),
                Source::Poll,
            )],
        )
        .unwrap();
    let a = h.account("default").await;
    assert_eq!(a["kind"], "default", "{a}");
    assert_eq!(a["summary"]["headroom"], 67);
    h.running.shutdown().await;
}
