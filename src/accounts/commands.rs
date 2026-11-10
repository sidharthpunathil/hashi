//! Shell commands that start an agent's CLI on one of its accounts:
//! `<name>-claude` and `<name>-dclaude` (`claude
//! --dangerously-skip-permissions`) for Claude Code, `<name>-codex` and
//! `<name>-dcodex` (`codex --dangerously-bypass-approvals-and-sandbox`) for
//! Codex. The names are patterns the owner can change ([`Names`], saved in
//! `settings.json`, set with `hashi account commands` or `n` in the
//! dashboard's Accounts popup); an empty pattern turns that command off.
//!
//! Each command is a small script in the folder that holds the `hashi`
//! command the shell finds on `PATH` (usually `~/.local/bin`), so it works
//! in every shell at once and no shell startup file is edited. A script
//! points the agent's account variable (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`)
//! at the account's directory (the same absolute path Hashi's runs use,
//! which Claude Code's macOS Keychain entry is tied to), drops the
//! variables that would pick another sign-in, and starts the CLI from
//! `PATH` with the arguments it was given.
//!
//! [`sync`] makes the folder match the accounts and the patterns: it
//! writes the commands of every local account and deletes those whose
//! account is gone or whose name changed. It runs when an account is
//! added, signed in again or removed, when the patterns change, when the
//! server starts, and on `hashi account list` and `hashi account commands`,
//! so accounts added before this existed get their commands too. Remote
//! and pool accounts get none (nothing runs here), and neither does a
//! login someone shared with you, whose use is meant to go through Hashi.
//! Codex gets commands for login accounts only: its CLI reads an API key
//! from the environment only in `codex exec`.
//!
//! Every script carries a marker line naming the Hashi home that wrote it.
//! Hashi only ever rewrites or deletes files with its own home's marker, so
//! a file of the same name that someone else wrote, or another Hashi home's
//! command, is left alone. A command that would have the same name as a
//! program elsewhere on `PATH` is not made, so a pattern can never hide
//! one. Setting `HASHI_NO_ACCOUNT_COMMANDS` turns the whole thing off.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::accounts::{self as acct, AccountKind};
use crate::agent::AgentKind;
use crate::store::{AccountState, Store};

/// Set to anything non-empty to stop Hashi writing account commands.
pub const OFF_VAR: &str = "HASHI_NO_ACCOUNT_COMMANDS";

/// What a pattern's account name goes in place of.
pub const PLACEHOLDER: &str = "{name}";

/// Longest pattern.
pub const MAX_PATTERN: usize = 40;

/// Start of the marker line's text, followed by the home's path.
const MARKER: &str = "hashi-account-command home=";

/// Most bytes read from a file to find its marker.
const HEAD: usize = 4096;

/// Files bigger than this are never Hashi's commands, so they are not read.
const MAX_SCRIPT: u64 = 16 * 1024;

/// An agent that gets commands, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spec {
    /// The agent.
    pub agent: AgentKind,
    /// The CLI started from `PATH`.
    pub bin: &'static str,
    /// The flag the second command adds.
    pub skip_flag: &'static str,
    /// The default pattern of the plain command.
    pub plain: &'static str,
    /// The default pattern of the command with [`Spec::skip_flag`].
    pub skip: &'static str,
    /// Whether key accounts get commands too.
    pub keys: bool,
    /// Prefixes of the [`acct::credential_vars`] a command clears.
    pub cleared: &'static [&'static str],
}

/// The agents that get commands.
pub const SPECS: [Spec; 2] = [
    Spec {
        agent: AgentKind::Claude,
        bin: "claude",
        skip_flag: "--dangerously-skip-permissions",
        plain: "{name}-claude",
        skip: "{name}-dclaude",
        keys: true,
        // Cloud variables such as `AWS_*` stay: the person may need them
        // for their own work in the session, and they do nothing once the
        // provider switches (`CLAUDE_CODE_USE_BEDROCK` and the rest) are
        // cleared.
        cleared: &["ANTHROPIC_", "CLAUDE_"],
    },
    Spec {
        agent: AgentKind::Codex,
        bin: "codex",
        skip_flag: "--dangerously-bypass-approvals-and-sandbox",
        plain: "{name}-codex",
        skip: "{name}-dcodex",
        keys: false,
        cleared: &["OPENAI_", "CODEX_"],
    },
];

/// The [`Spec`] of `agent`, if it gets commands.
#[must_use]
pub fn spec(agent: AgentKind) -> Option<&'static Spec> {
    SPECS.iter().find(|s| s.agent == agent)
}

/// One agent's two patterns. An empty one turns that command off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pair {
    /// The plain command.
    pub plain: String,
    /// The command that adds the agent's skip flag.
    pub skip: String,
}

/// The command name patterns, as saved in `settings.json`
/// (`accountCommands`). An agent left out uses its defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Names {
    /// Claude Code's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<Pair>,
    /// Codex's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex: Option<Pair>,
}

impl Names {
    /// The patterns of `spec`'s agent, its defaults when none are saved.
    #[must_use]
    pub fn pair(&self, spec: &Spec) -> Pair {
        let saved = match spec.agent {
            AgentKind::Claude => self.claude.as_ref(),
            AgentKind::Codex => self.codex.as_ref(),
            _ => None,
        };
        saved.cloned().unwrap_or_else(|| Pair {
            plain: spec.plain.to_owned(),
            skip: spec.skip.to_owned(),
        })
    }

    /// Sets `agent`'s patterns; the defaults are stored as none saved.
    pub fn set(&mut self, agent: AgentKind, pair: Pair) {
        let default = spec(agent).is_some_and(|s| pair.plain == s.plain && pair.skip == s.skip);
        let value = (!default).then_some(pair);
        match agent {
            AgentKind::Claude => self.claude = value,
            AgentKind::Codex => self.codex = value,
            _ => {}
        }
    }

