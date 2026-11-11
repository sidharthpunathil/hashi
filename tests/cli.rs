//! The admin commands as a script meets them: `--json` on the list
//! commands, tables that stay aligned, tokens named by name, and errors that
//! say what to run next. Every run uses a throwaway home and no real agent.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use serde_json::Value;

struct Cli {
    dir: tempfile::TempDir,
}

impl Cli {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn run(&self, args: &[&str]) -> Output {
        let missing = self.dir.path().join("missing-agent");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hashi"));
        cmd.arg("--home").arg(self.home()).args(args);
        for var in [
            "HASHI_CLAUDE_BIN",
            "HASHI_CODEX_BIN",
            "HASHI_GEMINI_BIN",
            "HASHI_OPENCODE_BIN",
            "HASHI_QWEN_BIN",
            "HASHI_CURSOR_BIN",
        ] {
            cmd.env(var, &missing);
        }
        cmd.env("HASHI_NO_UPDATE_CHECK", "1")
            .env_remove("HASHI_PUBLIC_URL")
            .env_remove("HASHI_PORT")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    /// Standard output of a command that must succeed.
    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// Standard error of a command that must fail.
    fn fails(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(!out.status.success(), "{args:?} succeeded");
        String::from_utf8(out.stderr).unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        let text = self.ok(args);
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{args:?}: {e}: {text}"))
    }
}

/// Any `hashi_` secret: 64 hex digits, alone or after `inv_`, `ask_` or
/// `pick_`.
fn has_secret(text: &str) -> bool {
    text.match_indices("hashi_").any(|(i, _)| {
        let rest = &text[i + "hashi_".len()..];
        let rest = ["inv_", "ask_", "pick_"]
            .iter()
            .find_map(|p| rest.strip_prefix(p))
            .unwrap_or(rest);
        rest.chars().take_while(char::is_ascii_hexdigit).count() >= 32
    })
}

fn keys(v: &Value) -> Vec<&str> {
    v.as_object().unwrap().keys().map(String::as_str).collect()
}

#[test]
fn list_commands_print_json_with_the_api_field_names_and_no_secrets() {
    let cli = Cli::new();
    let secret = cli.ok(&["token", "create", "ci-bot", "--agents", "claude"]);
    assert!(has_secret(&secret), "{secret}");
    let link = cli.ok(&["share", "alice", "--daily-requests", "5"]);
    assert!(link.contains("/claim/hashi_inv_"), "{link}");
    let folder = cli.dir.path().join("web");
    std::fs::create_dir_all(&folder).unwrap();
    cli.ok(&[
        "project",
        "add",
        "web",
        folder.to_str().unwrap(),
        "--agents",
        "claude",
    ]);
    cli.ok(&["requests", "open", "--hours", "2"]);

    let tokens = cli.json(&["token", "list", "--json"]);
    let list = tokens["tokens"].as_array().unwrap();
    let bot = list.iter().find(|t| t["name"] == "ci-bot").unwrap();
    for key in [
        "id",
        "prefix",
        "role",
        "sessionMode",
        "agents",
        "expiresAt",
        "lastUsedAt",
        "paused",
        "limits",
        "accounts",
        "project",
        "shareUsage",
        "status",
        "conversations",
    ] {
        assert!(bot.get(key).is_some(), "token has no {key}: {bot}");
    }
    assert_eq!(bot["status"], "active");
    assert_eq!(bot["agents"], serde_json::json!(["claude"]));

    let invites = cli.json(&["invite", "list", "--json"]);
    let alice = &invites["invites"][0];
    assert_eq!(alice["label"], "alice");
    assert_eq!(alice["status"], "pending");
    assert_eq!(alice["limits"]["dailyRequests"], 5);
    assert!(alice.get("tokenExpiresInDays").is_some());

    let projects = cli.json(&["project", "list", "--json"]);
    assert_eq!(projects["projects"][0]["name"], "web");

    let accounts = cli.json(&["account", "list", "--json"]);
    assert!(accounts["accounts"].as_array().unwrap().is_empty());

    let requests = cli.json(&["requests", "list", "--json"]);
    assert_eq!(requests["pending"], 0);
    assert_eq!(requests["link"]["status"], "open");
    assert!(requests["requests"].as_array().unwrap().is_empty());

    let audit = cli.json(&["audit", "--json", "--limit", "3"]);
    let entries = audit["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    let mut fields = keys(&entries[0]);
    fields.sort_unstable();
    assert_eq!(fields, ["action", "actor", "at", "detail", "target"]);

    for args in [
        &["token", "list", "--json"][..],
        &["invite", "list", "--json"],
        &["requests", "list", "--json", "--all"],
        &["audit", "--json"],
        &["token", "list"],
        &["invite", "list"],
    ] {
        let out = cli.ok(args);
        assert!(!has_secret(&out), "{args:?} printed a secret: {out}");
    }
}

#[test]
fn an_account_shows_up_in_account_list_json() {
    let cli = Cli::new();
    cli.ok(&["account", "add", "claude", "work"]);
    let accounts = cli.json(&["account", "list", "--json"]);
    let work = &accounts["accounts"][0];
    assert_eq!(work["agent"], "claude");
    assert_eq!(work["name"], "work");
    assert_eq!(work["kind"], "login");
    for key in [
        "state",
        "runsToday",
        "tokens",
        "summary",
        "usage",
        "lastUsed",
    ] {
        assert!(work.get(key).is_some(), "account has no {key}: {work}");
    }
}

#[test]
fn tables_stay_aligned_with_long_names() {
    let cli = Cli::new();
    let long = "a token with a rather long name";
    cli.ok(&["token", "create", long]);
    cli.ok(&["token", "create", "b"]);
    let out = cli.ok(&["token", "list"]);
    let lines: Vec<&str> = out.lines().collect();
    let col = |line: &str| line.find("hashi_").unwrap_or(usize::MAX);
    let header = lines[0].find("PREFIX").unwrap();
    for line in &lines[1..] {
        assert_eq!(col(line), header, "{out}");
    }
    // Every agent reads as `all`.
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with('b') && l.contains(" all ")),
        "{out}"
    );

