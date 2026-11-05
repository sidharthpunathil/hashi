//! End-to-end tests for the opt-in Crush backend, against a fake `crush`
//! that logs its argv, environment, working directory and stdin, prints a
//! canned `session last --json`, and exits with the code a prompt asks for.

// Test helpers outside #[test] functions are not covered by clippy.toml's
// test exemptions; panicking on setup failure is the desired behaviour here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hashi::accounts::{self, AccountKind};
use hashi::agent::{self, AgentConfig, AgentKind, Invocation, Limits};
use hashi::events::Bus;
use hashi::server::{self, Running, ServerConfig};
use reqwest::StatusCode;
use serde_json::{Value, json};

/// The fake. Files next to it steer it: `version` (default v0.100.0). It
/// appends one block per call to `log` next to it. A run keeps its session
/// and running cost in the data directory, as Crush does.
const FAKE_CRUSH: &str = r#"#!/bin/sh
here=$(cd "$(dirname "$0")" && pwd)
log="$here/log"
version=v0.100.0
[ -f "$here/version" ] && version=$(cat "$here/version")
{
  echo "call"
  for a in "$@"; do printf 'arg:%s\n' "$a"; done
  printf 'cwd:%s\n' "$(pwd)"
  for v in HOME CRUSH_GLOBAL_CONFIG CRUSH_GLOBAL_DATA CRUSH_CACHE_DIR CRUSH_SKILLS_DIR \
           CRUSH_DISABLE_METRICS DO_NOT_TRACK CRUSH_DISABLE_PROVIDER_AUTO_UPDATE \
           ANTHROPIC_API_KEY OPENAI_API_KEY CRUSH_ANTHROPIC_API_KEY CRUSH_HOME \
           XDG_CONFIG_HOME CRUSH_CLIENT_SERVER HASHI_SECRET_PROBE; do
    eval "val=\${$v+set:\$$v}"
    if [ -n "$val" ]; then printf 'env:%s=%s\n' "$v" "${val#set:}"; fi
  done
  if [ -c /dev/stdin ]; then echo "stdin:char"; elif [ -p /dev/stdin ]; then echo "stdin:pipe"; else echo "stdin:other"; fi
} >> "$log"
if [ "$1" = "--version" ]; then echo "crush version $version"; exit 0; fi
data="$2"
acct=$(basename "$HOME")
session_json() {
  cents=$(cat "$data/cents" 2>/dev/null || echo 0)
  cost=$(awk "BEGIN{print $cents/100}")
  printf '{"meta":{"id":"hash-%s","uuid":"%s","title":"t","cost":%s,"prompt_tokens":1200,"completion_tokens":80,"total_tokens":1280},"messages":[]}\n' "$1" "$1" "$cost"
}
case "$3" in
  session)
    if [ "$4" = "last" ]; then
      [ -f "$data/session" ] || { echo "no sessions found" >&2; exit 1; }
      session_json "$(cat "$data/session")"; exit 0
    fi
    if [ "$4" = "show" ]; then session_json "$5"; exit 0; fi
    exit 1 ;;
  run) ;;
  *) exit 1 ;;
esac
printf 'config:%s\n' "$(tr -d '\n ' < "$data/crush.json")" >> "$log"
for last; do :; done
case "$last" in
  limit*) echo "agent processing failed: Rate limit exceeded: Too many requests, please slow down" >&2; exit 1 ;;
  fail*) echo "agent processing failed: Invalid API key" >&2; exit 1 ;;
esac
session=""
prev=""
for a in "$@"; do
  if [ "$prev" = "-s" ]; then session="$a"; fi
  prev="$a"
done
if [ -z "$session" ]; then
  n=$(( $(cat "$here/count" 2>/dev/null || echo 0) + 1 ))
  echo "$n" > "$here/count"
  session="sess-$acct-$n"
fi
echo "$session" > "$data/session"
cents=$(( $(cat "$data/cents" 2>/dev/null || echo 0) + 25 ))
echo "$cents" > "$data/cents"
echo "crush on $acct says: $last"
"#;