    /// The name of `spec`'s command for account `name`, or `None` when it
    /// is turned off.
    #[must_use]
    pub fn command(&self, spec: &Spec, name: &str, skip: bool) -> Option<String> {
        let pair = self.pair(spec);
        let pattern = if skip { pair.skip } else { pair.plain };
        (!pattern.is_empty()).then(|| pattern.replace(PLACEHOLDER, name))
    }

    /// Checks every pattern, and that no two make the same name. The error
    /// names the pattern as `(agent, skip)`.
    ///
    /// # Errors
    ///
    /// The first bad pattern and why.
    pub fn check(&self) -> Result<(), ((AgentKind, bool), String)> {
        let mut seen: Vec<((AgentKind, bool), String)> = Vec::new();
        for spec in &SPECS {
            let pair = self.pair(spec);
            for (skip, pattern) in [(false, &pair.plain), (true, &pair.skip)] {
                let at = (spec.agent, skip);
                check_pattern(pattern).map_err(|e| (at, e))?;
                if pattern.is_empty() {
                    continue;
                }
                let key = pattern.to_ascii_lowercase();
                if seen.iter().any(|(_, p)| *p == key) {
                    return Err((
                        at,
                        format!("`{pattern}` is already used for another command"),
                    ));
                }
                seen.push((at, key));
            }
        }
        Ok(())
    }

    /// The saved patterns of `home`, or the defaults when none are saved
    /// or the saved ones are not valid.
    #[must_use]
    pub fn load(home: &Path) -> Self {
        crate::settings::load(home)
            .ok()
            .and_then(|s| s.account_commands)
            .filter(|n| n.check().is_ok())
            .unwrap_or_default()
    }

    /// Saves `self` in `home`'s `settings.json`, after checking it.
    ///
    /// # Errors
    ///
    /// When a pattern is bad or the file cannot be written.
    pub fn save(&self, home: &Path) -> Result<(), String> {
        self.check().map_err(|((agent, skip), e)| {
            format!(
                "{agent} {}command: {e}",
                if skip { "skip-permissions " } else { "" }
            )
        })?;
        let mut settings = crate::settings::load(home).unwrap_or_default();
        settings.account_commands = (*self != Self::default()).then(|| self.clone());
        crate::settings::save(home, &settings).map_err(|e| format!("saving settings: {e}"))
    }
}

/// Checks one pattern: empty (off), or [`PLACEHOLDER`] once with other
/// text from `A-Z a-z 0-9 . _ -`, not starting with `-` or `.`.
///
/// # Errors
///
/// Why it is not a valid pattern.
pub fn check_pattern(pattern: &str) -> Result<(), String> {
    if pattern.is_empty() {
        return Ok(());
    }
    if pattern.len() > MAX_PATTERN {
        return Err(format!("at most {MAX_PATTERN} characters"));
    }
    if pattern.matches(PLACEHOLDER).count() != 1 {
        return Err(format!(
            "needs {PLACEHOLDER} exactly once, for the account name"
        ));
    }
    let rest = pattern.replace(PLACEHOLDER, "");
    if rest.is_empty() {
        return Err(format!(
            "needs more than {PLACEHOLDER}, such as {PLACEHOLDER}-claude, so an account name \
             is never a command by itself"
        ));
    }
    if !rest
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err(format!(
            "only {PLACEHOLDER}, letters, digits, `.`, `_` and `-`"
        ));
    }
    if pattern.starts_with(['-', '.']) {
        return Err("cannot start with `-` or `.`".to_owned());
    }
    Ok(())
}

/// One command Hashi wants in the folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted {
    /// File name, `.cmd` included on Windows.
    pub file: String,
    /// What the user types.
    pub command: String,
    /// The account's agent.
    pub agent: AgentKind,
    /// The account's name.
    pub name: String,
    /// Whether it adds the agent's skip flag.
    pub skip: bool,
    /// The script.
    pub script: String,
}

/// A command in place.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Made {
    /// What the user types.
    pub command: String,
    /// The account's agent.
    pub agent: AgentKind,
    /// The account's name.
    pub name: String,
    /// Whether it adds the agent's skip flag.
    pub skip: bool,
}

impl Made {
    /// What it runs, such as `claude --dangerously-skip-permissions`.
    #[must_use]
    pub fn runs(&self) -> String {
        match spec(self.agent) {
            Some(s) if self.skip => format!("{} {}", s.bin, s.skip_flag),
            Some(s) => s.bin.to_owned(),
            None => String::new(),
        }
    }
}

/// What [`sync_in`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// The folder.
    pub dir: PathBuf,
    /// Commands in place for the accounts, sorted.
    pub commands: Vec<Made>,
    /// Commands written or rewritten.
    pub written: Vec<String>,
    /// Commands deleted: their account is gone or their name changed.
    pub removed: Vec<String>,
    /// Commands not written, with the reason.
    pub skipped: Vec<(String, String)>,
}

impl Report {
    /// The commands of account `name` of `agent`.
    #[must_use]
    pub fn for_account(&self, agent: AgentKind, name: &str) -> Vec<&str> {
        self.commands
            .iter()
            .filter(|m| m.agent == agent && m.name == name)
            .map(|m| m.command.as_str())
            .collect()
    }
}

/// Why there is no folder for the commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoDir {
    /// [`OFF_VAR`] is set.
    Off,
    /// The running `hashi` is not the one the shell finds on `PATH`.
    NotOnPath(Option<PathBuf>),
}

impl std::fmt::Display for NoDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Off => write!(f, "account commands are off ({OFF_VAR} is set)"),
            Self::NotOnPath(Some(exe)) => write!(
                f,
                "this hashi ({}) is not the one your PATH finds, so there is no folder on PATH \
                 to put the commands in",
                exe.display()
            ),
            Self::NotOnPath(None) => write!(f, "the running hashi's location is unknown"),
        }
    }
}

