//! One optional shell command that starts Claude Code or Codex without
//! permission prompts, named by the owner.
//!
//! Which login the CLI uses is not decided here: `hashi switch` points the
//! plain `claude` and `codex` commands at an account (see
//! [`crate::accounts::switch`]), so this command just runs the plain CLI
//! with its skip flag, `claude --dangerously-skip-permissions` or `codex
//! --dangerously-bypass-approvals-and-sandbox`, and follows the switch.
//! Nothing is set up by default: a person picks the word (`dclaude`, say)
//! with `hashi account commands --agent claude --name dclaude` or `n` in the
//! dashboard's Accounts popup, and an empty name removes the command.
//!
//! The command is a small script in the folder that holds the `hashi`
//! command the shell finds on `PATH` (usually `~/.local/bin`), so it works
//! in every shell and no shell startup file is edited. [`sync`] makes the
//! folder match the saved names: it writes the chosen commands and deletes
//! the ones Hashi wrote earlier that are no longer wanted, including the
//! per account `<name>-claude` scripts of Hashi 0.4.0. It runs when the
//! names change, when the server starts and on `hashi account commands`.
//!
//! Every script carries a marker line naming the Hashi home that wrote it.
//! Hashi only ever rewrites or deletes files with its own home's marker, so
//! a file of the same name that someone else wrote, or another Hashi home's
//! command, is left alone. A name that is already a program elsewhere on
//! `PATH` is refused, so the command can never hide one. Setting
//! `HASHI_NO_ACCOUNT_COMMANDS` turns the whole thing off.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::accounts as acct;
use crate::agent::AgentKind;

/// Set to anything non-empty to stop Hashi writing the command.
pub const OFF_VAR: &str = "HASHI_NO_ACCOUNT_COMMANDS";

/// Longest name.
pub const MAX_NAME: usize = 32;

/// Start of the marker line's text, followed by the home's path.
const MARKER: &str = "hashi-account-command home=";

/// Most bytes read from a file to find its marker.
const HEAD: usize = 4096;

/// Files bigger than this are never Hashi's commands, so they are not read.
const MAX_SCRIPT: u64 = 16 * 1024;

/// An agent that can have the command, and what it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spec {
    /// The agent.
    pub agent: AgentKind,
    /// The CLI started from `PATH`.
    pub bin: &'static str,
    /// The flag the command adds.
    pub skip_flag: &'static str,
}

/// The agents that can have the command.
pub const SPECS: [Spec; 2] = [
    Spec {
        agent: AgentKind::Claude,
        bin: "claude",
        skip_flag: "--dangerously-skip-permissions",
    },
    Spec {
        agent: AgentKind::Codex,
        bin: "codex",
        skip_flag: "--dangerously-bypass-approvals-and-sandbox",
    },
];

/// The [`Spec`] of `agent`, if it can have the command.
#[must_use]
pub fn spec(agent: AgentKind) -> Option<&'static Spec> {
    SPECS.iter().find(|s| s.agent == agent)
}

/// The names the owner chose, as saved in `settings.json`
/// (`dangerousCommands`). An agent left out has no command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Keywords {
    /// Claude Code's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<String>,
    /// Codex's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex: Option<String>,
}

impl Keywords {
    /// The name chosen for `agent`, if any.
    #[must_use]
    pub fn get(&self, agent: AgentKind) -> Option<&str> {
        match agent {
            AgentKind::Claude => self.claude.as_deref(),
            AgentKind::Codex => self.codex.as_deref(),
            _ => None,
        }
        .filter(|s| !s.is_empty())
    }

    /// Sets `agent`'s name; an empty one removes it.
    pub fn set(&mut self, agent: AgentKind, name: &str) {
        let value = (!name.is_empty()).then(|| name.to_owned());
        match agent {
            AgentKind::Claude => self.claude = value,
            AgentKind::Codex => self.codex = value,
            _ => {}
        }
    }

    /// Checks every name, and that the two differ. The error names the
    /// agent.
    ///
    /// # Errors
    ///
    /// The first bad name and why.
    pub fn check(&self) -> Result<(), (AgentKind, String)> {
        let mut seen: Vec<String> = Vec::new();
        for spec in &SPECS {
            let Some(name) = self.get(spec.agent) else {
                continue;
            };
            check_keyword(name, spec).map_err(|e| (spec.agent, e))?;
            let key = name.to_ascii_lowercase();
            if seen.contains(&key) {
                return Err((
                    spec.agent,
                    format!("`{name}` is already used for the other command"),
                ));
            }
            seen.push(key);
        }
        Ok(())
    }

    /// The saved names of `home`, none when nothing valid is saved.
    #[must_use]
    pub fn load(home: &Path) -> Self {
        crate::settings::load(home)
            .ok()
            .and_then(|s| s.dangerous_commands)
            .filter(|n| n.check().is_ok())
            .unwrap_or_default()
    }