fn write_fake(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = bin.join("crush");
    std::fs::write(&script, FAKE_CRUSH).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

/// The calls the fake logged, each as its lines.
fn calls(bin: &Path) -> Vec<Vec<String>> {
    let log = std::fs::read_to_string(bin.parent().unwrap().join("log")).unwrap_or_default();
    let mut out: Vec<Vec<String>> = Vec::new();
    for line in log.lines() {
        if line == "call" {
            out.push(Vec::new());
        } else if let Some(last) = out.last_mut() {
            last.push(line.to_owned());
        }
    }
    out
}

fn args_of(call: &[String]) -> Vec<String> {
    call.iter()
        .filter_map(|l| l.strip_prefix("arg:"))
        .map(str::to_owned)
        .collect()
}

fn value<'a>(call: &'a [String], prefix: &str) -> Option<&'a str> {
    call.iter().find_map(|l| l.strip_prefix(prefix))
}

fn run_calls(bin: &Path) -> Vec<Vec<String>> {
    calls(bin)
        .into_iter()
        .filter(|c| args_of(c).get(2).is_some_and(|a| a == "run"))
        .collect()
}

fn crush_cfg(bin: PathBuf) -> AgentConfig {
    AgentConfig {
        bin,
        ..AgentConfig::default_for(AgentKind::Crush)
    }
}

fn inv(prompt: &str) -> Invocation<'_> {
    Invocation {
        prompt,
        resume: None,
        model: None,
    }
}

/// The Hashi home, created first as the server does, so the account
/// paths are the same on every run.
fn home(root: &Path) -> PathBuf {
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    home
}

/// A workspace under `root` with its own git root, so the project config
/// walk stops inside the temporary directory.
fn workspace(root: &Path) -> PathBuf {
    let ws = root.join("ws");
    std::fs::create_dir_all(ws.join(".git")).unwrap();
    let work = ws.join("tok").join("crush");
    std::fs::create_dir_all(&work).unwrap();
    work
}

async fn run_default(
    home: &Path,
    cfg: &AgentConfig,
    work: &Path,
    prompt: &str,
) -> Result<agent::AgentOutput, agent::AgentError> {
    let env = accounts::run_env(
        home,
        AgentKind::Crush,
        accounts::DEFAULT,
        AccountKind::Default,
    )
    .unwrap();
    assert!(
        env.is_some(),
        "Crush's default account has its own directory"
    );
    agent::execute_as(
        AgentKind::Crush,
        cfg,
        inv(prompt),
        work,
        Limits::default(),
        env.as_ref(),
    )
    .await
}

#[tokio::test]
async fn argv_is_one_prompt_after_the_separator_with_no_auto_approval() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_fake(dir.path());
    let home = home(dir.path());
    let work = workspace(dir.path());
    let out = run_default(
        &home,
        &crush_cfg(bin.clone()),
        &work,
        "--yolo -s x explain this",
    )
    .await
    .unwrap();
    assert_eq!(out.text, "crush on default says: --yolo -s x explain this");

    let runs = run_calls(&bin);
    assert_eq!(runs.len(), 1);
    let args = args_of(&runs[0]);
    let data = accounts::account_dir(&home, AgentKind::Crush, "default").join("data");
    assert_eq!(args[0], "--data-dir");
    assert!(Path::new(&args[1]).starts_with(&data), "{args:?}");
    assert_eq!(&args[2..], ["run", "-q", "--", "--yolo -s x explain this"]);
    assert!(!args[..5].iter().any(|a| a == "-y" || a == "--yolo"));
    assert_eq!(
        value(&runs[0], "stdin:"),
        Some("char"),
        "stdin is /dev/null"
    );
    assert_eq!(
        value(&runs[0], "cwd:").map(PathBuf::from),
        Some(std::fs::canonicalize(&work).unwrap())
    );
    // Version first, the run, then the session lookup.
    let all = calls(&bin);
    assert_eq!(args_of(&all[0]), ["--version"]);
    assert_eq!(&args_of(&all[2])[2..], ["session", "last", "--json"]);
    for call in &all {
        assert_eq!(value(call, "env:CRUSH_DISABLE_METRICS="), Some("1"));
        assert_eq!(value(call, "env:DO_NOT_TRACK="), Some("1"));
        assert_eq!(
            value(call, "env:CRUSH_DISABLE_PROVIDER_AUTO_UPDATE="),
            Some("1")
        );
        assert_eq!(value(call, "env:CRUSH_CLIENT_SERVER="), None);
    }
}