    let out = cli.ok(&["audit"]);
    let lines: Vec<&str> = out.lines().collect();
    let target = lines[0].find("TARGET").unwrap();
    for line in &lines[1..] {
        // Each target starts in the TARGET column, after a space.
        assert_eq!(&line[target - 1..target], " ", "{out}");
        assert_ne!(&line[target..=target], " ", "{out}");
    }
}

#[test]
fn a_token_can_be_named_by_its_name_and_misses_say_what_to_run() {
    let cli = Cli::new();
    cli.ok(&["token", "create", "ci-bot"]);
    assert!(
        cli.ok(&["token", "update", "ci-bot", "--pause"])
            .contains("paused")
    );
    let tokens = cli.json(&["token", "list", "--json"]);
    let bot = tokens["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "ci-bot")
        .unwrap()
        .clone();
    assert_eq!(bot["status"], "paused");
    assert!(
        cli.ok(&["token", "revoke", "ci-bot"])
            .contains("revoked ci-bot")
    );

    let miss = cli.fails(&["token", "revoke", "nobody"]);
    assert!(miss.contains("hashi token list"), "{miss}");
    let miss = cli.fails(&["invite", "cancel", "nope"]);
    assert!(miss.contains("hashi invite list"), "{miss}");
    let miss = cli.fails(&["project", "remove", "nope"]);
    assert!(miss.contains("hashi project list"), "{miss}");
    let miss = cli.fails(&["requests", "decline", "nope"]);
    assert!(miss.contains("hashi requests list --all"), "{miss}");
    cli.ok(&["token", "create", "root", "--admin"]);
    let last = cli.fails(&["token", "revoke", "root"]);
    assert!(last.contains("--admin"), "{last}");
}

#[test]
fn empty_lists_say_how_to_add_one() {
    let cli = Cli::new();
    for (args, hint) in [
        (&["invite", "list"][..], "hashi share"),
        (&["account", "list"], "hashi account add"),
        (&["project", "list"], "hashi project add"),
    ] {
        let out = cli.ok(args);
        assert!(out.starts_with("No "), "{args:?}: {out}");
        assert!(out.contains(hint), "{args:?}: {out}");
    }
}

#[test]
fn the_main_commands_show_examples_in_their_help() {
    let cli = Cli::new();
    for cmd in [
        "share", "token", "account", "project", "requests", "connect", "llm",
    ] {
        let help = cli.ok(&[cmd, "--help"]);
        assert!(help.contains("Examples:"), "{cmd}: {help}");
        assert!(help.contains(&format!("hashi {cmd}")), "{cmd}: {help}");
    }
}

#[test]
fn llm_prints_the_guide_with_the_command_reference() {
    let cli = Cli::new();
    let guide = cli.ok(&["llm"]);
    assert_eq!(cli.ok(&["guide"]), guide);
    assert!(guide.starts_with("# Running Hashi for your owner"));
    assert!(guide.contains("### `hashi token create`"), "no reference");
    assert!(guide.contains("### `hashi llm`"));
    assert!(!has_secret(&guide));
    let lines = guide.lines().count();
    assert!((300..=800).contains(&lines), "{lines} lines");

    let path = cli.dir.path().join("guide.md");
    let out = cli.run(&["llm", "--output", path.to_str().unwrap()]);
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), guide);
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}