/// The folder for the commands: the `PATH` entry whose `hashi` is the
/// running binary (a symlinked install counts), as the user's `PATH`
/// spells it.
///
/// # Errors
///
/// When [`OFF_VAR`] is set, or the running binary is not on `PATH` (a
/// development build or a test, for example).
pub fn bin_dir() -> Result<PathBuf, NoDir> {
    if std::env::var_os(OFF_VAR).is_some_and(|v| !v.is_empty()) {
        return Err(NoDir::Off);
    }
    found_on_path()
}

/// [`bin_dir`] without looking at [`OFF_VAR`], for `hashi uninstall`, which
/// removes the commands even when they are switched off now.
///
/// # Errors
///
/// When the running binary is not on `PATH`.
pub fn found_on_path() -> Result<PathBuf, NoDir> {
    let exe = std::env::current_exe().ok();
    let canon = exe.as_deref().and_then(|e| std::fs::canonicalize(e).ok());
    let (Some(canon), Some(path)) = (canon, std::env::var_os("PATH")) else {
        return Err(NoDir::NotOnPath(exe));
    };
    let file = if cfg!(windows) { "hashi.exe" } else { "hashi" };
    std::env::split_paths(&path)
        .filter(|d| d.is_absolute())
        .find(|d| std::fs::canonicalize(d.join(file)).is_ok_and(|c| c == canon))
        .ok_or(NoDir::NotOnPath(Some(canon)))
}

/// The `PATH` folders other than `dir`, for the check that a command never
/// hides a program.
fn other_path_dirs(dir: &Path) -> Vec<PathBuf> {
    let own = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .filter(|d| d.is_absolute())
                .filter(|d| std::fs::canonicalize(d).map_or(true, |c| c != own))
                .collect()
        })
        .unwrap_or_default()
}

/// A program named `command` in one of `dirs`.
fn program_in(dirs: &[PathBuf], command: &str, windows: bool) -> Option<PathBuf> {
    let files: Vec<String> = if windows {
        [".exe", ".cmd", ".bat", ".com"]
            .iter()
            .map(|e| format!("{command}{e}"))
            .collect()
    } else {
        vec![command.to_owned()]
    };
    dirs.iter()
        .flat_map(|d| files.iter().map(move |f| d.join(f)))
        .find(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file()))
}

/// The home as written in the marker: the same absolute string the
/// account directories are built from.
fn home_key(home: &Path) -> Option<String> {
    let key = acct::root(home).parent()?.to_string_lossy().into_owned();
    (!key.contains(['\n', '\r'])).then_some(key)
}

/// Whether `a` should have commands, given what is on disk.
fn eligible(home: &Path, spec: &Spec, a: &AccountState) -> bool {
    if a.agent != spec.agent || a.shared_by.is_some() || a.name == acct::DEFAULT {
        return false;
    }
    let dir = acct::account_dir(home, a.agent, &a.name);
    match a.kind {
        AccountKind::Login => std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()),
        AccountKind::ApiKey | AccountKind::OauthToken => {
            spec.keys
                && acct::key_var(a.agent, a.kind).is_some()
                && std::fs::symlink_metadata(dir.join(acct::KEY_FILE)).is_ok_and(|m| m.is_file())
        }
        AccountKind::Default | AccountKind::Remote | AccountKind::Pool => false,
    }
}

/// The variables a command clears before starting the CLI: the agent's
/// [`acct::credential_vars`] that start with one of [`Spec::cleared`].
fn cleared_vars(spec: &Spec) -> Vec<&'static str> {
    acct::credential_vars(spec.agent)
        .iter()
        .copied()
        .filter(|v| !v.ends_with('*') && spec.cleared.iter().any(|p| v.starts_with(p)))
        .collect()
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// What a script needs to know.
struct Script<'a> {
    home_key: &'a str,
    dir: &'a str,
    spec: &'a Spec,
    account: &'a AccountState,
    command: &'a str,
    skip: bool,
}