#[tokio::test]
async fn the_workspace_config_holds_the_deny_list() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_fake(dir.path());
    let home = home(dir.path());
    let work = workspace(dir.path());
    run_default(&home, &crush_cfg(bin.clone()), &work, "hi")
        .await
        .unwrap();
    let runs = run_calls(&bin);
    let data = PathBuf::from(&args_of(&runs[0])[1]);
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(data.join("crush.json")).unwrap()).unwrap();
    let denied: Vec<&str> = written["options"]["disabled_tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(denied, agent::crush::DENIED_TOOLS);
    for tool in [
        "bash",
        "edit",
        "multiedit",
        "write",
        "fetch",
        "agentic_fetch",
        "download",
        "agent",
    ] {
        assert!(denied.contains(&tool), "{tool}");
    }
    assert_eq!(written["options"]["auto_lsp"], false);
    assert_eq!(written["options"]["disable_metrics"], true);
    // The fake saw it before the run.
    assert!(value(&runs[0], "config:").unwrap().contains("\"bash\""));

    // A tampered file is replaced on the next run.
    std::fs::write(
        data.join("crush.json"),
        r#"{"options":{"disabled_tools":[]}}"#,
    )
    .unwrap();
    run_default(&home, &crush_cfg(bin.clone()), &work, "again")
        .await
        .unwrap();
    let again: Value =
        serde_json::from_str(&std::fs::read_to_string(data.join("crush.json")).unwrap()).unwrap();
    assert_eq!(again, written);
}

#[tokio::test]
async fn a_project_config_in_the_workspace_or_a_parent_refuses_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_fake(dir.path());
    let home = home(dir.path());
    let work = workspace(dir.path());
    let cfg = crush_cfg(bin.clone());
    for place in [work.clone(), work.parent().unwrap().to_path_buf()] {
        for name in [".crushrc", "crushrc", ".crush.json", "crush.json"] {
            let path = place.join(name);
            std::fs::write(&path, "echo pwned").unwrap();
            let err = run_default(&home, &cfg, &work, "hi")
                .await
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("refusing to run Crush") && err.contains(name),
                "{err}"
            );
            std::fs::remove_file(&path).unwrap();
        }
    }
    assert!(calls(&bin).is_empty(), "Crush never ran");
    run_default(&home, &cfg, &work, "hi").await.unwrap();
}

#[tokio::test]
async fn a_newer_or_unreadable_version_is_refused_unless_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_fake(dir.path());
    let home = home(dir.path());
    let work = workspace(dir.path());
    let version = bin.parent().unwrap().join("version");
    let mut cfg = crush_cfg(bin.clone());

    std::fs::write(&version, "v0.101.0").unwrap();
    let err = run_default(&home, &cfg, &work, "hi")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("0.101.0") && err.contains("--crush-allow-newer"),
        "{err}"
    );
    std::fs::write(&version, "devel").unwrap();
    assert!(run_default(&home, &cfg, &work, "hi").await.is_err());
    std::fs::write(&version, "v0.99.3").unwrap();
    assert!(run_default(&home, &cfg, &work, "hi").await.is_err());
    assert!(run_calls(&bin).is_empty(), "nothing ran past the gate");

    cfg.allow_newer = true;
    std::fs::write(&version, "v0.101.0").unwrap();
    run_default(&home, &cfg, &work, "hi").await.unwrap();
    cfg.allow_newer = false;
    std::fs::write(&version, "v0.100.4").unwrap();
    run_default(&home, &cfg, &work, "hi").await.unwrap();
    assert_eq!(run_calls(&bin).len(), 2);
}

