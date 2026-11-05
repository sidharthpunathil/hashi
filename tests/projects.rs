//! Projects end to end: `hashi project` and `--project` on the real binary
//! with a throwaway home, and a real server running a fake agent that
//! prints where it runs. Only temporary folders are ever used as projects.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use hashi::agent::{AgentConfig, AgentKind, Limits};
use hashi::events::Bus;
use hashi::server::{self, Running, ServerConfig};
use reqwest::StatusCode;
use serde_json::{Value, json};

/// Answers with its physical working directory and its arguments.
const PWD_CLAUDE: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "9.9.9 (Fake Claude)"; exit 0; fi
printf '{"type":"result","is_error":false,"result":"%s|%s","session_id":"s-1"}\n' "$(pwd -P)" "$*"
"#;

fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A folder with one file in it, by its canonical path.
fn project_folder(root: &Path, name: &str) -> PathBuf {
    let dir = std::fs::canonicalize(root).unwrap().join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("README.md"), "keep me").unwrap();
    dir
}

/// Every entry under `dir`, sorted, for before and after comparisons.
fn listing(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

// ── CLI ────────────────────────────────────────────────────────────────────

struct Cli {
    dir: tempfile::TempDir,
    claude: PathBuf,
}

impl Cli {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let claude = write_script(dir.path(), "claude", "#!/bin/sh\nexit 0\n");
        Self { dir, claude }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn run(&self, args: &[&str]) -> Output {
        let missing = self.dir.path().join("missing-agent");
        Command::new(env!("CARGO_BIN_EXE_hashi"))
            .arg("--home")
            .arg(self.home())
            .args(args)
            .env("HASHI_CLAUDE_BIN", &self.claude)
            .env("HASHI_CODEX_BIN", &missing)
            .env("HASHI_GEMINI_BIN", &missing)
            .env("HASHI_OPENCODE_BIN", &missing)
            .env("HASHI_QWEN_BIN", &missing)
            .env("HASHI_CURSOR_BIN", &missing)
            .env("HASHI_NO_UPDATE_CHECK", "1")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "{args:?}: {text}");
        text
    }

    fn fails(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(!out.status.success(), "{args:?} should fail");
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    fn store(&self) -> hashi::store::Store {
        hashi::server::open_store(&self.home()).unwrap()
    }
}

#[test]
fn projects_are_validated_listed_updated_and_removed() {
    let cli = Cli::new();
    let api = project_folder(cli.dir.path(), "api");
    let api_s = api.to_str().unwrap();

    // Names.
    for bad in ["Api", "a_b", "default", "../x", &"a".repeat(33)] {
        let err = cli.fails(&["project", "add", bad, api_s]);
        assert!(
            err.contains("name") || err.contains("default"),
            "{bad}: {err}"
        );
    }
    // Paths.
    assert!(
        cli.fails(&["project", "add", "x", "relative/dir"])
            .contains("absolute")
    );
    let missing = api.join("missing");
    assert!(
        cli.fails(&["project", "add", "x", missing.to_str().unwrap()])
            .contains("does not exist")
    );
    let file = api.join("README.md");
    assert!(
        cli.fails(&["project", "add", "x", file.to_str().unwrap()])
            .contains("not a directory")
    );
    cli.ok(&["token", "list"]);
    let inside = cli.home().join("workspaces");
    std::fs::create_dir_all(&inside).unwrap();
    assert!(
        cli.fails(&["project", "add", "x", inside.to_str().unwrap()])
            .contains("inside the Hashi home")
    );
    // Accounts and models.
    assert!(
        cli.fails(&["project", "add", "x", api_s, "--account", "claude=ghost"])
            .contains("claude:ghost")
    );
    assert!(
        cli.fails(&["project", "add", "x", api_s, "--model", "claude=--yolo"])
            .contains("unsupported")
    );

    // A symlink is stored as the folder it points at.
    let link = std::fs::canonicalize(cli.dir.path())
        .unwrap()
        .join("api-link");
    std::os::unix::fs::symlink(&api, &link).unwrap();
    cli.ok(&["account", "add", "claude", "work"]);
    cli.ok(&["account", "add", "claude", "home"]);
    let out = cli.ok(&[
        "project",
        "add",
        "api",
        link.to_str().unwrap(),
        "--account",
        "claude=work,home",
        "--model",
        "claude=sonnet",
        "--note",
        "the api",
    ]);
    assert!(out.contains("hashi share <name> --project api"), "{out}");
    let p = cli.store().get_project("api").unwrap().unwrap();
    assert_eq!(p.path, api.to_string_lossy());
    // Default agents: only the installed ones.
    assert_eq!(p.agents, [AgentKind::Claude]);
    assert!(
        cli.fails(&["project", "add", "api", api_s])
            .contains("already exists")
    );

    let list = cli.ok(&["project", "list"]);
    assert!(
        list.contains("api")
            && list.contains(api_s)
            && list.contains("claude=work,home")
            && list.contains("claude=sonnet"),
        "{list}"
    );

    cli.ok(&[
        "project",
        "update",
        "api",
        "--agents",
        "claude,codex",
        "--default-agent",
        "claude",
    ]);
    let p = cli.store().get_project("api").unwrap().unwrap();
    assert_eq!(p.agents, [AgentKind::Claude, AgentKind::Codex]);
    assert_eq!(p.default_agent, Some(AgentKind::Claude));
    assert!(
        cli.fails(&["project", "update", "api"])
            .contains("nothing to update")
    );
    assert!(
        cli.fails(&["project", "update", "ghost", "--note", "x"])
            .contains("no project")
    );

    // Removing an unbound project never touches the folder.
    let before = listing(&api);
    let out = cli.ok(&["project", "remove", "api"]);
    assert!(out.contains("not touched"), "{out}");
    assert_eq!(listing(&api), before);
    assert_eq!(
        std::fs::read_to_string(api.join("README.md")).unwrap(),
        "keep me"
    );
    assert!(cli.store().get_project("api").unwrap().is_none());
    let audit: Vec<String> = cli
        .store()
        .list_audit(50)
        .unwrap()
        .into_iter()
        .map(|e| e.action)
        .collect();
    for action in ["project.add", "project.update", "project.remove"] {
        assert!(audit.iter().any(|a| a == action), "{action}: {audit:?}");
    }
}

#[test]
fn tokens_and_invites_bind_to_projects_without_widening_access() {
    let cli = Cli::new();
    let api = project_folder(cli.dir.path(), "api");
    cli.ok(&["account", "add", "claude", "work"]);
    cli.ok(&["account", "add", "claude", "home"]);
    cli.ok(&[
        "project",
        "add",
        "api",
        api.to_str().unwrap(),
        "--agents",
        "claude",
        "--account",
        "claude=work,home",
    ]);

    // A token's account list must stay inside the project's.
    let err = cli.fails(&[
        "token",
        "create",
        "bot",
        "--project",
        "api",
        "--account",
        "claude=work,default",
    ]);
    assert!(
        err.contains("claude:default") && err.contains("narrow"),
        "{err}"
    );
    assert!(
        cli.fails(&["token", "create", "bot", "--project", "ghost"])
            .contains("no project named")
    );
    assert!(
        cli.fails(&[
            "token",
            "create",
            "bot",
            "--project",
            "api",
            "--agents",
            "codex"
        ])
        .contains("allows none")
    );
    cli.ok(&[
        "token",
        "create",
        "bot",
        "--project",
        "api",
        "--account",
        "claude=home",
    ]);
    let store = cli.store();
    let bot = store
        .list_tokens()
        .unwrap()
        .into_iter()
        .find(|t| t.name == "bot")
        .unwrap();
    assert_eq!(bot.project.as_deref(), Some("api"));
    assert!(cli.ok(&["token", "list"]).contains("api"));

    // Updating the token keeps it inside too.
    assert!(
        cli.fails(&["token", "update", &bot.id, "--account", "claude=default"])
            .contains("claude:default")
    );
    // And the project cannot be narrowed under it.
    let err = cli.fails(&["project", "update", "api", "--account", "claude=work"]);
    assert!(err.contains("token bot"), "{err}");

    // Share binds the invite.
    let out = cli.ok(&["share", "carol", "--project", "api"]);
    assert!(out.contains("in project api"), "{out}");
    assert!(
        cli.fails(&[
            "share",
            "dave",
            "--project",
            "api",
            "--account",
            "claude=ghost"
        ])
        .contains("claude:ghost")
    );

    // Removing refuses while bound, and --unbind clears the bindings.
    let err = cli.fails(&["project", "remove", "api"]);
    assert!(err.contains("1 token(s) and 1 pending invite(s)") && err.contains("--unbind"));
    assert!(cli.store().get_project("api").unwrap().is_some());
    let out = cli.ok(&["project", "remove", "api", "--unbind"]);
    assert!(out.contains("unbound 1 token(s) and 1 invite(s)"), "{out}");
    let store = cli.store();
    assert_eq!(store.get_token(&bot.id).unwrap().unwrap().project, None);
    assert_eq!(store.list_invites().unwrap()[0].project, None);
    assert_eq!(
        std::fs::read_to_string(api.join("README.md")).unwrap(),
        "keep me"
    );

    // --clear-project puts a token back on its own workspace.
    cli.ok(&["project", "add", "blog", api.to_str().unwrap()]);
    // The token's claude=home is outside blog, which allows only the
    // default login, so it has to drop it to move there.
    assert!(
        cli.fails(&["token", "update", &bot.id, "--project", "blog"])
            .contains("claude:home")
    );
    cli.ok(&[
        "token",
        "update",
        &bot.id,
        "--project",
        "blog",
        "--clear-accounts",
    ]);
    assert_eq!(
        cli.store()
            .get_token(&bot.id)
            .unwrap()
            .unwrap()
            .project
            .as_deref(),
        Some("blog")
    );
    cli.ok(&["token", "update", &bot.id, "--clear-project"]);
    assert_eq!(
        cli.store().get_token(&bot.id).unwrap().unwrap().project,
        None
    );
}

// ── server ─────────────────────────────────────────────────────────────────

struct Harness {
    dir: tempfile::TempDir,
    running: Running,
    base: String,
    admin: String,
    http: reqwest::Client,
}

async fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let mut cfg = AgentConfig::default_for(AgentKind::Claude);
    cfg.bin = write_script(&bin, "claude", PWD_CLAUDE);
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

    async fn get(&self, path: &str, token: &str) -> (StatusCode, Value) {
        self.call(reqwest::Method::GET, path, Some(token), None)
            .await
    }

    async fn token(&self, body: Value) -> (String, String) {
        let (status, v) = self.post("/v1/tokens", &self.admin, body).await;
        assert_eq!(status, StatusCode::CREATED, "{v}");
        (
            v["token"].as_str().unwrap().to_owned(),
            v["record"]["id"].as_str().unwrap().to_owned(),
        )
    }

    async fn add_project(&self, body: Value) -> Value {
        let (status, v) = self.post("/v1/projects", &self.admin, body).await;
        assert_eq!(status, StatusCode::CREATED, "{v}");
        v
    }

    /// Asks claude and returns the fake's working directory and arguments.
    async fn ask(&self, token: &str, body: Value) -> (String, String) {
        let mut body = body;
        body["wait"] = json!(10);
        let (status, v) = self.post("/v1/ask", token, body).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        let text = v["response"].as_str().unwrap();
        let (cwd, args) = text.split_once('|').unwrap();
        (cwd.to_owned(), args.to_owned())
    }
}

