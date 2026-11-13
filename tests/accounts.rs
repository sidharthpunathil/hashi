//! `hashi account`, `--account` on `token` and `share`: the real binary on a
//! throwaway home, with a fake agent that only records how it was called.
//! Standard input is never a terminal here, so no sign-in command ever runs.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// Records every call; `auth status --json` reports a login.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
echo "$*" >> "$(dirname "$0")/calls.log"
if [ "$1" = "auth" ] && [ "$2" = "status" ]; then echo '{"loggedIn": true}'; exit 0; fi
exit 0
"#;

struct Cli {
    dir: tempfile::TempDir,
    claude: PathBuf,
}

impl Cli {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("claude");
        std::fs::write(&claude, FAKE_CLAUDE).unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir, claude }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn command(&self, args: &[&str]) -> Command {
        let missing = self.dir.path().join("missing-agent");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hashi"));
        cmd.arg("--home")
            .arg(self.home())
            .args(args)
            .env("HASHI_CLAUDE_BIN", &self.claude)
            .env("HASHI_CODEX_BIN", &missing)
            .env("HASHI_GEMINI_BIN", &missing)
            .env("HASHI_OPENCODE_BIN", &missing)
            .env("HASHI_QWEN_BIN", &missing)
            .env("HASHI_CURSOR_BIN", &missing)
            .env("HASHI_NO_UPDATE_CHECK", "1")
            .stdin(Stdio::null());
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
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

    fn calls(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("calls.log")).unwrap_or_default()
    }
}

