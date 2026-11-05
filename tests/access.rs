//! Access requests end to end: a real server on a random port, fake agents,
//! and the friend's side driven both over HTTP and through `hashi request`.
//! Nothing here touches the network beyond loopback.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hashi::agent::{AgentConfig, AgentKind, Limits};
use hashi::events::{Bus, Event};
use hashi::server::{self, Running, ServerConfig};
use hashi::store::{AccessKind, MoreAsk, NewAccessRequest};
use reqwest::StatusCode;
use serde_json::{Value, json};

const FAKE_CLAUDE: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "9.9.9 (Fake Claude)"; exit 0; fi
for last; do :; done
printf '{"type":"result","is_error":false,"result":"claude says %s","session_id":"s1","usage":{"input_tokens":1,"output_tokens":1}}\n' "$last"
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

async fn harness_with(tweak: impl FnOnce(&mut ServerConfig)) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let mut agents = HashMap::new();
    for kind in [AgentKind::Claude, AgentKind::Codex] {
        let mut cfg = AgentConfig::default_for(kind);
        cfg.bin = write_script(&bin, kind.as_str(), FAKE_CLAUDE);
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
        account_cooldown: Duration::from_secs(3600),
        usage: Default::default(),
    };
    tweak(&mut cfg);
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
    harness_with(|_| {}).await
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

    async fn post(&self, path: &str, token: Option<&str>, body: Value) -> (StatusCode, Value) {
        self.call(reqwest::Method::POST, path, token, Some(body))
            .await
    }

    async fn get(&self, path: &str, token: Option<&str>) -> (StatusCode, Value) {
        self.call(reqwest::Method::GET, path, token, None).await
    }

    /// Opens the link and returns its code.
    async fn open_link(&self, body: Value) -> String {
        let (status, v) = self.post("/v1/request-link", Some(&self.admin), body).await;
        assert_eq!(status, StatusCode::CREATED, "{v}");
        let url = v["url"].as_str().unwrap();
        assert!(url.starts_with(&format!("{}/ask-access/hashi_ask_", self.base)));
        url.rsplit('/').next().unwrap().to_owned()
    }

    async fn ask(&self, code: &str, name: &str) -> (StatusCode, Value) {
        self.post(
            "/v1/ask-access",
            None,
            json!({ "code": code, "name": name, "message": "please", "agents": ["claude"] }),
        )
        .await
    }

    async fn user_token(&self, extra: Value) -> (String, String) {
        let mut body = json!({ "name": "friend" });
        if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
            b.extend(e.clone());
        }
        let (status, v) = self.post("/v1/tokens", Some(&self.admin), body).await;
        assert_eq!(status, StatusCode::CREATED, "{v}");
        (
            v["token"].as_str().unwrap().to_owned(),
            v["record"]["id"].as_str().unwrap().to_owned(),
        )
    }

    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.dir.path().join("home").join("hashi.db")).unwrap()
    }

    async fn audit_actions(&self) -> Vec<String> {
        let (_, v) = self.get("/v1/audit", Some(&self.admin)).await;
        v["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["action"].as_str().unwrap().to_owned())
            .collect()
    }
}