#[tokio::test]
async fn project_runs_start_in_the_folder_with_the_project_model() {
    let h = harness().await;
    let folders = tempfile::tempdir().unwrap();
    let api = project_folder(folders.path(), "api");
    let before = listing(&api);
    let v = h
        .add_project(json!({
            "name": "api",
            "path": api,
            "models": { "claude": "sonnet" },
        }))
        .await;
    assert_eq!(v["project"]["path"], api.to_string_lossy().as_ref());
    assert_eq!(
        v["project"]["agents"],
        json!(["claude"]),
        "installed agents"
    );

    let (bound, bound_id) = h
        .token(json!({ "name": "bot", "project": "api", "sessionMode": "stateful" }))
        .await;
    let (cwd, args) = h.ask(&bound, json!({ "message": "where" })).await;
    assert_eq!(cwd, api.to_string_lossy(), "the run starts in the project");
    assert!(args.contains("--model sonnet"), "{args}");
    // A model in the request wins.
    let (_, args) = h
        .ask(&bound, json!({ "message": "where", "model": "opus" }))
        .await;
    assert!(
        args.contains("--model opus") && !args.contains("sonnet"),
        "{args}"
    );
    // The stateful session is kept in the project.
    let (_, args) = h.ask(&bound, json!({ "message": "again" })).await;
    assert!(args.contains("--resume s-1"), "{args}");
    let store = &h.running.state.store;
    assert_eq!(
        store
            .project_session(&bound_id, AgentKind::Claude, "default", "api")
            .unwrap()
            .as_deref(),
        Some("s-1")
    );
    assert!(
        store
            .session(&bound_id, AgentKind::Claude)
            .unwrap()
            .is_none()
    );

    // A token without a project still runs in its own workspace.
    let (free, free_id) = h.token(json!({ "name": "free" })).await;
    let (cwd, args) = h.ask(&free, json!({ "message": "where" })).await;
    assert!(
        cwd.ends_with(&format!("workspaces/{free_id}/claude")),
        "{cwd}"
    );
    assert!(!args.contains("--model"), "{args}");

    // Requests and stats carry the project.
    let (_, list) = h.get("/v1/projects", &h.admin).await;
    let p = &list["projects"][0];
    assert_eq!(p["name"], "api");
    assert_eq!(p["tokens"], 1);
    assert_eq!(p["stats"]["todayRequests"], 3);
    assert_eq!(p["stats"]["monthRequests"], 3);
    assert!(p["stats"]["todayCostUsd"].is_number());
    assert!(p["stats"]["monthCostUsd"].is_number());

    // Hashi wrote nothing into the folder, and revoking leaves it alone.
    assert_eq!(listing(&api), before);
    let (status, _) = h
        .call(
            reqwest::Method::DELETE,
            &format!("/v1/tokens/{bound_id}"),
            Some(&h.admin),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(api.is_dir());
    assert_eq!(listing(&api), before);
    assert_eq!(
        std::fs::read_to_string(api.join("README.md")).unwrap(),
        "keep me"
    );
    h.running.shutdown().await;
}

#[tokio::test]
async fn holders_see_only_the_project_name() {
    let h = harness().await;
    let folders = tempfile::tempdir().unwrap();
    let api = project_folder(folders.path(), "api");
    let secret_path = api.to_string_lossy().into_owned();
    h.running
        .state
        .store
        .add_account(
            AgentKind::Claude,
            "work",
            hashi::accounts::AccountKind::Login,
        )
        .unwrap();
    h.add_project(json!({
        "name": "api",
        "path": api,
        "accounts": { "claude": ["work", "default"] },
    }))
    .await;

    let (bound, _) = h
        .token(json!({ "name": "bot", "project": "api", "accounts": { "claude": ["default"] } }))
        .await;
    let (status, me) = h.get("/v1/me", &bound).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["project"], "api");
    let text = me.to_string();
    assert!(!text.contains(&secret_path), "{text}");
    assert!(
        !text.contains("work") && !text.contains("accounts"),
        "{text}"
    );

    let (status, inv) = h
        .post(
            "/v1/invites",
            &h.admin,
            json!({ "label": "carol", "project": "api" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{inv}");
    assert_eq!(inv["invite"]["project"], "api");
    let code = inv["url"].as_str().unwrap().rsplit('/').next().unwrap();
    let (status, preview) = h
        .call(
            reqwest::Method::POST,
            "/v1/claim/preview",
            None,
            Some(json!({ "code": code })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["project"], "api");
    let text = preview.to_string();
    assert!(!text.contains(&secret_path), "{text}");
    assert!(
        !text.contains("work") && !text.contains("accounts"),
        "{text}"
    );

    // The claimed token is bound to the project.
    let (status, claimed) = h
        .call(
            reqwest::Method::POST,
            "/v1/claim/redeem",
            None,
            Some(json!({ "code": code })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{claimed}");
    assert!(!claimed.to_string().contains(&secret_path));
    let (_, me) = h.get("/v1/me", claimed["token"].as_str().unwrap()).await;
    assert_eq!(me["project"], "api");

    // A request's error never shows the folder either.
    let (_, req) = h
        .post("/v1/ask", &bound, json!({ "message": "where", "wait": 10 }))
        .await;
    assert_eq!(req["project"], "api");
    h.running.shutdown().await;
}

#[tokio::test]
async fn project_api_is_admin_only_and_validates() {
    let h = harness().await;
    let folders = tempfile::tempdir().unwrap();
    let api = project_folder(folders.path(), "api");
    let (user, _) = h.token(json!({ "name": "u" })).await;

    let body = json!({ "name": "api", "path": api });
    for (method, path, body) in [
        (reqwest::Method::GET, "/v1/projects", None),
        (reqwest::Method::POST, "/v1/projects", Some(body.clone())),
        (
            reqwest::Method::PATCH,
            "/v1/projects/api",
            Some(json!({ "note": "x" })),
        ),
        (reqwest::Method::DELETE, "/v1/projects/api", None),
    ] {
        let (status, _) = h
            .call(method.clone(), path, Some(&user), body.clone())
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}");
        let (status, _) = h.call(method.clone(), path, None, body).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
    }

    for (bad, want) in [
        (json!({ "name": "Bad", "path": api }), "name"),
        (json!({ "name": "default", "path": api }), "default"),
        (
            json!({ "name": "x", "path": api.join("nope") }),
            "does not exist",
        ),
        (
            json!({ "name": "x", "path": api.join("README.md") }),
            "not a directory",
        ),
        (
            json!({ "name": "x", "path": h.dir.path().join("home") }),
            "inside the Hashi home",
        ),
        (
            json!({ "name": "x", "path": api, "agents": [] }),
            "at least one",
        ),
        (
            json!({ "name": "x", "path": api, "accounts": { "claude": ["ghost"] } }),
            "claude:ghost",
        ),
    ] {
        let (status, v) = h.post("/v1/projects", &h.admin, bad.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {v}");
        let msg = v["error"]["message"].as_str().unwrap();
        assert!(msg.contains(want), "{bad}: {msg}");
    }

    h.add_project(body.clone()).await;
    let (status, _) = h.post("/v1/projects", &h.admin, body).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Binding through the API refuses wider accounts and unknown projects.
    let (status, v) = h
        .post(
            "/v1/tokens",
            &h.admin,
            json!({ "name": "t", "project": "ghost" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    h.running
        .state
        .store
        .add_account(
            AgentKind::Claude,
            "work",
            hashi::accounts::AccountKind::Login,
        )
        .unwrap();
    let (status, v) = h
        .post(
            "/v1/tokens",
            &h.admin,
            json!({ "name": "t", "project": "api", "accounts": { "claude": ["work"] } }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("claude:work")
    );

    // PATCH binds and unbinds a token.
    let (_, id) = h.token(json!({ "name": "t" })).await;
    let token_path = format!("/v1/tokens/{id}");
    let patch = |body: Value| {
        h.call(
            reqwest::Method::PATCH,
            &token_path,
            Some(&h.admin),
            Some(body),
        )
    };
    let (status, v) = patch(json!({ "project": "api" })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["record"]["project"], "api");

    // Removal refuses while bound, unless asked to unbind.
    let (status, v) = h
        .call(
            reqwest::Method::DELETE,
            "/v1/projects/api",
            Some(&h.admin),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["error"]["code"], "project_in_use");
    let (status, v) = patch(json!({ "project": null })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["record"]["project"], Value::Null);
    patch(json!({ "project": "api" })).await;
    let (status, v) = h
        .call(
            reqwest::Method::DELETE,
            "/v1/projects/api?unbind=true",
            Some(&h.admin),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["unboundTokens"], 1);
    assert!(api.join("README.md").is_file());
    let (_, list) = h.get("/v1/projects", &h.admin).await;
    assert_eq!(list["projects"], json!([]));
    h.running.shutdown().await;
}