impl Script<'_> {
    fn label(&self) -> String {
        format!("{}:{}", self.account.agent, self.account.name)
    }

    fn dir_var(&self) -> &'static str {
        acct::dir_var(self.spec.agent).unwrap_or("CLAUDE_CONFIG_DIR")
    }

    fn run(&self) -> String {
        if self.skip {
            format!("{} {}", self.spec.bin, self.spec.skip_flag)
        } else {
            self.spec.bin.to_owned()
        }
    }

    /// The POSIX shell script.
    fn unix(&self) -> String {
        let label = self.label();
        let command = self.command;
        let mut s = String::new();
        s.push_str("#!/bin/sh\n");
        s.push_str(&format!("# {MARKER}{}\n", self.home_key));
        s.push_str(&format!(
            "# Starts {} on the Hashi account {label}. Hashi writes this file and\n\
             # deletes it when the account is removed, so edits here do not last.\n",
            self.spec.agent.display_name()
        ));
        s.push_str(&format!("dir={}\n", sh_quote(self.dir)));
        s.push_str(&format!(
            "if [ ! -d \"$dir\" ]; then\n  \
             echo \"{command}: the Hashi account {label} is gone; see: hashi account list\" >&2\n  \
             exit 1\nfi\n"
        ));
        s.push_str(&format!("unset {}\n", cleared_vars(self.spec).join(" ")));
        if let Some(var) = acct::key_var(self.account.agent, self.account.kind) {
            s.push_str(&format!(
                "if [ ! -r \"$dir/{key}\" ]; then\n  \
                 echo \"{command}: the Hashi account {label} has no key; add it again with hashi account add\" >&2\n  \
                 exit 1\nfi\n\
                 {var}=$(cat \"$dir/{key}\") || exit 1\nexport {var}\n",
                key = acct::KEY_FILE,
            ));
        }
        let var = self.dir_var();
        s.push_str(&format!("{var}=$dir\nexport {var}\n"));
        s.push_str(&format!("exec {} \"$@\"\n", self.run()));
        s
    }

    /// The Windows batch file, with CRLF line ends. `None` when a path
    /// holds a `%`, which `cmd` would expand.
    fn cmd(&self) -> Option<String> {
        if self.dir.contains('%') || self.home_key.contains('%') {
            return None;
        }
        let label = self.label();
        let command = self.command;
        let mut lines = vec![
            "@echo off".to_owned(),
            format!("rem {MARKER}{}", self.home_key),
            format!(
                "rem Starts {} on the Hashi account {label}. Hashi writes this file and",
                self.spec.agent.display_name()
            ),
            "rem deletes it when the account is removed, so edits here do not last.".to_owned(),
            "setlocal".to_owned(),
            format!("set \"HASHI_ACCOUNT_DIR={}\"", self.dir),
            "if not exist \"%HASHI_ACCOUNT_DIR%\\\" (".to_owned(),
            format!(
                "  echo {command}: the Hashi account {label} is gone; see: hashi account list 1>&2"
            ),
            "  exit /b 1".to_owned(),
            ")".to_owned(),
        ];
        lines.extend(
            cleared_vars(self.spec)
                .iter()
                .map(|v| format!("set \"{v}=\"")),
        );
        if let Some(var) = acct::key_var(self.account.agent, self.account.kind) {
            let key = acct::KEY_FILE;
            lines.push(format!("if not exist \"%HASHI_ACCOUNT_DIR%\\{key}\" ("));
            lines.push(format!(
                "  echo {command}: the Hashi account {label} has no key; add it again with hashi account add 1>&2"
            ));
            lines.push("  exit /b 1".to_owned());
            lines.push(")".to_owned());
            lines.push(format!("set /p {var}=<\"%HASHI_ACCOUNT_DIR%\\{key}\""));
        }
        lines.push(format!("set \"{}=%HASHI_ACCOUNT_DIR%\"", self.dir_var()));
        lines.push("set \"HASHI_ACCOUNT_DIR=\"".to_owned());
        lines.push(format!("{} %*", self.run()));
        let mut s = lines.join("\r\n");
        s.push_str("\r\n");
        Some(s)
    }
}

/// The commands `accounts` call for under `names`, for `windows` or a
/// POSIX shell, and the ones that cannot be made, with the reason.
#[must_use]
pub fn wanted(
    home: &Path,
    accounts: &[AccountState],
    names: &Names,
    windows: bool,
) -> (Vec<Wanted>, Vec<(String, String)>) {
    let mut out: Vec<Wanted> = Vec::new();
    let mut skipped = Vec::new();
    let Some(key) = home_key(home) else {
        return (out, skipped);
    };
    for spec in &SPECS {
        for a in accounts.iter().filter(|a| eligible(home, spec, a)) {
            let dir = acct::account_dir(home, a.agent, &a.name);
            let dir = dir.to_string_lossy();
            for skip in [false, true] {
                let Some(command) = names.command(spec, &a.name, skip) else {
                    continue;
                };
                let script = Script {
                    home_key: &key,
                    dir: &dir,
                    spec,
                    account: a,
                    command: &command,
                    skip,
                };
                let script = if windows {
                    script.cmd()
                } else {
                    Some(script.unix())
                };
                let Some(script) = script else {
                    skipped.push((command, "its folder's path holds a %".to_owned()));
                    continue;
                };
                let file = if windows {
                    format!("{command}.cmd")
                } else {
                    command.clone()
                };
                out.push(Wanted {
                    file,
                    command,
                    agent: a.agent,
                    name: a.name.clone(),
                    skip,
                    script,
                });
            }
        }
    }
    // Two accounts whose names make the same command get neither.
    let mut clash: Vec<String> = Vec::new();
    for (i, w) in out.iter().enumerate() {
        if out[i + 1..]
            .iter()
            .any(|o| o.file.eq_ignore_ascii_case(&w.file))
        {
            clash.push(w.file.to_ascii_lowercase());
        }
    }
    out.retain(|w| {
        let bad = clash.contains(&w.file.to_ascii_lowercase());
        if bad {
            skipped.push((
                w.command.clone(),
                "two accounts would get this name; change a pattern".to_owned(),
            ));
        }
        !bad
    });
    (out, skipped)
}

/// Whose a file is, read from its marker.
#[derive(Debug, PartialEq, Eq)]
enum Owner {
    /// Not a small regular file, unreadable, or without a marker.
    Someone,
    /// Written by the Hashi home with this path.
    Hashi(String),
}

fn owner(path: &Path) -> Owner {
    use std::io::Read;
    if !std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && m.len() <= MAX_SCRIPT) {
        return Owner::Someone;
    }
    let Ok(file) = std::fs::File::open(path) else {
        return Owner::Someone;
    };
    let mut head = Vec::new();
    if file.take(HEAD as u64).read_to_end(&mut head).is_err() {
        return Owner::Someone;
    }
    let text = String::from_utf8_lossy(&head);
    text.lines()
        .take(3)
        .find_map(|l| {
            l.split_once(MARKER)
                .map(|(_, home)| home.trim_end().to_owned())
        })
        .map_or(Owner::Someone, Owner::Hashi)
}

/// The files in `dir` that `home` wrote, whatever their names.
#[must_use]
pub fn owned_in(dir: &Path, home: &Path, windows: bool) -> Vec<PathBuf> {
    let Some(key) = home_key(home) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            !name.starts_with('.')
                && (!windows
                    || Path::new(&name)
                        .extension()
                        .is_some_and(|x| x.eq_ignore_ascii_case("cmd")))
        })
        .map(|e| e.path())
        .filter(|p| owner(p) == Owner::Hashi(key.clone()))
        .collect();
    out.sort();
    out
}