    /// Saves `self` in `home`'s `settings.json`, after checking it.
    ///
    /// # Errors
    ///
    /// When a name is bad or the file cannot be written.
    pub fn save(&self, home: &Path) -> Result<(), String> {
        self.check()
            .map_err(|(agent, e)| format!("{agent} command: {e}"))?;
        let mut settings = crate::settings::load(home).unwrap_or_default();
        settings.dangerous_commands = (*self != Self::default()).then(|| self.clone());
        crate::settings::save(home, &settings).map_err(|e| format!("saving settings: {e}"))
    }
}

/// Checks one name: letters, digits, `.`, `_` and `-`, not starting with
/// `-` or `.`, and not the CLI's own name.
///
/// # Errors
///
/// Why it is not a valid name.
pub fn check_keyword(name: &str, spec: &Spec) -> Result<(), String> {
    if name.len() > MAX_NAME {
        return Err(format!("at most {MAX_NAME} characters"));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err("only letters, digits, `.`, `_` and `-`".to_owned());
    }
    if name.starts_with(['-', '.']) {
        return Err("cannot start with `-` or `.`".to_owned());
    }
    if name.eq_ignore_ascii_case(spec.bin) {
        return Err(format!(
            "`{}` is the CLI itself; pick another word",
            spec.bin
        ));
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
    /// The agent it starts.
    pub agent: AgentKind,
    /// The script.
    pub script: String,
}

/// A command in place.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Made {
    /// What the user types.
    pub command: String,
    /// The agent it starts.
    pub agent: AgentKind,
}

impl Made {
    /// What it runs, such as `claude --dangerously-skip-permissions`.
    #[must_use]
    pub fn runs(&self) -> String {
        spec(self.agent).map_or_else(String::new, |s| format!("{} {}", s.bin, s.skip_flag))
    }
}

/// What [`sync_in`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// The folder.
    pub dir: PathBuf,
    /// Commands in place, sorted.
    pub commands: Vec<Made>,
    /// Commands written or rewritten.
    pub written: Vec<String>,
    /// Commands deleted: not wanted any more.
    pub removed: Vec<String>,
    /// Commands not written, with the reason.
    pub skipped: Vec<(String, String)>,
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

/// The script for `spec`'s command, for `windows` or a POSIX shell.
fn script(home_key: &str, spec: &Spec, command: &str, windows: bool) -> Option<String> {
    let name = spec.agent.display_name();
    if windows {
        if home_key.contains('%') {
            return None;
        }
        let lines = [
            "@echo off".to_owned(),
            format!("rem {MARKER}{home_key}"),
            format!("rem Starts {name} without permission prompts ({command}). Hashi writes this"),
            "rem file and deletes it when the name is changed, so edits here do not last."
                .to_owned(),
            format!("{} {} %*", spec.bin, spec.skip_flag),
        ];
        let mut s = lines.join("\r\n");
        s.push_str("\r\n");
        Some(s)
    } else {
        Some(format!(
            "#!/bin/sh\n# {MARKER}{home_key}\n# Starts {name} without permission prompts ({command}). \
             Hashi writes this file and\n# deletes it when the name is changed, so edits here do not last.\n\
             exec {} {} \"$@\"\n",
            spec.bin, spec.skip_flag
        ))
    }
}

