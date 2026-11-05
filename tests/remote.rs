//! `hashi connect`, `hashi remote` and `hashi ask --remote`, run as the real
//! binary against a local server with a fake agent. Nothing leaves loopback.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use hashi::agent::{AgentConfig, AgentKind, Limits};
use hashi::events::Bus;
use hashi::server::{self, Running, ServerConfig};
use serde_json::{Value, json};

const FAKE_CLAUDE: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "9.9.9 (Fake Claude)"; exit 0; fi
for last; do :; done
case "$last" in
  fail*) echo '{"type":"result","is_error":true,"subtype":"error_during_execution","result":"simulated failure"}'; exit 1 ;;
esac
printf '{"type":"result","is_error":false,"result":"claude says %s\\u001b[31m\\u202e","session_id":"s1","total_cost_usd":0.01,"usage":{"input_tokens":5,"output_tokens":7}}\n' "$last"
"#;

/// Stands in for `claude mcp add`: records its arguments, one per line.
const FAKE_CLAUDE_SETUP: &str = r#"#!/bin/sh
for a; do printf '%s\n' "$a"; done > "$(dirname "$0")/claude-args.txt"
"#;

fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

struct Harness {
    dir: tempfile::TempDir,
    running: Running,
    base: String,
    admin: String,
}

async fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let mut cfg = AgentConfig::default_for(AgentKind::Claude);
    cfg.bin = write_script(&bin, "claude", FAKE_CLAUDE);
    let agents = HashMap::from([(AgentKind::Claude, cfg)]);
    let cfg = ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        home: dir.path().join("server-home"),
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
    let running = server::start(cfg, Bus::default()).await.unwrap();
    let base = running.state.runtime.local_url.clone();
    let admin = running.bootstrap_token.clone().unwrap();
    Harness {
        dir,
        running,
        base,
        admin,
    }
}

impl Harness {
    fn friend_home(&self) -> PathBuf {
        self.dir.path().join("friend-home")
    }

    async fn invite(&self) -> String {
        let res = reqwest::Client::new()
            .post(format!("{}/v1/invites", self.base))
            .bearer_auth(&self.admin)
            .json(&json!({ "label": "friend" }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 201);
        let v: Value = res.json().await.unwrap();
        v["url"].as_str().unwrap().to_owned()
    }

    /// Runs the hashi binary off the async runtime, so the server keeps
    /// answering while it waits.
    async fn hashi(&self, args: &[&str], envs: &[(&str, &Path)]) -> Output {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_hashi"));
        cmd.arg("--home").arg(self.friend_home()).args(args);
        cmd.env_remove("HASHI_CLAUDE_BIN");
        for (k, v) in envs {
            cmd.env(k, v);
        }
        tokio::task::spawn_blocking(move || cmd.output().unwrap())
            .await
            .unwrap()
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_then_ask_a_remote() {
    let h = harness().await;
    let url = h.invite().await;

    let out = h.hashi(&["connect", &url], &[]).await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(stdout.contains("saved as \"test-hashi\""), "{stdout}");
    assert!(stdout.contains("claude mcp add --transport http test-hashi"));
    assert!(stdout.contains("hashi ask --remote test-hashi"));
    assert!(stdout.contains(&format!("{}/me", h.base)));

    let path = h.friend_home().join("remotes.json");
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let remote = &saved["remotes"][0];
    assert_eq!(remote["name"], "test-hashi");
    assert_eq!(remote["baseUrl"], h.base.as_str());
    let token = remote["token"].as_str().unwrap().to_owned();
    assert!(token.starts_with("hashi_"));
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);

    // The link is spent now, whichever way it was used.
    let again = h.hashi(&["connect", &url, "--name", "other"], &[]).await;
    assert!(!again.status.success());
    assert!(
        text(&again.stderr).contains("already used"),
        "{}",
        text(&again.stderr)
    );

    // List shows names and URLs, never the token.
    let list = h.hashi(&["remote", "list"], &[]).await;
    let listed = text(&list.stdout);
    assert!(listed.contains("test-hashi") && listed.contains(&h.base));
    assert!(!listed.contains(&token));

    // One saved remote: the name can be left out. Control and bidi
    // characters in the answer are neutralised.
    let ask = h.hashi(&["ask", "--remote", "hello there"], &[]).await;
    assert!(ask.status.success(), "{}", text(&ask.stderr));
    let answer = text(&ask.stdout);
    assert!(answer.contains("claude says hello there"), "{answer}");
    assert!(
        !answer.contains('\u{1b}') && !answer.contains('\u{202e}'),
        "{answer:?}"
    );
    assert!(text(&ask.stderr).contains("5 in"));

    let named = h
        .hashi(
            &["ask", "--remote", "test-hashi", "-a", "claude", "again"],
            &[],
        )
        .await;
    assert!(named.status.success(), "{}", text(&named.stderr));
    assert!(text(&named.stdout).contains("claude says again"));

    // A failed run prints the error and exits non-zero.
    let failed = h.hashi(&["ask", "--remote", "fail now"], &[]).await;
    assert!(!failed.status.success());
    assert!(
        text(&failed.stderr).contains("simulated failure"),
        "{}",
        text(&failed.stderr)
    );

    let unknown = h.hashi(&["ask", "--remote", "nope", "hi"], &[]).await;
    assert!(!unknown.status.success());

    let removed = h.hashi(&["remote", "remove", "test-hashi"], &[]).await;
    assert!(removed.status.success());
    let list = h.hashi(&["remote", "list"], &[]).await;
    assert!(text(&list.stdout).contains("No remotes saved"));

    h.running.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_can_set_up_claude_code() {
    let h = harness().await;
    let url = h.invite().await;
    let tools = h.dir.path().join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    let fake = write_script(&tools, "claude", FAKE_CLAUDE_SETUP);

    let out = h
        .hashi(
            &["connect", &url, "--name", "work", "--setup", "claude"],
            &[("HASHI_CLAUDE_BIN", &fake)],
        )
        .await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("added to Claude Code"));

    let saved = hashi::remote::load(&h.friend_home()).unwrap();
    let token = &saved[0].token;
    let args = std::fs::read_to_string(tools.join("claude-args.txt")).unwrap();
    let want = format!(
        "mcp\nadd\n--transport\nhttp\nwork\n{}/mcp\n--header\nAuthorization: Bearer {token}\n",
        h.base
    );
    assert_eq!(args, want);
    h.running.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_refuses_bad_links_without_calling_out() {
    let h = harness().await;
    for bad in [
        "http://example.com/claim/hashi_inv_0123456789abcdef",
        "https://example.com/claim/not-a-code",
    ] {
        let out = h.hashi(&["connect", bad], &[]).await;
        assert!(!out.status.success(), "{bad}");
    }
    assert!(!h.friend_home().join("remotes.json").exists());
    h.running.shutdown().await;
}