/// The commands of account `name` of `agent` that `home` wrote in
/// [`bin_dir`], as typed. Reads at most two files; writes nothing.
#[must_use]
pub fn existing_for(home: &Path, agent: AgentKind, name: &str) -> Vec<String> {
    let (Some(spec), Ok(dir), Some(key)) = (spec(agent), bin_dir(), home_key(home)) else {
        return Vec::new();
    };
    let names = Names::load(home);
    [false, true]
        .into_iter()
        .filter_map(|skip| names.command(spec, name, skip))
        .filter(|c| {
            let file = if cfg!(windows) {
                format!("{c}.cmd")
            } else {
                c.clone()
            };
            owner(&dir.join(file)) == Owner::Hashi(key.clone())
        })
        .collect()
}

fn write_script(dir: &Path, file: &str, script: &str) -> std::io::Result<()> {
    let tmp = dir.join(format!(".{file}.hashi-{}", std::process::id()));
    let result = (|| {
        std::fs::write(&tmp, script)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        }
        std::fs::rename(&tmp, dir.join(file))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn command_of(file: &str, windows: bool) -> String {
    match file.len().checked_sub(4) {
        Some(i) if windows && file.is_char_boundary(i) => file[..i].to_owned(),
        _ => file.to_owned(),
    }
}

/// Makes `dir` hold exactly the commands `accounts` call for under
/// `names`, among the files `home` wrote. Never touches a file another
/// home or anyone else wrote, and never makes a command with the name of
/// a program in `others` (the rest of `PATH`).
#[must_use]
pub fn sync_in(
    dir: &Path,
    home: &Path,
    accounts: &[AccountState],
    names: &Names,
    others: &[PathBuf],
    windows: bool,
) -> Report {
    let mut report = Report {
        dir: dir.to_path_buf(),
        ..Report::default()
    };
    let Some(key) = home_key(home) else {
        return report;
    };
    let (mut wanted, skipped) = wanted(home, accounts, names, windows);
    report.skipped = skipped;
    wanted.retain(|w| match program_in(others, &w.command, windows) {
        Some(p) => {
            report.skipped.push((
                w.command.clone(),
                format!("it would have the same name as {}", p.display()),
            ));
            false
        }
        None => true,
    });
    let same = |a: &str, b: &str| {
        if windows {
            a.eq_ignore_ascii_case(b)
        } else {
            a == b
        }
    };
    for path in owned_in(dir, home, windows) {
        let file = path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        if wanted.iter().any(|w| same(&w.file, &file)) {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => report.removed.push(command_of(&file, windows)),
            Err(e) => report.skipped.push((
                command_of(&file, windows),
                format!("could not delete it: {e}"),
            )),
        }
    }
    for w in wanted {
        let path = dir.join(&w.file);
        let made = Made {
            command: w.command.clone(),
            agent: w.agent,
            name: w.name.clone(),
            skip: w.skip,
        };
        if std::fs::symlink_metadata(&path).is_ok() {
            match owner(&path) {
                Owner::Hashi(h) if h == key => {}
                Owner::Hashi(h) => {
                    report.skipped.push((
                        w.command,
                        format!("another Hashi home ({h}) already has a command by that name"),
                    ));
                    continue;
                }
                Owner::Someone => {
                    report.skipped.push((
                        w.command,
                        format!(
                            "{} is already there and Hashi did not write it",
                            path.display()
                        ),
                    ));
                    continue;
                }
            }
            if std::fs::read_to_string(&path).is_ok_and(|s| s == w.script) {
                report.commands.push(made);
                continue;
            }
        }
        match write_script(dir, &w.file, &w.script) {
            Ok(()) => {
                report.written.push(w.command);
                report.commands.push(made);
            }
            Err(e) => report
                .skipped
                .push((w.command, format!("could not write it: {e}"))),
        }
    }
    report.commands.sort();
    report
}

/// [`sync_in`] on [`bin_dir`] with the accounts in `store` and the saved
/// patterns. Never fails the caller: a problem is in the result.
///
/// # Errors
///
/// When there is no folder for the commands or the accounts cannot be
/// read.
pub fn sync(store: &Store, home: &Path) -> Result<Report, String> {
    let dir = bin_dir().map_err(|e| e.to_string())?;
    let accounts = store
        .list_accounts()
        .map_err(|e| format!("reading the accounts: {e}"))?;
    let others = other_path_dirs(&dir);
    Ok(sync_in(
        &dir,
        home,
        &accounts,
        &Names::load(home),
        &others,
        cfg!(windows),
    ))
}

/// A Claude account row for tests.
#[cfg(test)]
pub(crate) fn test_account(name: &str, kind: AccountKind) -> AccountState {
    AccountState {
        agent: AgentKind::Claude,
        name: name.to_owned(),
        kind,
        disabled: false,
        limited_until: None,
        last_used: None,
        created_at: 0,
        remote: None,
        remote_agent: None,
        disabled_reason: None,
        shared_by: None,
        remind_at: None,
        share_until: None,
        pool_member: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(name: &str, kind: AccountKind) -> AccountState {
        test_account(name, kind)
    }

    fn codex(name: &str, kind: AccountKind) -> AccountState {
        AccountState {
            agent: AgentKind::Codex,
            ..test_account(name, kind)
        }
    }

    struct Setup {
        _tmp: tempfile::TempDir,
        home: PathBuf,
        bin: PathBuf,
    }

    impl Setup {
        fn sync(&self, accounts: &[AccountState]) -> Report {
            self.sync_with(accounts, &Names::default(), false)
        }

        fn sync_with(&self, accounts: &[AccountState], names: &Names, windows: bool) -> Report {
            sync_in(&self.bin, &self.home, accounts, names, &[], windows)
        }
    }

    fn setup() -> Setup {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        Setup {
            _tmp: tmp,
            home,
            bin,
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    fn pair(plain: &str, skip: &str) -> Pair {
        Pair {
            plain: plain.to_owned(),
            skip: skip.to_owned(),
        }
    }

    #[test]
    fn writes_both_commands_for_a_login_and_removes_them_with_it() {
        let s = setup();
        acct::create_dir(&s.home, AgentKind::Claude, "rajat").unwrap();
        let accounts = vec![account("rajat", AccountKind::Login)];
        let r = s.sync(&accounts);
        assert_eq!(r.written, ["rajat-claude", "rajat-dclaude"]);
        assert_eq!(
            r.for_account(AgentKind::Claude, "rajat"),
            ["rajat-claude", "rajat-dclaude"]
        );
        assert!(r.skipped.is_empty(), "{:?}", r.skipped);
        assert_eq!(names(&s.bin), ["rajat-claude", "rajat-dclaude"]);
        assert_eq!(
            r.commands[1].runs(),
            "claude --dangerously-skip-permissions"
        );

        let plain = std::fs::read_to_string(s.bin.join("rajat-claude")).unwrap();
        let dir = acct::account_dir(&s.home, AgentKind::Claude, "rajat");
        assert!(
            plain.contains(&format!("dir='{}'", dir.display())),
            "{plain}"
        );
        assert!(plain.contains("export CLAUDE_CONFIG_DIR"));
        assert!(plain.ends_with("exec claude \"$@\"\n"));
        assert!(plain.contains("unset ANTHROPIC_API_KEY "));
        // Cloud variables are the person's own; only the switches go.
        assert!(!plain.contains("AWS_"));
        assert!(plain.contains("CLAUDE_CODE_USE_BEDROCK"));
        let skip = std::fs::read_to_string(s.bin.join("rajat-dclaude")).unwrap();
        assert!(skip.ends_with("exec claude --dangerously-skip-permissions \"$@\"\n"));

        // Unchanged: nothing is rewritten.
        let again = s.sync(&accounts);
        assert!(again.written.is_empty() && again.removed.is_empty());
        assert_eq!(again.commands, r.commands);

        // The account is removed: its commands go.
        let gone = s.sync(&[]);
        assert_eq!(gone.removed, ["rajat-claude", "rajat-dclaude"]);
        assert!(names(&s.bin).is_empty());
    }

    #[test]
    fn codex_logins_get_codex_commands() {
        let s = setup();
        acct::create_dir(&s.home, AgentKind::Codex, "rajat").unwrap();
        acct::create_dir(&s.home, AgentKind::Claude, "rajat").unwrap();
        acct::write_key(&s.home, AgentKind::Codex, "ci", "sk").unwrap();
        let accounts = vec![
            codex("rajat", AccountKind::Login),
            account("rajat", AccountKind::Login),
            // Codex reads a key from the environment only in `codex exec`.
            codex("ci", AccountKind::ApiKey),
        ];
        let r = s.sync(&accounts);
        assert_eq!(
            names(&s.bin),
            [
                "rajat-claude",
                "rajat-codex",
                "rajat-dclaude",
                "rajat-dcodex"
            ]
        );
        assert_eq!(
            r.for_account(AgentKind::Codex, "rajat"),
            ["rajat-codex", "rajat-dcodex"]
        );
        let text = std::fs::read_to_string(s.bin.join("rajat-dcodex")).unwrap();
        let dir = acct::account_dir(&s.home, AgentKind::Codex, "rajat");
        assert!(text.contains(&format!("dir='{}'", dir.display())));
        assert!(text.contains("CODEX_HOME=$dir\nexport CODEX_HOME\n"));
        assert!(text.contains("unset OPENAI_API_KEY "));
        assert!(!text.contains("ANTHROPIC_"));
        assert!(text.ends_with("exec codex --dangerously-bypass-approvals-and-sandbox \"$@\"\n"));
    }

    #[test]
    fn patterns_rename_and_turn_off_commands() {
        let s = setup();
        acct::create_dir(&s.home, AgentKind::Claude, "rajat").unwrap();
        let accounts = vec![account("rajat", AccountKind::Login)];
        let _ = s.sync(&accounts);
        let mut n = Names::default();
        n.set(AgentKind::Claude, pair("cc-{name}", ""));
        n.check().unwrap();
        let r = s.sync_with(&accounts, &n, false);
        assert_eq!(r.written, ["cc-rajat"]);
        assert_eq!(r.removed, ["rajat-claude", "rajat-dclaude"]);
        assert_eq!(names(&s.bin), ["cc-rajat"]);

        // Back to the defaults, which are stored as nothing saved.
        n.set(AgentKind::Claude, pair("{name}-claude", "{name}-dclaude"));
        assert_eq!(n, Names::default());
        let _ = s.sync_with(&accounts, &n, false);
        assert_eq!(names(&s.bin), ["rajat-claude", "rajat-dclaude"]);
    }

    #[test]
    fn patterns_are_checked() {
        for good in ["{name}-claude", "cc.{name}", "{name}_x", ""] {
            assert!(check_pattern(good).is_ok(), "{good}");
        }
        for bad in [
            "{name}",
            "claude",
            "{name}-{name}",
            "-{name}",
            ".{name}x",
            "{name} x",
            "{name}/x",
            "{name};rm",
            "{name}-éclair",
        ] {
            assert!(check_pattern(bad).is_err(), "{bad}");
        }
        let mut n = Names::default();
        n.set(AgentKind::Codex, pair("{name}-claude", ""));
        assert_eq!(n.check().unwrap_err().0, (AgentKind::Codex, false));
        let mut n = Names::default();
        n.set(AgentKind::Claude, pair("{name}-X", "{name}-x"));
        assert!(n.check().is_err(), "names differing only in case clash");
    }

    #[test]
    fn saved_patterns_load_and_bad_ones_fall_back() {
        let s = setup();
        assert_eq!(Names::load(&s.home), Names::default());
        let mut n = Names::default();
        n.set(AgentKind::Codex, pair("cx-{name}", "cxy-{name}"));
        n.save(&s.home).unwrap();
        assert_eq!(Names::load(&s.home), n);
        // The port saved beside them survives.
        let mut settings = crate::settings::load(&s.home).unwrap();
        settings.port = Some(9000);
        crate::settings::save(&s.home, &settings).unwrap();
        assert_eq!(Names::load(&s.home), n);
        let mut bad = Names::default();
        bad.set(AgentKind::Claude, pair("{name}", ""));
        assert!(bad.save(&s.home).is_err());
        settings.account_commands = Some(bad);
        crate::settings::save(&s.home, &settings).unwrap();
        assert_eq!(Names::load(&s.home), Names::default());
    }

    #[test]
    fn a_command_never_hides_a_program_on_path() {
        let s = setup();
        acct::create_dir(&s.home, AgentKind::Claude, "rajat").unwrap();
        let accounts = vec![account("rajat", AccountKind::Login)];
        let _ = s.sync(&accounts);
        let other = s.home.parent().unwrap().join("usr-bin");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("rajat-dclaude"), "a real program").unwrap();
        let r = sync_in(
            &s.bin,
            &s.home,
            &accounts,
            &Names::default(),
            &[other],
            false,
        );
        assert_eq!(r.removed, ["rajat-dclaude"]);
        assert_eq!(r.skipped[0].0, "rajat-dclaude");
        assert!(r.skipped[0].1.contains("same name"));
        assert_eq!(names(&s.bin), ["rajat-claude"]);
    }

    #[test]
    fn two_accounts_making_one_name_get_neither() {
        let s = setup();
        acct::create_dir(&s.home, AgentKind::Claude, "a").unwrap();
        let mut n = Names::default();
        // Distinct patterns that still make `a-x` twice: claude:a and codex:x.
        n.set(AgentKind::Claude, pair("{name}-x", ""));
        n.set(AgentKind::Codex, pair("a-{name}", ""));
        acct::create_dir(&s.home, AgentKind::Codex, "x").unwrap();
        let accounts = [
            account("a", AccountKind::Login),
            codex("x", AccountKind::Login),
        ];
        let r = s.sync_with(&accounts, &n, false);
        assert!(r.commands.is_empty(), "{:?}", r.commands);
        assert_eq!(r.skipped.len(), 2);
        assert!(names(&s.bin).is_empty());
    }

    #[test]
    fn a_missing_directory_or_key_means_no_commands() {
        let s = setup();
        let accounts = vec![
            account("gone", AccountKind::Login),
            account("nokey", AccountKind::ApiKey),
        ];
        let r = s.sync(&accounts);
        assert!(r.commands.is_empty());
        assert!(names(&s.bin).is_empty());

        acct::create_dir(&s.home, AgentKind::Claude, "gone").unwrap();
        let _ = s.sync(&accounts);
        assert_eq!(names(&s.bin), ["gone-claude", "gone-dclaude"]);
        let dir = acct::account_dir(&s.home, AgentKind::Claude, "gone");
        std::fs::remove_dir(&dir).unwrap();
        let r = s.sync(&accounts);
        assert_eq!(r.removed, ["gone-claude", "gone-dclaude"]);
    }

    #[test]
    fn key_accounts_read_their_key_at_run_time() {
        let s = setup();
        acct::write_key(&s.home, AgentKind::Claude, "ci", "sk-secret").unwrap();
        acct::write_key(&s.home, AgentKind::Claude, "bot", "oauth-secret").unwrap();
        let accounts = vec![
            account("ci", AccountKind::ApiKey),
            account("bot", AccountKind::OauthToken),
        ];
        let r = s.sync(&accounts);
        assert_eq!(r.commands.len(), 4);
        let ci = std::fs::read_to_string(s.bin.join("ci-claude")).unwrap();
        assert!(ci.contains("ANTHROPIC_API_KEY=$(cat \"$dir/key\")"), "{ci}");
        let bot = std::fs::read_to_string(s.bin.join("bot-claude")).unwrap();
        assert!(bot.contains("CLAUDE_CODE_OAUTH_TOKEN=$(cat \"$dir/key\")"));
        for script in [ci, bot] {
            assert!(!script.contains("secret"), "the key is never in the script");
        }
    }

    #[test]
    fn only_local_accounts_get_commands() {
        let s = setup();
        acct::create_dir(&s.home, AgentKind::Gemini, "work").unwrap();
        acct::create_dir(&s.home, AgentKind::Claude, "friend").unwrap();
        acct::write_key(&s.home, AgentKind::Claude, "lent", "k").unwrap();
        let mut gemini = account("work", AccountKind::Login);
        gemini.agent = AgentKind::Gemini;
        let mut remote = account("friend", AccountKind::Remote);
        remote.remote = Some("alice".to_owned());
        let mut shared = account("lent", AccountKind::OauthToken);
        shared.shared_by = Some("alice".to_owned());
        let r = s.sync(&[gemini, remote, shared]);
        assert!(r.commands.is_empty(), "{:?}", r.commands);
        assert!(names(&s.bin).is_empty());
    }

    #[test]
    fn files_hashi_did_not_write_are_never_touched() {
        let s = setup();
        acct::create_dir(&s.home, AgentKind::Claude, "rajat").unwrap();
        std::fs::write(s.bin.join("rajat-claude"), "#!/bin/sh\necho mine\n").unwrap();
        std::fs::write(s.bin.join("other-claude"), "#!/bin/sh\necho mine too\n").unwrap();
        let accounts = vec![account("rajat", AccountKind::Login)];
        let r = s.sync(&accounts);
        assert_eq!(r.written, ["rajat-dclaude"]);
        assert_eq!(r.skipped.len(), 1);
        assert_eq!(r.skipped[0].0, "rajat-claude");
        assert_eq!(
            std::fs::read_to_string(s.bin.join("rajat-claude")).unwrap(),
            "#!/bin/sh\necho mine\n"
        );
        let _ = s.sync(&[]);
        assert_eq!(names(&s.bin), ["other-claude", "rajat-claude"]);
    }

    #[test]
    fn another_homes_commands_are_left_alone() {
        let s = setup();
        let other = s.home.parent().unwrap().join("other-home");
        std::fs::create_dir_all(&other).unwrap();
        acct::create_dir(&other, AgentKind::Claude, "rajat").unwrap();
        acct::create_dir(&s.home, AgentKind::Claude, "rajat").unwrap();
        let accounts = vec![account("rajat", AccountKind::Login)];
        let first = sync_in(&s.bin, &other, &accounts, &Names::default(), &[], false);
        assert_eq!(first.written.len(), 2);
        let before = std::fs::read_to_string(s.bin.join("rajat-claude")).unwrap();

        let r = s.sync(&accounts);
        assert!(r.written.is_empty());
        assert_eq!(r.skipped.len(), 2);
        let _ = s.sync(&[]);
        assert_eq!(
            std::fs::read_to_string(s.bin.join("rajat-claude")).unwrap(),
            before
        );
        assert_eq!(owned_in(&s.bin, &other, false).len(), 2);
        assert!(owned_in(&s.bin, &s.home, false).is_empty());
    }

    #[test]
    fn a_changed_command_is_rewritten() {
        let s = setup();
        acct::create_dir(&s.home, AgentKind::Claude, "rajat").unwrap();
        let accounts = vec![account("rajat", AccountKind::Login)];
        let _ = s.sync(&accounts);
        let path = s.bin.join("rajat-claude");
        let edited = std::fs::read_to_string(&path).unwrap() + "echo edited\n";
        std::fs::write(&path, edited).unwrap();
        let r = s.sync(&accounts);
        assert_eq!(r.written, ["rajat-claude"]);
        assert!(!std::fs::read_to_string(&path).unwrap().contains("edited"));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_in_the_way_is_not_followed() {
        let s = setup();
        acct::create_dir(&s.home, AgentKind::Claude, "rajat").unwrap();
        let target = s.home.parent().unwrap().join("target");
        std::fs::write(&target, "keep").unwrap();
        std::os::unix::fs::symlink(&target, s.bin.join("rajat-claude")).unwrap();
        let r = s.sync(&[account("rajat", AccountKind::Login)]);
        assert_eq!(r.skipped[0].0, "rajat-claude");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
    }

    #[cfg(unix)]
    #[test]
    fn the_script_runs_the_cli_on_the_account() {
        use std::os::unix::fs::PermissionsExt;
        let s = setup();
        acct::create_dir(&s.home, AgentKind::Claude, "rajat").unwrap();
        acct::create_dir(&s.home, AgentKind::Codex, "rajat").unwrap();
        let _ = s.sync(&[
            account("rajat", AccountKind::Login),
            codex("rajat", AccountKind::Login),
        ]);
        let mode = std::fs::metadata(s.bin.join("rajat-dclaude"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);

        // Fake CLIs that print what they were given.
        let fake = s.home.parent().unwrap().join("fake");
        std::fs::create_dir_all(&fake).unwrap();
        for (bin, var, key) in [
            ("claude", "CLAUDE_CONFIG_DIR", "ANTHROPIC_API_KEY"),
            ("codex", "CODEX_HOME", "OPENAI_API_KEY"),
        ] {
            let path = fake.join(bin);
            std::fs::write(
                &path,
                format!("#!/bin/sh\necho \"dir=${var} key=${{{key}-unset}} args=$*\"\n"),
            )
            .unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let run = |command: &str| {
            std::process::Command::new(s.bin.join(command))
                .arg("--resume")
                .env("PATH", format!("{}:/usr/bin:/bin", fake.display()))
                .env("ANTHROPIC_API_KEY", "owner-key")
                .env("OPENAI_API_KEY", "owner-key")
                .output()
                .unwrap()
        };
        let claude_dir = acct::account_dir(&s.home, AgentKind::Claude, "rajat");
        let out = run("rajat-dclaude");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            format!(
                "dir={} key=unset args=--dangerously-skip-permissions --resume",
                claude_dir.display()
            )
        );
        let codex_dir = acct::account_dir(&s.home, AgentKind::Codex, "rajat");
        let out = run("rajat-codex");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            format!("dir={} key=unset args=--resume", codex_dir.display())
        );

        // Once the directory is gone the command says so instead of
        // letting the CLI make a fresh, signed-out one.
        std::fs::remove_dir(&claude_dir).unwrap();
        let out = run("rajat-claude");
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("is gone"));
        assert!(!claude_dir.exists());
    }

    #[test]
    fn windows_scripts_are_batch_files() {
        let s = setup();
        acct::write_key(&s.home, AgentKind::Claude, "ci", "k").unwrap();
        let r = s.sync_with(
            &[account("ci", AccountKind::ApiKey)],
            &Names::default(),
            true,
        );
        assert_eq!(r.commands.len(), 2);
        assert_eq!(names(&s.bin), ["ci-claude.cmd", "ci-dclaude.cmd"]);
        let text = std::fs::read_to_string(s.bin.join("ci-dclaude.cmd")).unwrap();
        assert!(text.starts_with("@echo off\r\n"));
        assert!(text.contains("set /p ANTHROPIC_API_KEY=<"));
        assert!(text.ends_with("claude --dangerously-skip-permissions %*\r\n"));
        let gone = s.sync_with(&[], &Names::default(), true);
        assert_eq!(gone.removed, ["ci-claude", "ci-dclaude"]);
    }
}