#[tokio::test]
async fn usage_and_session_come_from_session_last_and_resume_uses_s() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_fake(dir.path());
    let home = home(dir.path());
    let work = workspace(dir.path());
    let cfg = crush_cfg(bin.clone());
    let first = run_default(&home, &cfg, &work, "one").await.unwrap();
    assert_eq!(first.session_id.as_deref(), Some("sess-default-1"));
    let usage = first.usage.unwrap();
    assert_eq!((usage.input_tokens, usage.output_tokens), (1200, 80));
    assert!((usage.cost_usd.unwrap() - 0.25).abs() < 1e-9);

    let env = accounts::run_env(&home, AgentKind::Crush, "default", AccountKind::Default).unwrap();
    let second = agent::execute_as(
        AgentKind::Crush,
        &cfg,
        Invocation {
            prompt: "two",
            resume: Some("sess-default-1"),
            model: Some("anthropic/claude-sonnet-4-5"),
        },
        &work,
        Limits::default(),
        env.as_ref(),
    )
    .await
    .unwrap();
    assert_eq!(second.session_id.as_deref(), Some("sess-default-1"));
    // The session's running cost is 0.50 now; this run added 0.25.
    assert!((second.usage.unwrap().cost_usd.unwrap() - 0.25).abs() < 1e-9);
    let runs = run_calls(&bin);
    assert_eq!(
        &args_of(&runs[1])[2..],
        [
            "run",
            "-q",
            "-m",
            "anthropic/claude-sonnet-4-5",
            "-s",
            "sess-default-1",
            "--",
            "two"
        ]
    );
    let shows: Vec<_> = calls(&bin)
        .into_iter()
        .filter(|c| args_of(c).get(3).is_some_and(|a| a == "show"))
        .collect();
    assert_eq!(
        &args_of(&shows[0])[2..],
        ["session", "show", "sess-default-1", "--json"]
    );
}

#[tokio::test]
async fn provider_limits_are_detected() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_fake(dir.path());
    let home = home(dir.path());
    let work = workspace(dir.path());
    let cfg = crush_cfg(bin);
    let err = run_default(&home, &cfg, &work, "limit me")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("Rate limit exceeded"), "{err}");
    assert_eq!(agent::limit_hit(AgentKind::Crush, &err), Some(None));
    let err = run_default(&home, &cfg, &work, "fail me")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("Invalid API key"), "{err}");
    assert_eq!(agent::limit_hit(AgentKind::Crush, &err), None);
}

/// The owner's variables, set on a re-run of this test binary (the
/// environment cannot be changed in process).
const OWNER_VARS: [(&str, &str); 6] = [
    ("ANTHROPIC_API_KEY", "owner-anthropic"),
    ("OPENAI_API_KEY", "owner-openai"),
    ("CRUSH_ANTHROPIC_API_KEY", "owner-crush-anthropic"),
    ("CRUSH_HOME", "/owner/home"),
    ("XDG_CONFIG_HOME", "/owner/.config"),
    ("CRUSH_CLIENT_SERVER", "1"),
];