#[tokio::test]
async fn new_person_flow_from_open_to_claim() {
    let h = harness().await;
    let mut events = h.running.state.bus.subscribe();

    // Off by default.
    let (_, v) = h.get("/v1/request-link", Some(&h.admin)).await;
    assert_eq!(v["open"], false);

    let code = h
        .open_link(json!({ "note": "for the docs team", "hours": 24 }))
        .await;
    assert_eq!(code.len(), "hashi_ask_".len() + 32);
    let (status, p) = h
        .post("/v1/ask-access/preview", None, json!({ "code": code }))
        .await;
    assert_eq!(status, StatusCode::OK, "{p}");
    assert_eq!(p["server"], "Test Hashi");
    assert_eq!(p["note"], "for the docs team");
    assert!(p["agents"].as_array().unwrap().contains(&json!("claude")));

    let (status, r) = h.ask(&code, "Alice").await;
    assert_eq!(status, StatusCode::CREATED, "{r}");
    let pickup = r["pickup"].as_str().unwrap().to_owned();
    assert!(pickup.starts_with("hashi_pick_") && pickup.len() == "hashi_pick_".len() + 64);
    let id = r["id"].as_str().unwrap().to_owned();

    // The dashboard hears about it.
    let ev = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(Event::AccessRequest { id, kind, name }) = events.recv().await {
                return (id, kind, name);
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(ev, (id.clone(), AccessKind::New, "Alice".to_owned()));

    let (_, s) = h
        .post("/v1/ask-access/status", None, json!({ "pickup": pickup }))
        .await;
    assert_eq!(s["status"], "pending");
    assert!(s["claimUrl"].is_null());

    let (status, list) = h.get("/v1/access-requests", Some(&h.admin)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["pending"], 1);
    let req = &list["requests"][0];
    assert_eq!(
        (req["name"].as_str(), req["kind"].as_str()),
        (Some("Alice"), Some("new"))
    );
    assert_eq!(req["prefill"]["agents"], json!(["claude"]));
    // Never a secret or an address.
    let text = list.to_string();
    assert!(!text.contains(&pickup) && !text.contains("127.0.0.1"));

    let (status, a) = h
        .post(
            &format!("/v1/access-requests/{id}/accept"),
            Some(&h.admin),
            json!({ "limits": { "dailyRequests": 5 } }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{a}");
    assert_eq!(a["invite"]["label"], "Alice");
    assert_eq!(a["invite"]["limits"]["dailyRequests"], 5);
    assert!(!a.to_string().contains("hashi_inv_"), "{a}");

    let (_, s) = h
        .post("/v1/ask-access/status", None, json!({ "pickup": pickup }))
        .await;
    assert_eq!(s["status"], "accepted", "{s}");
    let claim = s["claimCode"].as_str().unwrap().to_owned();
    assert_eq!(
        s["claimUrl"].as_str().unwrap(),
        format!("{}/claim/{claim}", h.base)
    );

    let (status, t) = h
        .post("/v1/claim/redeem", None, json!({ "code": claim }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{t}");
    let token = t["token"].as_str().unwrap();
    let (status, me) = h.get("/v1/me", Some(token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (me["name"].as_str(), me["limits"]["dailyRequests"].as_u64()),
        (Some("Alice"), Some(5))
    );
    assert_eq!(me["agents"], json!(["claude"]));

    let (_, s) = h
        .post("/v1/ask-access/status", None, json!({ "pickup": pickup }))
        .await;
    assert_eq!(s["status"], "claimed");
    assert!(s["claimCode"].is_null());

    let actions = h.audit_actions().await;
    for want in [
        "access.link.open",
        "access.request",
        "access.accept",
        "invite.claim",
    ] {
        assert!(actions.iter().any(|a| a == want), "{want} in {actions:?}");
    }
    // Accepting twice is refused.
    let (status, _) = h
        .post(
            &format!("/v1/access-requests/{id}/accept"),
            Some(&h.admin),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    h.running.shutdown().await;
}

#[tokio::test]
async fn hashi_request_saves_the_remote_once_accepted() {
    let h = harness().await;
    let code = h.open_link(json!({})).await;
    let friend = tempfile::tempdir().unwrap();
    let home = friend.path().join("home");
    let url = format!("{}/ask-access/{code}", h.base);

    let opts = hashi::access_client::RequestOptions {
        url,
        name: "Bob".into(),
        message: Some("hello".into()),
        agents: Some(vec!["claude".into()]),
        remote_name: Some("owner".into()),
    };
    let home2 = home.clone();
    let pending =
        tokio::task::spawn_blocking(move || hashi::access_client::request(&home2, &opts).unwrap())
            .await
            .unwrap();
    let path = hashi::access_client::pending_path(&home);
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);

    let home2 = home.clone();
    let checked = tokio::task::spawn_blocking(move || hashi::access_client::check(&home2).unwrap())
        .await
        .unwrap();
    assert_eq!(checked.len(), 1);
    assert_eq!(checked[0].state, "pending");

    let (status, v) = h
        .post(
            &format!("/v1/access-requests/{}/accept", pending.id),
            Some(&h.admin),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");

    let home2 = home.clone();
    let checked = tokio::task::spawn_blocking(move || hashi::access_client::check(&home2).unwrap())
        .await
        .unwrap();
    assert_eq!(checked[0].state, "connected", "{checked:?}");
    assert_eq!(checked[0].detail, "owner");
    let remotes = hashi::remote::load(&home).unwrap();
    assert_eq!(remotes.len(), 1);
    assert_eq!(
        (remotes[0].name.as_str(), remotes[0].base_url.as_str()),
        ("owner", h.base.as_str())
    );
    let (status, me) = h.get("/v1/me", Some(&remotes[0].token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["name"], "Bob");
    assert!(!path.exists(), "the pending file is removed once empty");
    h.running.shutdown().await;
}

#[tokio::test]
async fn decline_with_a_reason_reaches_the_requester() {
    let h = harness().await;
    let code = h.open_link(json!({})).await;
    let (_, r) = h.ask(&code, "Carol").await;
    let id = r["id"].as_str().unwrap();
    let pickup = r["pickup"].as_str().unwrap();
    let (status, v) = h
        .post(
            &format!("/v1/access-requests/{id}/decline"),
            Some(&h.admin),
            json!({ "reason": "only the team for now" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (_, s) = h
        .post("/v1/ask-access/status", None, json!({ "pickup": pickup }))
        .await;
    assert_eq!(s["status"], "declined");
    assert_eq!(s["reason"], "only the team for now");
    assert!(s["claimCode"].is_null());
    assert!(
        h.audit_actions()
            .await
            .iter()
            .any(|a| a == "access.decline")
    );
    let (status, _) = h
        .post(
            &format!("/v1/access-requests/{id}/accept"),
            Some(&h.admin),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    h.running.shutdown().await;
}

#[tokio::test]
async fn closed_and_expired_links_take_no_requests() {
    let h = harness().await;
    let code = h.open_link(json!({})).await;
    let (status, v) = h
        .call(
            reqwest::Method::DELETE,
            "/v1/request-link",
            Some(&h.admin),
            None,
        )
        .await;
    assert_eq!(
        (status, v["closed"].as_bool()),
        (StatusCode::OK, Some(true))
    );
    let (status, v) = h.ask(&code, "Dan").await;
    assert_eq!(status, StatusCode::GONE);
    assert_eq!(v["error"]["code"], "link_closed");
    let (status, _) = h
        .post("/v1/ask-access/preview", None, json!({ "code": code }))
        .await;
    assert_eq!(status, StatusCode::GONE);

    // A link past its hours closes by itself.
    let code = h.open_link(json!({ "hours": 1 })).await;
    h.db()
        .execute(
            "UPDATE access_links SET expires_at = 1 WHERE closed_at IS NULL",
            [],
        )
        .unwrap();
    let (status, v) = h.ask(&code, "Dan").await;
    assert_eq!(
        (status, v["error"]["code"].as_str()),
        (StatusCode::GONE, Some("link_closed"))
    );
    let (_, v) = h.get("/v1/request-link", Some(&h.admin)).await;
    assert_eq!(
        (v["open"].as_bool(), v["link"]["status"].as_str()),
        (Some(false), Some("expired"))
    );

    // An unknown code is not found.
    let (status, v) = h
        .ask("hashi_ask_00000000000000000000000000000000", "Dan")
        .await;
    assert_eq!(
        (status, v["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("link_not_found"))
    );

    // A request left undecided for a week expires.
    let code = h.open_link(json!({})).await;
    let (_, r) = h.ask(&code, "Eve").await;
    h.db()
        .execute("UPDATE access_requests SET expires_at = 1", [])
        .unwrap();
    let (_, s) = h
        .post(
            "/v1/ask-access/status",
            None,
            json!({ "pickup": r["pickup"] }),
        )
        .await;
    assert_eq!(s["status"], "expired");
    let (status, _) = h
        .post(
            &format!("/v1/access-requests/{}/accept", r["id"].as_str().unwrap()),
            Some(&h.admin),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    h.running.shutdown().await;
}

#[tokio::test]
async fn submissions_are_limited_per_address() {
    let h = harness_with(|c| c.rate_limit = 2).await;
    let code = h.open_link(json!({})).await;
    assert_eq!(h.ask(&code, "a").await.0, StatusCode::CREATED);
    assert_eq!(h.ask(&code, "b").await.0, StatusCode::CREATED);
    let (status, v) = h.ask(&code, "c").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{v}");
    // Guessing pickup secrets is throttled like guessing invite codes.
    let mut last = StatusCode::OK;
    for _ in 0..4 {
        last = h
            .post(
                "/v1/ask-access/status",
                None,
                json!({ "pickup": "hashi_pick_nope" }),
            )
            .await
            .0;
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
    h.running.shutdown().await;
}

#[tokio::test]
async fn pending_caps_per_link_and_in_total() {
    let h = harness().await;
    let code = h.open_link(json!({})).await;
    let (_, v) = h.get("/v1/request-link", Some(&h.admin)).await;
    let link_id = v["link"]["id"].as_str().unwrap().to_owned();
    let store = &h.running.state.store;
    let fill = |link: &str, n: usize| {
        for i in 0..n {
            store
                .insert_access_request(&NewAccessRequest {
                    kind: AccessKind::New,
                    link_id: Some(link.to_owned()),
                    token_id: None,
                    name: format!("p{i}"),
                    message: String::new(),
                    agents: vec![AgentKind::Claude],
                    project: None,
                    asked: MoreAsk::default(),
                })
                .unwrap();
        }
    };
    fill(&link_id, hashi::store::MAX_PENDING_PER_LINK);
    let (status, v) = h.ask(&code, "late").await;
    assert_eq!(
        (status, v["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("link_full"))
    );
    fill("other-1", 50);
    fill("other-2", 50);
    fill("other-3", 50);
    let code = h.open_link(json!({})).await;
    let (status, v) = h.ask(&code, "late").await;
    assert_eq!(
        (status, v["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("requests_full"))
    );
    let (_, list) = h.get("/v1/access-requests", Some(&h.admin)).await;
    assert_eq!(list["pending"], hashi::store::MAX_PENDING_TOTAL);
    h.running.shutdown().await;
}

#[tokio::test]
async fn fields_are_limited_and_stripped_of_control_and_bidi_characters() {
    let h = harness().await;
    let code = h
        .open_link(json!({ "note": "hi\u{202e}there\u{1b}[2J" }))
        .await;
    let (_, p) = h
        .post("/v1/ask-access/preview", None, json!({ "code": code }))
        .await;
    assert_eq!(p["note"], "hithere[2J");
    let (status, r) = h
        .post(
            "/v1/ask-access",
            None,
            json!({
                "code": code,
                "name": "  Mal\u{202e}lory\u{2066}\u{200b}\u{7} ",
                "message": "line one\r\nline\u{1b}[31m two\u{2069}",
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{r}");
    let (_, list) = h.get("/v1/access-requests", Some(&h.admin)).await;
    let req = &list["requests"][0];
    assert_eq!(req["name"], "Mallory");
    assert_eq!(req["message"], "line one\nline[31m two");
    let text = list.to_string();
    for c in [
        '\u{202e}', '\u{2066}', '\u{2069}', '\u{200b}', '\u{1b}', '\u{7}',
    ] {
        assert!(!text.contains(c), "{c:?} in {text}");
    }
    // Lengths are checked after cleaning.
    for (name, message) in [
        ("\u{202e}\u{200b}".to_owned(), String::new()),
        ("a".repeat(61), String::new()),
        ("ok".to_owned(), "m".repeat(501)),
    ] {
        let (status, _) = h
            .post(
                "/v1/ask-access",
                None,
                json!({ "code": code, "name": name, "message": message }),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    // Only the link's agents can be asked for.
    let folder = tempfile::tempdir().unwrap();
    let (status, v) = h
        .post(
            "/v1/projects",
            Some(&h.admin),
            json!({ "name": "docs", "path": folder.path(), "agents": ["codex"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    let code = h.open_link(json!({ "project": "docs" })).await;
    let (_, p) = h
        .post("/v1/ask-access/preview", None, json!({ "code": code }))
        .await;
    assert_eq!(p["agents"], json!(["codex"]));
    assert_eq!(p["project"], "docs");
    let (status, _) = h.ask(&code, "x").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    h.running.shutdown().await;
}

#[tokio::test]
async fn more_requests_apply_the_owners_edited_limits() {
    let h = harness().await;
    let (token, token_id) = h
        .user_token(
            json!({ "limits": { "dailyRequests": 10, "maxPending": 2 }, "agents": ["claude"] }),
        )
        .await;
    let (status, r) = h
        .post(
            "/v1/me/requests",
            Some(&token),
            json!({ "dailyRequests": 100, "monthlyRequests": 2000, "agents": ["codex"], "message": "big week" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{r}");
    let id = r["request"]["id"].as_str().unwrap().to_owned();
    // One an hour.
    let (status, v) = h
        .post(
            "/v1/me/requests",
            Some(&token),
            json!({ "message": "again" }),
        )
        .await;
    assert_eq!(
        (status, v["error"]["code"].as_str()),
        (StatusCode::TOO_MANY_REQUESTS, Some("request_too_soon"))
    );

    let (_, list) = h.get("/v1/access-requests", Some(&h.admin)).await;
    let req = &list["requests"][0];
    assert_eq!(req["kind"], "more");
    assert_eq!(req["tokenId"], token_id.as_str());
    assert_eq!(req["prefill"]["current"]["limits"]["dailyRequests"], 10);
    assert_eq!(req["prefill"]["limits"]["dailyRequests"], 100);
    assert_eq!(req["prefill"]["agents"], json!(["claude", "codex"]));

    // The owner edits the daily cap down and keeps Claude only.
    let (status, a) = h
        .post(
            &format!("/v1/access-requests/{id}/accept"),
            Some(&h.admin),
            json!({ "limits": { "dailyRequests": 50 }, "agents": ["claude"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{a}");
    let (_, me) = h.get("/v1/me", Some(&token)).await;
    assert_eq!(me["limits"]["dailyRequests"], 50);
    assert_eq!(me["limits"]["monthlyRequests"], 2000);
    assert_eq!(me["limits"]["maxPending"], 2);
    assert_eq!(me["agents"], json!(["claude"]));
    let (_, mine) = h.get("/v1/me/requests", Some(&token)).await;
    assert_eq!(mine["requests"][0]["status"], "accepted");
    // New-person fields are refused for a token.
    h.db()
        .execute("UPDATE access_requests SET created_at = 1", [])
        .unwrap();
    let (_, r) = h
        .post(
            "/v1/me/requests",
            Some(&token),
            json!({ "message": "more" }),
        )
        .await;
    let (status, _) = h
        .post(
            &format!(
                "/v1/access-requests/{}/accept",
                r["request"]["id"].as_str().unwrap()
            ),
            Some(&h.admin),
            json!({ "label": "x" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    h.running.shutdown().await;
}

#[tokio::test]
async fn request_more_is_an_mcp_tool() {
    let h = harness().await;
    let (token, _) = h.user_token(json!({})).await;
    let rpc = |id: u64, method: &str, params: Value| json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    let (_, v) = h
        .post("/mcp", Some(&token), rpc(1, "tools/list", json!({})))
        .await;
    let tools: Vec<&str> = v["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(tools.contains(&"request_more"), "{tools:?}");
    let (_, v) = h
        .post(
            "/mcp",
            Some(&token),
            rpc(
                2,
                "tools/call",
                json!({ "name": "request_more", "arguments": { "monthlyBudgetUsd": 20, "message": "for a demo" } }),
            ),
        )
        .await;
    assert_eq!(v["result"]["isError"], false, "{v}");
    assert!(
        v["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("$20/month")
    );
    let (_, list) = h.get("/v1/access-requests", Some(&h.admin)).await;
    assert_eq!(list["requests"][0]["asked"]["monthlyBudgetUsd"], 20.0);
    assert_eq!(list["requests"][0]["message"], "for a demo");
    // A second one within the hour is a tool error, not a protocol error.
    let (_, v) = h
        .post(
            "/mcp",
            Some(&token),
            rpc(
                3,
                "tools/call",
                json!({ "name": "request_more", "arguments": { "message": "x" } }),
            ),
        )
        .await;
    assert_eq!(v["result"]["isError"], true, "{v}");
    h.running.shutdown().await;
}

#[tokio::test]
async fn owner_endpoints_are_admin_only() {
    let h = harness().await;
    let (user, _) = h.user_token(json!({})).await;
    let code = h.open_link(json!({})).await;
    let (_, r) = h.ask(&code, "Zed").await;
    let id = r["id"].as_str().unwrap();
    let cases = [
        (reqwest::Method::GET, "/v1/request-link".to_owned()),
        (reqwest::Method::POST, "/v1/request-link".to_owned()),
        (reqwest::Method::DELETE, "/v1/request-link".to_owned()),
        (reqwest::Method::GET, "/v1/access-requests".to_owned()),
        (
            reqwest::Method::POST,
            format!("/v1/access-requests/{id}/accept"),
        ),
        (
            reqwest::Method::POST,
            format!("/v1/access-requests/{id}/decline"),
        ),
    ];
    for (method, path) in cases {
        let (status, _) = h
            .call(method.clone(), &path, Some(&user), Some(json!({})))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}");
        let (status, _) = h.call(method.clone(), &path, None, Some(json!({}))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
    }
    // Still pending: nothing was decided.
    let (_, list) = h.get("/v1/access-requests", Some(&h.admin)).await;
    assert_eq!(list["requests"][0]["status"], "pending");
    // Admins have nobody to ask.
    let (status, _) = h
        .post("/v1/me/requests", Some(&h.admin), json!({ "message": "x" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    h.running.shutdown().await;
}

#[tokio::test]
async fn ask_access_page_has_strict_csp() {
    let h = harness().await;
    let res = h
        .http
        .get(format!("{}/ask-access/hashi_ask_whatever", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["content-security-policy"], hashi::web::CSP);
    let csp = hashi::web::CSP;
    assert!(csp.contains("default-src 'none'") && csp.contains("script-src 'self'"));
    assert!(csp.contains("form-action 'none'") && csp.contains("frame-ancestors 'none'"));
    assert_eq!(res.headers()["referrer-policy"], "no-referrer");
    assert_eq!(res.headers()["cache-control"], "no-store");
    assert_eq!(res.headers()["x-frame-options"], "DENY");
    let html = res.text().await.unwrap();
    assert!(html.contains("<script src=\"/assets/hashi.js\"></script>"));
    assert_eq!(html.matches("<script").count(), 1);
    assert!(!html.contains("style=") && !html.contains("onclick"));
    h.running.shutdown().await;
}