fn mode(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn a_login_account_prints_its_sign_in_command_without_a_terminal() {
    let cli = Cli::new();
    let out = cli.ok(&["account", "add", "claude", "work", "--yes"]);
    let dir = std::fs::canonicalize(cli.home())
        .unwrap()
        .join("accounts/claude/work");
    assert!(
        out.contains(&format!("CLAUDE_CONFIG_DIR={}", dir.display())),
        "{out}"
    );
    assert!(out.contains("auth login"), "{out}");
    assert_eq!(mode(&dir), 0o700);
    assert!(
        !cli.calls().contains("auth login"),
        "the sign-in command must not run without a terminal"
    );

    // Signing in again also only prints, and the status check uses the
    // account directory.
    let out = cli.ok(&["account", "login", "claude", "work"]);
    assert!(out.contains("auth login"), "{out}");
    let list = cli.ok(&["account", "list"]);
    assert!(
        list.contains("work") && list.contains("login") && list.contains("yes"),
        "{list}"
    );
    assert!(cli.calls().contains("auth status --json"));
    assert!(!cli.calls().contains("auth login"));

    // Names and kinds are checked.
    assert!(
        cli.fails(&["account", "add", "claude", "work"])
            .contains("already has")
    );
    assert!(
        cli.fails(&["account", "add", "claude", "Bad_Name"])
            .contains("1 to 32")
    );
    assert!(
        cli.fails(&["account", "add", "claude", "default"])
            .contains("default")
    );
    assert!(
        cli.fails(&["account", "add", "cursor", "x"])
            .contains("api-key")
    );
    assert!(
        cli.fails(&["account", "add", "qwen", "x", "--key-env", "K"])
            .contains("does not support")
    );
    assert!(
        cli.fails(&["account", "login", "claude", "ghost"])
            .contains("no claude account")
    );
}

#[test]
fn key_accounts_read_the_key_from_env_or_stdin_and_never_print_it() {
    let cli = Cli::new();
    let out = cli
        .command(&["account", "add", "codex", "ci", "--key-env", "MY_TEST_KEY"])
        .env("MY_TEST_KEY", "sk-cli-secret-1")
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{text}");
    assert!(!text.contains("sk-cli-secret-1"), "{text}");
    let key = std::fs::canonicalize(cli.home())
        .unwrap()
        .join("accounts/codex/ci/key");
    assert_eq!(std::fs::read_to_string(&key).unwrap(), "sk-cli-secret-1");
    assert_eq!(mode(&key), 0o600);

    // From a pipe on standard input.
    let mut child = cli
        .command(&[
            "account",
            "add",
            "claude",
            "bot",
            "--kind",
            "oauth-token",
            "--key-stdin",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"sk-ant-oat-pipe\n")
            .unwrap();
    }
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert!(!String::from_utf8_lossy(&out.stdout).contains("sk-ant-oat-pipe"));

    // A missing variable leaves nothing behind.
    assert!(
        cli.fails(&[
            "account",
            "add",
            "gemini",
            "g",
            "--key-env",
            "UNSET_TEST_KEY_VAR"
        ])
        .contains("UNSET_TEST_KEY_VAR is not set")
    );
    let list = cli.ok(&["account", "list"]);
    assert!(!list.contains("gemini"), "{list}");
    assert!(list.contains("api-key") && list.contains("oauth-token") && list.contains("key set"));
    assert!(!list.contains("sk-"), "{list}");
}

#[test]
fn tokens_and_invites_take_accounts_and_removal_takes_them_off() {
    let cli = Cli::new();
    cli.ok(&["account", "add", "claude", "work"]);
    cli.ok(&["account", "add", "claude", "personal"]);

    let err = cli.fails(&["token", "create", "bot", "--account", "claude=ghost"]);
    assert!(err.contains("claude:ghost"), "{err}");
    assert!(
        cli.fails(&["token", "create", "bot", "--account", "claude"])
            .contains("expected")
    );
    cli.ok(&[
        "token",
        "create",
        "bot",
        "--account",
        "claude=work,personal",
    ]);
    let list = cli.ok(&["token", "list"]);
    assert!(list.contains("claude=work,personal"), "{list}");

    let id = {
        let store = hashi::server::open_store(&cli.home()).unwrap();
        store
            .list_tokens()
            .unwrap()
            .into_iter()
            .find(|t| t.name == "bot")
            .unwrap()
            .id
    };
    cli.ok(&["token", "update", &id, "--account", "claude=personal"]);
    assert!(cli.ok(&["token", "list"]).contains("claude=personal"));
    cli.ok(&["token", "update", &id, "--clear-accounts"]);
    assert!(!cli.ok(&["token", "list"]).contains("claude=personal"));
    cli.ok(&["token", "update", &id, "--account", "claude=work,personal"]);

    cli.ok(&["share", "carol", "--account", "claude=work"]);
    assert!(
        cli.fails(&["share", "dave", "--account", "claude=ghost"])
            .contains("claude:ghost")
    );

    cli.ok(&["account", "disable", "claude", "work"]);
    assert!(cli.ok(&["account", "list"]).contains("disabled"));
    cli.ok(&["account", "enable", "claude", "work"]);

    // A limit far in the future (here two weeks) is cleared on request.
    {
        let store = hashi::server::open_store(&cli.home()).unwrap();
        let until = hashi::util::now_ms() + 14 * 86_400_000;
        store
            .mark_account_limited(hashi::agent::AgentKind::Claude, "work", until)
            .unwrap();
    }
    assert!(cli.ok(&["account", "list"]).contains("limited until"));
    let out = cli.ok(&["account", "clear-limit", "claude", "work"]);
    assert!(out.contains("cleared the limit on claude:work"), "{out}");
    assert!(!cli.ok(&["account", "list"]).contains("limited until"));
    assert!(
        cli.ok(&["account", "clear-limit", "claude", "work"])
            .contains("was not limited")
    );
    assert!(
        cli.ok(&["account", "clear-limit", "claude", "default"])
            .contains("was not limited")
    );
    assert!(
        cli.fails(&["account", "clear-limit", "claude", "ghost"])
            .contains("no claude account")
    );
    assert!(
        cli.fails(&["account", "disable", "claude", "default"])
            .contains("cannot be disabled")
    );

    // Removing asks first; without a terminal it needs --yes.
    assert!(
        cli.fails(&["account", "remove", "claude", "work"])
            .contains("--yes")
    );
    let out = cli.ok(&["account", "remove", "claude", "work", "--yes"]);
    assert!(out.contains("1 token(s) and 1 invite(s)"), "{out}");
    let dir = std::fs::canonicalize(cli.home())
        .unwrap()
        .join("accounts/claude/work");
    assert!(!dir.exists());
    let store = hashi::server::open_store(&cli.home()).unwrap();
    let bot = store.get_token(&id).unwrap().unwrap();
    assert_eq!(bot.accounts[&hashi::agent::AgentKind::Claude], ["personal"]);
    assert!(store.list_invites().unwrap()[0].accounts.is_empty());
    let audit: Vec<String> = store
        .list_audit(50)
        .unwrap()
        .into_iter()
        .map(|e| e.action)
        .collect();
    assert!(audit.iter().any(|a| a == "account.remove"), "{audit:?}");
}

#[test]
fn account_usage_shows_bars_resets_and_unknowns_without_reading_anything() {
    use hashi::agent::AgentKind;
    use hashi::usage::{Source, Window};
    let cli = Cli::new();
    assert!(
        cli.ok(&["account", "usage"])
            .contains("no accounts added yet"),
        "nothing added yet"
    );
    assert!(
        cli.ok(&["account", "usage", "--all"])
            .contains("no usage yet"),
        "nothing read yet"
    );
    cli.ok(&["account", "add", "claude", "work", "--yes"]);
    cli.ok(&["account", "add", "claude", "home", "--yes"]);
    let store = hashi::server::open_store(&cli.home()).unwrap();
    let in_2h = hashi::util::now_ms() + 2 * 3_600_000 + 60_000;
    store
        .replace_usage(
            AgentKind::Claude,
            "work",
            &[
                Window::new(
                    "five_hour",
                    "5 hours",
                    31.0,
                    Some(in_2h),
                    Some(300),
                    Source::Poll,
                ),
                Window::new("seven_day", "week", 92.0, None, Some(10_080), Source::Poll),
            ],
        )
        .unwrap();
    store
        .set_poll_status(AgentKind::Claude, "work", true, Some("max"), None)
        .unwrap();
    store
        .set_poll_status(
            AgentKind::Claude,
            "home",
            false,
            None,
            Some("unknown, refreshes on its next run"),
        )
        .unwrap();
    // Your own login's figures show only with --all.
    store
        .replace_usage(
            AgentKind::Claude,
            "default",
            &[Window::new(
                "seven_day",
                "week",
                10.0,
                None,
                Some(10_080),
                Source::Poll,
            )],
        )
        .unwrap();
    drop(store);
    assert!(
        cli.ok(&["account", "usage", "--all"])
            .contains("claude:default"),
        "--all shows the default account"
    );

    let out = cli.ok(&["account", "usage"]);
    assert!(!out.contains("claude:default"), "{out}");
    assert!(
        out.contains("claude:work  max  8% left (warning)  (poll,"),
        "{out}"
    );
    assert!(out.contains("5 hours"), "{out}");
    assert!(out.contains("31%") && out.contains("resets in 2h"), "{out}");
    assert!(out.contains("week") && out.contains("92%"), "{out}");
    assert!(out.contains("claude:home  unknown"), "{out}");
    assert!(out.contains("refreshes on its next run"), "{out}");
    assert!(out.contains("0 run(s) today"), "{out}");

    let json: serde_json::Value =
        serde_json::from_str(&cli.ok(&["account", "usage", "--json", "--agent", "claude"]))
            .unwrap();
    let work = json["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "work")
        .unwrap();
    assert_eq!(work["summary"]["headroom"], 8);
    assert_eq!(work["summary"]["state"], "warning");
    assert_eq!(work["usage"].as_array().unwrap().len(), 2);
    let other = cli.ok(&["account", "usage", "--agent", "codex"]);
    assert!(other.contains("no accounts added yet"), "{other}");

    let list = cli.ok(&["account", "list"]);
    assert!(
        list.contains("USAGE") && list.contains("8% left (warning)"),
        "{list}"
    );
    assert!(list.contains("unknown"), "{list}");
}

#[test]
fn projects_take_a_strategy_and_threshold() {
    let cli = Cli::new();
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().display().to_string();
    let out = cli.ok(&[
        "project",
        "add",
        "api",
        &path,
        "--agents",
        "claude",
        "--account-strategy",
        "most-left",
        "--rotate-at",
        "80",
    ]);
    assert!(out.contains("added project api"), "{out}");
    let list = cli.ok(&["project", "list"]);
    assert!(list.contains("most-left at 80%"), "{list}");
    let out = cli.ok(&["project", "update", "api", "--clear-rotate-at"]);
    assert!(out.contains("rotate at server default"), "{out}");
    assert!(
        cli.ok(&["project", "list"])
            .contains("most-left at default")
    );
    let out = cli.ok(&[
        "project",
        "update",
        "api",
        "--account-strategy",
        "soonest-reset",
    ]);
    assert!(out.contains("account strategy soonest-reset"), "{out}");
    assert!(
        cli.fails(&["project", "update", "api", "--rotate-at", "40"])
            .contains("40")
    );
    assert!(
        cli.fails(&["project", "update", "api", "--account-strategy", "random"])
            .contains("unknown account strategy")
    );
    let store = hashi::server::open_store(&cli.home()).unwrap();
    let p = store.get_project("api").unwrap().unwrap();
    assert_eq!(
        (p.account_strategy, p.rotate_at),
        (Some(hashi::usage::Strategy::SoonestReset), None)
    );
}