#[tokio::test]
async fn each_account_gets_its_own_directories_and_no_owner_keys() {
    const MARK: &str = "HASHI_CRUSH_ENV_PROBE";
    if std::env::var_os(MARK).is_none() {
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "each_account_gets_its_own_directories_and_no_owner_keys",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(MARK, "1");
        for (k, v) in OWNER_VARS {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8_lossy(&out.stdout).contains("1 passed"));
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let bin = write_fake(dir.path());
    let home = home(dir.path());
    let work = workspace(dir.path());
    let cfg = crush_cfg(bin.clone());
    let login = accounts::create_dir(&home, AgentKind::Crush, "work").unwrap();
    let default = accounts::account_dir(&home, AgentKind::Crush, "default");

    run_default(&home, &cfg, &work, "d").await.unwrap();
    let env = accounts::run_env(&home, AgentKind::Crush, "work", AccountKind::Login).unwrap();
    let out = agent::execute_as(
        AgentKind::Crush,
        &cfg,
        inv("w"),
        &work,
        Limits::default(),
        env.as_ref(),
    )
    .await
    .unwrap();
    assert_eq!(out.text, "crush on work says: w");

    let runs = run_calls(&bin);
    for (call, acct, owner_keys) in [(&runs[0], &default, true), (&runs[1], &login, false)] {
        let acct = std::fs::canonicalize(acct).unwrap();
        let get = |k: &str| value(call, &format!("env:{k}=")).map(PathBuf::from);
        assert_eq!(get("HOME"), Some(acct.clone()));
        assert_eq!(get("CRUSH_GLOBAL_CONFIG"), Some(acct.join("config")));
        assert_eq!(get("CRUSH_GLOBAL_DATA"), Some(acct.join("global-data")));
        assert_eq!(get("CRUSH_CACHE_DIR"), Some(acct.join("cache")));
        assert_eq!(get("CRUSH_SKILLS_DIR"), Some(acct.join("skills")));
        assert!(Path::new(&args_of(call)[1]).starts_with(acct.join("data")));
        // Every inherited CRUSH_* is gone, on every account.
        for k in [
            "CRUSH_ANTHROPIC_API_KEY",
            "CRUSH_HOME",
            "CRUSH_CLIENT_SERVER",
            "XDG_CONFIG_HOME",
        ] {
            assert_eq!(value(call, &format!("env:{k}=")), None, "{k}");
        }
        // The default account may use the owner's provider keys; others not.
        assert_eq!(
            value(call, "env:ANTHROPIC_API_KEY=").is_some(),
            owner_keys,
            "{call:?}"
        );
        assert_eq!(value(call, "env:OPENAI_API_KEY=").is_some(), owner_keys);
    }
    // Different accounts, different data directories for the same workspace.
    assert_ne!(args_of(&runs[0])[1], args_of(&runs[1])[1]);
}

// ── through the server ─────────────────────────────────────────────────────

struct Harness {
    _dir: tempfile::TempDir,
    running: Running,
    base: String,
    admin: String,
    http: reqwest::Client,
}

async fn harness(enabled: bool) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_fake(dir.path());
    // `--enable-crush` off leaves Crush out of the configured agents.
    let agents: HashMap<AgentKind, AgentConfig> = if enabled {
        HashMap::from([(AgentKind::Crush, crush_cfg(bin))])
    } else {
        HashMap::new()
    };
    let cfg = ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        home: dir.path().join("home"),
        agents,
        default_agent: AgentKind::Crush,
        concurrency: 2,
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
        let mut body = json!({ "name": "crush-user" });
        if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
            b.extend(e.clone());
        }
        let (status, v) = self.post("/v1/tokens", &self.admin, body).await;
        assert_eq!(status, StatusCode::CREATED, "{v}");
        v["token"].as_str().unwrap().to_owned()
    }

    async fn ask(&self, token: &str, message: &str) -> (StatusCode, Value) {
        self.post(
            "/v1/ask",
            token,
            json!({"message": message, "agent": "crush", "wait": 10}),
        )
        .await
    }

    async fn tools(&self, token: &str) -> String {
        let (status, v) = self
            .post(
                "/mcp",
                token,
                json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{v}");
        v.to_string()
    }
}

#[tokio::test]
async fn disabled_crush_is_not_listed_and_not_available() {
    let h = harness(false).await;
    assert!(
        h.running
            .state
            .runtime
            .agents
            .iter()
            .all(|a| a.agent != AgentKind::Crush)
    );
    let token = h.user_token(json!({})).await;
    let (status, v) = h.ask(&token, "hello").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert!(
        v.to_string()
            .contains("Crush is not installed or not enabled"),
        "{v}"
    );
    assert!(!h.tools(&token).await.contains("ask_crush"));
    h.running.shutdown().await;
}

#[tokio::test]
async fn enabled_crush_answers_and_stateful_tokens_resume() {
    let h = harness(true).await;
    let token = h.user_token(json!({"sessionMode": "stateful"})).await;
    assert!(h.tools(&token).await.contains("ask_crush"));
    let (status, first) = h.ask(&token, "one").await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["status"], "completed", "{first}");
    assert_eq!(first["response"], "crush on default says: one");
    assert_eq!(first["usage"]["inputTokens"], 1200, "{first}");
    assert_eq!(first["usage"]["outputTokens"], 80);
    let (_, second) = h.ask(&token, "two").await;
    assert_eq!(second["status"], "completed", "{second}");
    let bin = h._dir.path().join("bin").join("crush");
    let runs = run_calls(&bin);
    let args = args_of(&runs[1]);
    let at = args.iter().position(|a| a == "-s").unwrap();
    assert_eq!(args[at + 1], "sess-default-1");
    h.running.shutdown().await;
}
