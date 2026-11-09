//! The two ways to share, end to end on loopback: a bridge share whose
//! token stops at a share of the owner's quota, and a login share that
//! hands a key account over through a one-time link. No network beyond
//! 127.0.0.1, no real CLI, never the real `~/.hashi`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use hashi::accounts::AccountKind;
use hashi::agent::{AgentConfig, AgentKind, Limits};
use hashi::events::Bus;
use hashi::server::{self, Running, ServerConfig};
use hashi::usage::{Source, UsageSettings, Window};
use hashi::util::now_ms;
use reqwest::StatusCode;
use serde_json::{Value, json};

const CLAUDE: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "9.9.9 (Fake Claude)"; exit 0; fi
for last; do :; done
echo '{"type":"system","subtype":"init","session_id":"s"}'
printf '{"type":"result","subtype":"success","is_error":false,"result":"ok %s","session_id":"sess"}\n' "$last"
"#;

const KEY: &str = "sk-ant-oat01-shared-for-the-test";

struct Server {
    dir: tempfile::TempDir,
    running: Running,
    base: String,
    admin: String,
    http: reqwest::Client,
}

async fn server() -> Server {
    server_with(CLAUDE).await
}

async fn server_with(claude_script: &str) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = bin.join("claude");
    std::fs::write(&script, claude_script).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut claude = AgentConfig::default_for(AgentKind::Claude);
    claude.bin = script;
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
            timeout: Duration::from_secs(60),
            ..Limits::default()
        },
        rate_limit: 0,
        queue_limit: 100,
        retention_days: 14,
        tunnel: false,
        public_url: None,
        name: "Alice".to_owned(),
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
        (status, res.json().await.unwrap_or(Value::Null))
    }

    async fn get(&self, path: &str, token: &str) -> (StatusCode, Value) {
        let res = self
            .http
            .get(format!("{}{path}", self.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        let status = res.status();
        (status, res.json().await.unwrap_or(Value::Null))
    }

    /// Sets the default Claude account's 5-hour window to `used` percent.
    fn used(&self, used: f64, reset: i64) {
        self.store()
            .replace_usage(
                AgentKind::Claude,
                "default",
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

    /// A Claude OAuth-token account `bot` holding [`KEY`].
    fn add_key_account(&self) {
        hashi::accounts::ops::create(
            self.store(),
            &self.home(),
            AgentKind::Claude,
            "bot",
            AccountKind::OauthToken,
            Some(KEY),
            "test",
        )
        .unwrap();
    }
}

fn code_of(url: &str) -> String {
    url.rsplit('/').next().unwrap().to_owned()
}

async fn redeem(
    home: &Path,
    url: &str,
    name: &'static str,
) -> anyhow::Result<hashi::remote::Redemption> {
    let home = home.to_path_buf();
    let url = url.to_owned();
    tokio::task::spawn_blocking(move || hashi::remote::redeem(&home, &url, Some(name)))
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_token_stops_at_its_share_of_the_owners_quota() {
    let s = server().await;
    let (status, v) = s
        .post(
            "/v1/tokens",
            &s.admin,
            json!({"name": "carol", "agents": ["claude"], "limits": {"stopAtPercent": 70}}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    let token = v["token"].as_str().unwrap().to_owned();
    assert_eq!(v["record"]["limits"]["stopAtPercent"], 70);

    // Under the stop: it runs.
    let reset = now_ms() + 2 * 3_600_000;
    s.used(60.0, reset);
    let (status, v) = s
        .post("/v1/ask", &token, json!({"message": "hello", "wait": 20}))
        .await;
    assert!(status.is_success(), "{v}");
    assert_eq!(v["response"], "ok hello");

    // Past it: refused with when it opens again, and nothing queued.
    s.used(75.0, reset);
    let (status, v) = s.post("/v1/ask", &token, json!({"message": "again"})).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{v}");
    assert_eq!(v["error"]["code"], "limit_reached");
    assert_eq!(v["error"]["limit"], "stopAtPercent");
    let msg = v["error"]["message"].as_str().unwrap();
    assert!(msg.contains("stops at 70%") && msg.contains("75%"), "{msg}");
    assert_eq!(
        v["error"]["resetAt"],
        hashi::util::rfc3339(reset).unwrap().as_str()
    );

    // Out of range is refused.
    let (status, _) = s
        .post(
            "/v1/tokens",
            &s.admin,
            json!({"name": "x", "limits": {"stopAtPercent": 0}}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // An invite passes the stop on to the token it creates.
    let (status, inv) = s
        .post(
            "/v1/invites",
            &s.admin,
            json!({"label": "dave", "limits": {"stopAtPercent": 80}}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{inv}");
    let code = code_of(inv["url"].as_str().unwrap());
    let (_, preview) = s.post("/v1/claim/preview", "", json!({"code": code})).await;
    assert_eq!(preview["limits"]["stopAtPercent"], 80);
    let (status, claimed) = s.post("/v1/claim/redeem", "", json!({"code": code})).await;
    assert_eq!(status, StatusCode::CREATED, "{claimed}");
    assert_eq!(claimed["limits"]["stopAtPercent"], 80);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_login_share_hands_the_key_over_once_with_its_terms() {
    let owner = server().await;
    owner.add_key_account();
    let (status, inv) = owner
        .post(
            "/v1/invites",
            &owner.admin,
            json!({"label": "dana", "loginAccount": "claude:bot", "remindAt": 70,
                   "expiresInDays": 7}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{inv}");
    assert_eq!(inv["invite"]["loginAccount"], "claude:bot");
    assert_eq!(inv["invite"]["shareUsage"], true);
    let url = inv["url"].as_str().unwrap().to_owned();

    // The preview says what it is, never the key.
    let (_, preview) = owner
        .post("/v1/claim/preview", "", json!({"code": code_of(&url)}))
        .await;
    assert_eq!(preview["login"]["agent"], "claude");
    assert_eq!(preview["login"]["remindAt"], 70);
    assert!(!preview.to_string().contains(KEY));
    assert!(
        preview["notice"]
            .as_str()
            .unwrap()
            .contains("hands you the owner's Claude Code key")
    );

    // The person's Hashi adds it as an account, with the terms.
    let person = tempfile::tempdir().unwrap();
    let r = redeem(person.path(), &url, "alice").await.unwrap();
    let Some(hashi::remote::LoginAdded::Account { agent, name, .. }) = r.login else {
        panic!("expected an account, got {:?}", r.login)
    };
    assert_eq!((agent, name.as_str()), (AgentKind::Claude, "alice"));
    let store = hashi::server::open_store(person.path()).unwrap();
    let a = store
        .get_account(AgentKind::Claude, "alice")
        .unwrap()
        .unwrap();
    assert_eq!(a.kind, AccountKind::OauthToken);
    assert_eq!(a.shared_by.as_deref(), Some("alice"));
    assert_eq!(a.remind_at, Some(70));
    let left = a.share_until.unwrap() - now_ms();
    assert!((6 * 86_400_000..=7 * 86_400_000).contains(&left), "{left}");
    assert_eq!(
        hashi::accounts::read_key(person.path(), AgentKind::Claude, "alice").unwrap(),
        KEY
    );

    // It works once.
    let again = redeem(person.path(), &url, "alice2").await.unwrap_err();
    assert!(format!("{again:#}").contains("already used"), "{again:#}");

    // The token it made reads the owner's usage back, and is marked.
    let remote = hashi::remote::load(person.path())
        .unwrap()
        .into_iter()
        .find(|x| x.name == "alice")
        .unwrap();
    owner.used(72.0, now_ms() + 3_600_000);
    let (_, me) = owner.get("/v1/me", &remote.token).await;
    assert_eq!(me["agentUsage"]["claude"]["headroom"], 28, "{me}");
    let tokens = owner.store().list_tokens().unwrap();
    let t = tokens.iter().find(|t| t.name == "dana").unwrap();
    assert_eq!(t.login_account.as_deref(), Some("claude:bot"));
    assert_eq!(t.remind_at, Some(70));

    // The person's Hashi reads that usage for the shared account, and
    // reminds them once it crosses what they agreed.
    let store = Arc::new(store);
    let bus = Bus::default();
    let mut events = bus.subscribe();
    let hub = Arc::new(hashi::usage::Hub::default());
    let settings = UsageSettings {
        poll: false,
        claude_base_url: "http://127.0.0.1:1".to_owned(),
        security_bin: None,
        keychain_user: None,
        claude_home: Some(person.path().join("no-claude-home")),
        codex_home: Some(person.path().join("no-codex-home")),
        ..UsageSettings::default()
    };
    let poller = |store: &Arc<hashi::store::Store>| {
        hashi::usage::Poller::new(
            Arc::clone(store),
            bus.clone(),
            &hub,
            person.path().to_path_buf(),
            settings.clone(),
            None,
            &[AgentKind::Claude],
        )
    };
    owner.used(60.0, now_ms() + 3_600_000);
    let first = poller(&store);
    assert!(
        first
            .eligible()
            .contains(&(AgentKind::Claude, "alice".to_owned()))
    );
    first.poll(AgentKind::Claude, "alice").await.unwrap();
    let summary = store
        .account_summary(AgentKind::Claude, "alice", 90, now_ms())
        .unwrap();
    assert_eq!(summary.used().map(f64::round), Some(60.0));
    owner.used(74.0, now_ms() + 3_600_000);
    poller(&store)
        .poll(AgentKind::Claude, "alice")
        .await
        .unwrap();
    let mut logs = Vec::new();
    while let Ok(e) = events.try_recv() {
        if let hashi::events::Event::Log(m) = e {
            logs.push(m);
        }
    }
    let reminders: Vec<&String> = logs.iter().filter(|m| m.starts_with("reminder")).collect();
    assert_eq!(reminders.len(), 1, "{logs:?}");
    assert!(
        reminders[0].contains("74%") && reminders[0].contains("stop at 70%"),
        "{}",
        reminders[0]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_that_cannot_be_read_leaves_the_link_working() {
    let owner = server().await;
    owner.add_key_account();
    let (status, inv) = owner
        .post(
            "/v1/invites",
            &owner.admin,
            json!({"label": "erin", "loginAccount": "claude:bot"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{inv}");
    let code = code_of(inv["url"].as_str().unwrap());
    let key_file =
        hashi::accounts::account_dir(&owner.home(), AgentKind::Claude, "bot").join("key");
    let saved = std::fs::read(&key_file).unwrap();
    std::fs::remove_file(&key_file).unwrap();
    let (status, v) = owner
        .post("/v1/claim/redeem", "", json!({"code": code}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["error"]["code"], "login_unavailable");
    std::fs::write(&key_file, saved).unwrap();
    let (status, v) = owner
        .post("/v1/claim/redeem", "", json!({"code": code}))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    assert_eq!(v["login"]["key"], KEY);

    // Only key accounts can be handed over.
    let (status, v) = owner
        .post(
            "/v1/invites",
            &owner.admin,
            json!({"label": "x", "loginAccount": "claude:nobody"}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
}

#[test]
fn an_ended_share_switches_the_copy_off_with_a_way_back() {
    let store = hashi::store::Store::open_in_memory().unwrap();
    store
        .add_account(AgentKind::Claude, "alice", AccountKind::OauthToken)
        .unwrap();
    store
        .set_account_share(
            AgentKind::Claude,
            "alice",
            "alice",
            Some(70),
            Some(now_ms() - 1),
        )
        .unwrap();
    store
        .add_account(AgentKind::Claude, "bob", AccountKind::OauthToken)
        .unwrap();
    store
        .set_account_share(
            AgentKind::Claude,
            "bob",
            "bob",
            None,
            Some(now_ms() + 86_400_000),
        )
        .unwrap();
    let ended = hashi::share::end_expired_shares(&store, now_ms()).unwrap();
    assert_eq!(ended.len(), 1, "{ended:?}");
    let a = store
        .get_account(AgentKind::Claude, "alice")
        .unwrap()
        .unwrap();
    assert!(a.disabled);
    assert!(
        a.disabled_reason
            .unwrap()
            .contains("hashi account enable claude alice")
    );
    assert!(
        !store
            .get_account(AgentKind::Claude, "bob")
            .unwrap()
            .unwrap()
            .disabled
    );
    // Once off, it is not switched off again.
    assert!(
        hashi::share::end_expired_shares(&store, now_ms())
            .unwrap()
            .is_empty()
    );
}

const MEMBER_CLAUDE: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "9.9.9 (Fake Claude)"; exit 0; fi
for last; do :; done
echo '{"type":"system","subtype":"init","session_id":"s"}'
printf '{"type":"result","subtype":"success","is_error":false,"result":"member ran %s","session_id":"sess","total_cost_usd":0.5,"usage":{"input_tokens":5,"output_tokens":6}}\n' "$last"
"#;

async fn wait_for(mut check: impl FnMut() -> bool, what: &str) {
    for _ in 0..150 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pool_member_serves_prompts_from_its_own_machine_under_its_own_limits() {
    let host = server().await;
    let member = server_with(MEMBER_CLAUDE).await;
    let (_, code) = host.store().open_pool_link(2, 10).unwrap();
    let link = format!("{}/pool/{code}", host.base);

    // The member joins, offering Claude Code and stopping at 70%.
    let joined = {
        let store = Arc::clone(&member.running.state.store);
        let home = member.home();
        let link = link.clone();
        tokio::task::spawn_blocking(move || {
            hashi::pool_client::join(
                &store,
                &home,
                &hashi::pool_client::JoinOptions {
                    link,
                    name: "sid".to_owned(),
                    agents: vec![AgentKind::Claude],
                    accounts: Default::default(),
                    stop_at: Some(70),
                    hours: Some(3),
                },
            )
        })
        .await
        .unwrap()
        .unwrap()
    };
    assert_eq!(joined.name, "sid");
    let local = member
        .store()
        .get_token(&joined.local_token_id)
        .unwrap()
        .unwrap();
    assert_eq!(local.limits.stop_at_percent, Some(70));
    assert!(local.expires_at.is_some());

    // Its worker comes online, with its usage at one endpoint.
    member.used(40.0, now_ms() + 3_600_000);
    let hub = Arc::clone(host.running.state.dispatcher.pool());
    let id = host.store().list_pool_members().unwrap()[0].id.clone();
    wait_for(|| hub.online(&id), "the member's worker").await;
    let (status, pool) = host.get("/v1/pool", &joined.token).await;
    assert_eq!(status, StatusCode::OK, "{pool}");
    assert_eq!(pool["members"][0]["name"], "sid");
    assert_eq!(pool["members"][0]["online"], true);
    // The usage arrives with the next poll.
    let (status, _) = host.get("/v1/pool", "hashi_not_a_token").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A prompt through the pool runs on the member's machine.
    let (status, v) = host
        .post(
            "/v1/ask",
            &joined.token,
            json!({"message": "hello pool", "agent": "claude", "wait": 30}),
        )
        .await;
    assert!(status.is_success(), "{v}");
    assert_eq!(v["response"], "member ran hello pool", "{v}");
    assert_eq!(v["usage"]["outputTokens"], 6, "{v}");
    // It went through the member's own queue, with its pool token.
    let ran = member
        .store()
        .recent_requests(10)
        .unwrap()
        .into_iter()
        .filter(|r| r.token_id == joined.local_token_id)
        .count();
    assert_eq!(ran, 1);

    // Past the member's own stop, the member says limited and nothing runs.
    member.used(80.0, now_ms() + 3_600_000);
    let (_, v) = host
        .post(
            "/v1/ask",
            &joined.token,
            json!({"message": "again", "agent": "claude", "wait": 30}),
        )
        .await;
    assert_eq!(v["status"], "failed", "{v}");
    assert!(v["error"].as_str().unwrap().contains("at its limit"), "{v}");

    // Leaving takes the member and its accounts out.
    {
        let store = Arc::clone(&member.running.state.store);
        let home = member.home();
        tokio::task::spawn_blocking(move || hashi::pool_client::leave(&store, &home, None))
            .await
            .unwrap()
            .unwrap();
    }
    assert!(host.store().list_pool_members().unwrap().is_empty());
    assert!(
        member
            .store()
            .get_token(&joined.local_token_id)
            .unwrap()
            .is_none()
    );
}