/// The commands `keywords` call for, for `windows` or a POSIX shell, and
/// the ones that cannot be made, with the reason.
#[must_use]
pub fn wanted(
    home: &Path,
    keywords: &Keywords,
    windows: bool,
) -> (Vec<Wanted>, Vec<(String, String)>) {
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    let Some(key) = home_key(home) else {
        return (out, skipped);
    };
    for spec in &SPECS {
        let Some(command) = keywords.get(spec.agent) else {
            continue;
        };
        let Some(script) = script(&key, spec, command, windows) else {
            skipped.push((command.to_owned(), "the home's path holds a %".to_owned()));
            continue;
        };
        out.push(Wanted {
            file: if windows {
                format!("{command}.cmd")
            } else {
                command.to_owned()
            },
            command: command.to_owned(),
            agent: spec.agent,
            script,
        });
    }
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

/// Makes `dir` hold exactly the commands `keywords` call for, among the
/// files `home` wrote. Never touches a file another home or anyone else
/// wrote, and never makes a command with the name of a program in `others`
/// (the rest of `PATH`).
#[must_use]
pub fn sync_in(
    dir: &Path,
    home: &Path,
    keywords: &Keywords,
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
    let (mut wanted, skipped) = wanted(home, keywords, windows);
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

/// [`sync_in`] on [`bin_dir`] with the saved names. Never fails the
/// caller: a problem is in the result.
///
/// # Errors
///
/// When there is no folder for the commands.
pub fn sync(home: &Path) -> Result<Report, String> {
    let dir = bin_dir().map_err(|e| e.to_string())?;
    let others = other_path_dirs(&dir);
    Ok(sync_in(
        &dir,
        home,
        &Keywords::load(home),
        &others,
        cfg!(windows),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Setup {
        _tmp: tempfile::TempDir,
        home: PathBuf,
        bin: PathBuf,
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

    fn files(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    fn kw(claude: &str, codex: &str) -> Keywords {
        let mut k = Keywords::default();
        k.set(AgentKind::Claude, claude);
        k.set(AgentKind::Codex, codex);
        k
    }

    #[test]
    fn nothing_is_made_until_a_word_is_chosen() {
        let s = setup();
        let r = sync_in(&s.bin, &s.home, &Keywords::default(), &[], false);
        assert!(r.commands.is_empty() && r.written.is_empty());
        assert!(files(&s.bin).is_empty());
    }

    #[test]
    fn writes_the_chosen_word_and_follows_renames() {
        let s = setup();
        let r = sync_in(&s.bin, &s.home, &kw("dclaude", ""), &[], false);
        assert_eq!(r.written, ["dclaude"]);
        assert_eq!(
            r.commands[0].runs(),
            "claude --dangerously-skip-permissions"
        );
        let text = std::fs::read_to_string(s.bin.join("dclaude")).unwrap();
        assert!(text.starts_with("#!/bin/sh\n# hashi-account-command home="));
        assert!(text.ends_with("exec claude --dangerously-skip-permissions \"$@\"\n"));
        // It never pins an account: it runs whatever plain claude is.
        assert!(!text.contains("CLAUDE_CONFIG_DIR"));
        // Unchanged: nothing is rewritten.
        let again = sync_in(&s.bin, &s.home, &kw("dclaude", ""), &[], false);
        assert!(again.written.is_empty() && again.removed.is_empty());
        // Renamed: the old one goes.
        let r = sync_in(&s.bin, &s.home, &kw("yolo", "dcodex"), &[], false);
        assert_eq!(r.removed, ["dclaude"]);
        assert_eq!(files(&s.bin), ["dcodex", "yolo"]);
        let codex = std::fs::read_to_string(s.bin.join("dcodex")).unwrap();
        assert!(codex.contains("exec codex --dangerously-bypass-approvals-and-sandbox \"$@\""));
        // Cleared: all gone.
        let _ = sync_in(&s.bin, &s.home, &Keywords::default(), &[], false);
        assert!(files(&s.bin).is_empty());
    }

    #[test]
    fn the_per_account_scripts_of_0_4_0_are_cleaned_up() {
        let s = setup();
        let key = home_key(&s.home).unwrap();
        let old = format!("#!/bin/sh\n# {MARKER}{key}\nexec claude \"$@\"\n");
        std::fs::write(s.bin.join("rajat-claude"), &old).unwrap();
        std::fs::write(s.bin.join("mine"), "someone else's").unwrap();
        let r = sync_in(&s.bin, &s.home, &Keywords::default(), &[], false);
        assert_eq!(r.removed, ["rajat-claude"]);
        assert_eq!(files(&s.bin), ["mine"]);
    }

    #[test]
    fn never_overwrites_a_foreign_file_or_hides_a_program() {
        let s = setup();
        std::fs::write(s.bin.join("dclaude"), "someone else's").unwrap();
        let r = sync_in(&s.bin, &s.home, &kw("dclaude", ""), &[], false);
        assert!(r.commands.is_empty() && r.skipped.len() == 1);
        assert_eq!(
            std::fs::read_to_string(s.bin.join("dclaude")).unwrap(),
            "someone else's"
        );
        let other = s.bin.parent().unwrap().join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("yolo"), "").unwrap();
        let r = sync_in(&s.bin, &s.home, &kw("yolo", ""), &[other], false);
        assert!(r.commands.is_empty());
        assert!(r.skipped[0].1.contains("same name"));
    }

    #[test]
    fn windows_makes_a_cmd_file() {
        let s = setup();
        let r = sync_in(&s.bin, &s.home, &kw("dclaude", ""), &[], true);
        assert_eq!(r.written, ["dclaude"]);
        let text = std::fs::read_to_string(s.bin.join("dclaude.cmd")).unwrap();
        assert!(text.contains("\r\nclaude --dangerously-skip-permissions %*\r\n"));
    }

    #[test]
    fn checks_the_words() {
        assert!(kw("dclaude", "dcodex").check().is_ok());
        assert!(kw("claude", "").check().is_err(), "the CLI's own name");
        assert!(kw("a b", "").check().is_err());
        assert!(kw("-x", "").check().is_err());
        assert!(kw("same", "same").check().is_err());
        assert!(kw("", "").check().is_ok());
    }

    #[test]
    fn saves_and_loads_through_settings() {
        let s = setup();
        assert_eq!(Keywords::load(&s.home), Keywords::default());
        kw("dclaude", "").save(&s.home).unwrap();
        assert_eq!(
            Keywords::load(&s.home).get(AgentKind::Claude),
            Some("dclaude")
        );
        assert!(kw("bad word", "").save(&s.home).is_err());
        Keywords::default().save(&s.home).unwrap();
        assert_eq!(Keywords::load(&s.home), Keywords::default());
    }
}
