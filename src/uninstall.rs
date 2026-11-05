//! `hashi uninstall`: remove what Hashi put on this machine, and nothing else.
//!
//! The command builds an inventory first (the binary, a leftover
//! `hashi.old.exe`, the Hashi home and, on Windows, the user PATH entry the
//! installer added), prints it, asks for confirmation and only then deletes.
//! It refuses while a server is running from the same home, and refuses a
//! home that does not look like one Hashi created. Nothing is followed
//! through a symlink.

use std::io::{ErrorKind, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::diagnostics::terminal_safe;
use crate::util::human_bytes;

/// What the user types to confirm.
pub const CONFIRM_WORD: &str = "uninstall";

/// Name of the Hashi home under the user's home directory.
pub const DEFAULT_HOME_NAME: &str = ".hashi";

/// Flags of `hashi uninstall`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Leave the Hashi home in place; remove only the binary and PATH entry.
    pub keep_data: bool,
    /// Do not ask for confirmation.
    pub yes: bool,
    /// Print the inventory and stop.
    pub dry_run: bool,
}

// ── safety checks on the home ──────────────────────────────────────────────

/// `true` for a top-level name that Hashi itself creates in its home.
#[must_use]
pub fn is_hashi_entry(name: &str) -> bool {
    const EXACT: &[&str] = &[
        "hashi.db",
        "hashi.db-wal",
        "hashi.db-shm",
        "hashi.db-journal",
        "logs",
        "workspaces",
        crate::accounts::DIR,
        crate::remote::REMOTES_FILE,
        crate::update::CACHE_FILE,
        crate::server::PUBLIC_URL_FILE,
        crate::server::PID_FILE,
        "cloudflared.yml",
        ".DS_Store",
    ];
    EXACT.contains(&name)
        // `remote::save` writes through a temporary file.
        || name.starts_with(".remotes.json.")
        // `hashi update` downloads into `update-<uuid>`.
        || name.strip_prefix("update-").is_some_and(|rest| {
            !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_hexdigit())
        })
}

/// The top-level names in `dir`, sorted.
fn entry_names(dir: &Path) -> Result<Vec<String>> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    Ok(names)
}

fn listing(names: &[String]) -> String {
    const SHOWN: usize = 20;
    let mut out: Vec<String> = names
        .iter()
        .take(SHOWN)
        .map(|n| format!("  {}", terminal_safe(n)))
        .collect();
    if names.len() > SHOWN {
        out.push(format!("  ... and {} more", names.len() - SHOWN));
    }
    out.join("\n")
}

/// Checks that `home` is safe to delete. Returns `false` when it does not
/// exist.
///
/// It must be a real directory (not a symlink), must not be `/`, the user's
/// home directory or one of its ancestors, and must look like a Hashi home:
/// it is `default_home`, or it holds `hashi.db` and nothing Hashi did not
/// create.
///
/// # Errors
///
/// Fails, listing what was found, when any check does not hold.
pub fn check_home(
    home: &Path,
    default_home: Option<&Path>,
    user_home: Option<&Path>,
) -> Result<bool> {
    let shown = terminal_safe(&home.display().to_string());
    let meta = match std::fs::symlink_metadata(home) {
        Ok(m) => m,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e).with_context(|| format!("reading {shown}")),
    };
    if meta.file_type().is_symlink() {
        bail!("{shown} is a symlink; Hashi will not delete through it. Remove it yourself.");
    }
    if !meta.is_dir() {
        bail!("{shown} is not a directory; Hashi will not delete it.");
    }
    let canon = home
        .canonicalize()
        .with_context(|| format!("resolving {shown}"))?;
    if canon.parent().is_none() {
        bail!("{shown} is a filesystem root; Hashi will not delete it.");
    }
    if let Some(user) = user_home {
        let user = user.canonicalize().unwrap_or_else(|_| user.to_path_buf());
        if user.starts_with(&canon) {
            bail!("{shown} is your home directory or holds it; Hashi will not delete it.");
        }
    }
    let is_default =
        default_home.is_some_and(|d| d == home || d.canonicalize().is_ok_and(|d| d == canon));
    if is_default {
        return Ok(true);
    }
    let names = entry_names(home)?;
    if !names.iter().any(|n| n == "hashi.db") {
        bail!(
            "{shown} does not look like a Hashi home (it has no hashi.db), so nothing was \
             removed. It holds:\n{}",
            listing(&names)
        );
    }
    let unrelated: Vec<String> = names.into_iter().filter(|n| !is_hashi_entry(n)).collect();
    if !unrelated.is_empty() {
        bail!(
            "{shown} holds files Hashi did not create, so nothing was removed. Move them \
             out first:\n{}",
            listing(&unrelated)
        );
    }
    Ok(true)
}

/// Total size of the tree at `path`. Symlinks count as themselves and are
/// never followed.
#[must_use]
pub fn tree_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| tree_size(&e.path()))
                .sum()
        })
        .unwrap_or(0)
}

/// Deletes the Hashi home. The entry is checked again without following
/// symlinks, and `remove_dir_all` does not follow symlinks inside the tree,
/// the same way [`crate::server::remove_token_workspace`] works.
fn remove_home(home: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(home) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(home),
        Ok(_) => Err(std::io::Error::other("the home is no longer a directory")),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

// ── running server ─────────────────────────────────────────────────────────

/// `true` when `name` is a Hashi executable's name (`hashi`, `hashi.exe`,
/// or a build such as `hashi-1a2b3c`).
fn hashi_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "hashi" || name.starts_with("hashi.") || name.starts_with("hashi-")
}

/// `true` when `pid` is alive and is a Hashi process.
#[must_use]
pub fn is_hashi_process(pid: u32) -> bool {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let pid = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet),
    );
    sys.process(pid).is_some_and(|p| {
        hashi_name(&p.name().to_string_lossy())
            || p.exe()
                .and_then(Path::file_name)
                .is_some_and(|n| hashi_name(&n.to_string_lossy()))
    })
}

/// The pid of a server running from `home`, if `<home>/server.pid` names a
/// live Hashi process other than `own_pid`. A stale file is ignored.
#[must_use]
pub fn running_server(home: &Path, own_pid: u32, is_hashi: &dyn Fn(u32) -> bool) -> Option<u32> {
    crate::server::read_server_pid(home).filter(|&pid| pid != own_pid && is_hashi(pid))
}

// ── binary and PATH ────────────────────────────────────────────────────────

/// How the binary is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryKind {
    /// Hashi deletes it.
    Delete,
    /// Installed by Cargo: left for `cargo uninstall hashi`.
    Cargo,
    /// Installed by Homebrew: left for `brew uninstall hashi`.
    Homebrew,
}

/// The Hashi binary.
#[derive(Debug, Clone)]
pub struct Binary {
    /// Its path.
    pub path: PathBuf,
    /// Its size.
    pub size: u64,
    /// What happens to it.
    pub kind: BinaryKind,
}

fn binary_kind(exe: &Path) -> BinaryKind {
    if crate::update::refuse_cargo_install(exe).is_err() {
        BinaryKind::Cargo
    } else if exe.components().any(|c| c.as_os_str() == "Cellar") {
        BinaryKind::Homebrew
    } else {
        BinaryKind::Delete
    }
}

/// The directory `install.ps1` installs to by default:
/// `%LOCALAPPDATA%\Programs\hashi\bin`.
#[must_use]
pub fn windows_install_dir(local_app_data: &Path) -> PathBuf {
    local_app_data.join("Programs").join("hashi").join("bin")
}

/// `path` (a `;`-separated Windows PATH) without the entries equal to `dir`,
/// compared without regard to case, as PowerShell's `-contains` does in
/// `install.ps1`. Empty entries are dropped, as the installer does. `None`
/// when `dir` is not in it.
#[must_use]
pub fn without_path_entry(path: &str, dir: &str) -> Option<String> {
    let parts: Vec<&str> = path.split(';').filter(|p| !p.is_empty()).collect();
    let kept: Vec<&str> = parts
        .iter()
        .copied()
        .filter(|p| !p.eq_ignore_ascii_case(dir))
        .collect();
    (kept.len() != parts.len()).then(|| kept.join(";"))
}

// ── inventory ──────────────────────────────────────────────────────────────

/// Explicit paths, so tests never touch the real machine.
#[derive(Debug, Clone, Default)]
pub struct Target {
    /// The running binary, canonicalised.
    pub exe: Option<PathBuf>,
    /// The Hashi home.
    pub home: PathBuf,
    /// `~/.hashi`.
    pub default_home: Option<PathBuf>,
    /// The user's home directory.
    pub user_home: Option<PathBuf>,
    /// Windows: the user PATH and the installer's directory.
    pub user_path: Option<(String, PathBuf)>,
}

/// Everything `hashi uninstall` would remove.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    /// The binary.
    pub binary: Option<Binary>,
    /// A `hashi.old.exe` left next to it by `hashi update`.
    pub old_exe: Option<(PathBuf, u64)>,
    /// The home and its size, when it is removed.
    pub home: Option<(PathBuf, u64)>,
    /// The home, when `--keep-data` leaves it.
    pub kept_home: Option<PathBuf>,
    /// The user PATH entry to remove, and the PATH without it.
    pub path_entry: Option<(String, String)>,
    /// Saved remotes (name, base URL), read before the home goes.
    pub remotes: Vec<(String, String)>,
    /// A problem reading `remotes.json`.
    pub remotes_error: Option<String>,
}

impl Plan {
    /// `true` when there is nothing to remove.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.binary
            .as_ref()
            .is_none_or(|b| b.kind != BinaryKind::Delete)
            && self.old_exe.is_none()
            && self.home.is_none()
            && self.path_entry.is_none()
    }
}

/// Builds the inventory.
///
/// # Errors
///
/// Fails when the home is unsafe to delete (see [`check_home`]); nothing is
/// removed then.
pub fn plan(t: &Target, keep_data: bool) -> Result<Plan> {
    let mut plan = Plan::default();
    if let Some(exe) = &t.exe
        && let Ok(meta) = std::fs::symlink_metadata(exe)
        && meta.is_file()
    {
        plan.binary = Some(Binary {
            path: exe.clone(),
            size: meta.len(),
            kind: binary_kind(exe),
        });
        if let Some(dir) = exe.parent() {
            let old = dir.join(crate::update::OLD_EXE);
            if let Ok(m) = std::fs::symlink_metadata(&old)
                && m.is_file()
            {
                plan.old_exe = Some((old, m.len()));
            }
        }
    }
    if let (Some((path, dir)), Some(exe)) = (&t.user_path, &t.exe) {
        let dir_s = dir.to_string_lossy();
        let holds_exe = exe
            .parent()
            .is_some_and(|p| p.to_string_lossy().eq_ignore_ascii_case(&dir_s));
        if holds_exe && let Some(rest) = without_path_entry(path, &dir_s) {
            plan.path_entry = Some((dir_s.into_owned(), rest));
        }
    }
    if keep_data {
        if t.home.exists() {
            plan.kept_home = Some(t.home.clone());
        }
        return Ok(plan);
    }
    if check_home(&t.home, t.default_home.as_deref(), t.user_home.as_deref())? {
        match crate::remote::load(&t.home) {
            Ok(r) => plan.remotes = r.into_iter().map(|r| (r.name, r.base_url)).collect(),
            Err(e) => plan.remotes_error = Some(format!("{e:#}")),
        }
        plan.home = Some((t.home.clone(), tree_size(&t.home)));
    }
    Ok(plan)
}

/// The inventory as printed before confirming.
#[must_use]
pub fn describe(plan: &Plan) -> String {
    let p = |path: &Path| terminal_safe(&path.display().to_string());
    let mut out = Vec::new();
    if let Some(b) = &plan.binary {
        match b.kind {
            BinaryKind::Delete => out.push(format!(
                "  binary      {}  ({})",
                p(&b.path),
                human_bytes(b.size)
            )),
            BinaryKind::Cargo => out.push(format!(
                "  binary      {}  (installed by Cargo; left for `cargo uninstall hashi`)",
                p(&b.path)
            )),
            BinaryKind::Homebrew => out.push(format!(
                "  binary      {}  (installed by Homebrew; left for `brew uninstall hashi`)",
                p(&b.path)
            )),
        }
    }
    if let Some((path, size)) = &plan.old_exe {
        out.push(format!(
            "  old binary  {}  ({})",
            p(path),
            human_bytes(*size)
        ));
    }
    if let Some((path, size)) = &plan.home {
        out.push(format!(
            "  data        {}  ({}: database, logs, workspaces, saved remotes)",
            p(path),
            human_bytes(*size)
        ));
    }
    if let Some(path) = &plan.kept_home {
        out.push(format!("  data        {}  (kept: --keep-data)", p(path)));
    }
    if let Some((dir, _)) = &plan.path_entry {
        out.push(format!(
            "  PATH entry  {}  (your user PATH)",
            terminal_safe(dir)
        ));
    }
    if out.is_empty() {
        out.push("  nothing found".to_owned());
    }
    out.join("\n")
}

/// What was removed, and what failed.
#[derive(Debug, Default)]
pub struct Removed {
    /// Paths removed.
    pub done: Vec<PathBuf>,
    /// Failures, as `path: error`.
    pub failed: Vec<String>,
}

fn delete_binary(path: &Path, current_exe: Option<&Path>) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        if current_exe == Some(path) {
            // A running exe cannot delete itself on Windows; self-replace
            // schedules the delete for when this process exits.
            return self_replace::self_delete_at(path);
        }
    }
    let _ = current_exe;
    std::fs::remove_file(path)
}

/// Removes the files and directories in `plan`: the home first, then the old
/// binary, then the binary. `current_exe` is the running binary, which
/// Windows deletes after exit. The PATH entry is not touched here.
pub fn remove_files(plan: &Plan, current_exe: Option<&Path>) -> Removed {
    let mut r = Removed::default();
    let mut record = |path: &Path, result: std::io::Result<()>| match result {
        Ok(()) => r.done.push(path.to_path_buf()),
        Err(e) => r.failed.push(format!("{}: {e}", path.display())),
    };
    if let Some((home, _)) = &plan.home {
        record(home, remove_home(home));
    }
    if let Some((old, _)) = &plan.old_exe {
        record(old, std::fs::remove_file(old));
    }
    if let Some(b) = &plan.binary
        && b.kind == BinaryKind::Delete
    {
        record(&b.path, delete_binary(&b.path, current_exe));
    }
    r
}

// ── what is left ───────────────────────────────────────────────────────────

/// How to remove a helper program (`cloudflared`, `tailscale`) found at
/// `path` on `os` (`std::env::consts::OS`).
#[must_use]
pub fn removal_hint(tool: &str, path: &Path, os: &str) -> String {
    let s = path.to_string_lossy();
    let brew = s.contains("/Cellar/")
        || s.starts_with("/opt/homebrew/")
        || s.starts_with("/home/linuxbrew/")
        || (os == "macos" && s.starts_with("/usr/local/bin/"));
    if brew {
        return format!("brew uninstall {tool}");
    }
    match (os, tool) {
        ("windows", "cloudflared") => "winget uninstall --id Cloudflare.cloudflared".to_owned(),
        ("windows", "tailscale") => "winget uninstall --id Tailscale.Tailscale".to_owned(),
        ("windows", _) => format!("winget uninstall {tool}"),
        ("macos", _) => format!("remove {tool} the way you installed it"),
        _ => format!("sudo apt remove {tool} (or your package manager's equivalent)"),
    }
}

/// The notes printed afterwards about what Hashi leaves alone.
#[must_use]
pub fn leftovers(plan: &Plan, helpers: &[(&str, PathBuf)], os: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (tool, path) in helpers {
        out.push(format!(
            "{tool} is still installed at {}. To remove it: {}",
            terminal_safe(&path.display().to_string()),
            removal_hint(tool, path, os)
        ));
    }
    if !plan.remotes.is_empty() {
        out.push(
            "MCP entries for your saved remotes are still in your own tools. Remove them with:"
                .to_owned(),
        );
        for (name, _) in &plan.remotes {
            let name = terminal_safe(name);
            out.push(format!("  claude mcp remove {name}"));
            out.push(format!("  codex mcp remove {name}"));
        }
        out.push(
            "Your tokens on these bridges stay valid until their owners revoke them:".to_owned(),
        );
        for (name, url) in &plan.remotes {
            out.push(format!("  {}  {}", terminal_safe(name), terminal_safe(url)));
        }
    }
    if let Some(e) = &plan.remotes_error {
        out.push(format!(
            "Could not read the saved remotes ({}); check `claude mcp list` and \
             `codex mcp list` for Hashi entries.",
            terminal_safe(e)
        ));
    }
    out
}

// ── Windows PATH ───────────────────────────────────────────────────────────

#[cfg(windows)]
fn powershell() -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    Path::new(&root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe")
}

/// The user PATH, read the way `install.ps1` reads it.
#[cfg(windows)]
fn read_user_path() -> Option<String> {
    let out = std::process::Command::new(powershell())
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Environment]::GetEnvironmentVariable('Path', 'User')",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Writes the user PATH the way `install.ps1` does. The value travels in an
/// environment variable, so it is never parsed as PowerShell.
#[cfg(windows)]
fn write_user_path(value: &str) -> Result<()> {
    let status = std::process::Command::new(powershell())
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Environment]::SetEnvironmentVariable('Path', $env:HASHI_NEW_PATH, 'User')",
        ])
        .env("HASHI_NEW_PATH", value)
        .stdin(std::process::Stdio::null())
        .status()
        .context("running PowerShell")?;
    if !status.success() {
        bail!("PowerShell could not update your user PATH ({status})");
    }
    Ok(())
}

#[cfg(not(windows))]
fn write_user_path(_value: &str) -> Result<()> {
    Ok(())
}

// ── hashi uninstall ────────────────────────────────────────────────────────

fn current_target(home: &Path) -> Target {
    let user_home = dirs::home_dir();
    #[cfg(windows)]
    let user_path = read_user_path()
        .zip(std::env::var_os("LOCALAPPDATA").map(|d| windows_install_dir(Path::new(&d))));
    #[cfg(not(windows))]
    let user_path = None;
    // Windows canonical paths carry a `\\?\` prefix that no PATH entry
    // has, so the path is left as Windows reports it.
    let exe = std::env::current_exe().ok().map(|p| {
        if cfg!(windows) {
            p
        } else {
            p.canonicalize().unwrap_or(p)
        }
    });
    Target {
        exe,
        home: home.to_path_buf(),
        default_home: user_home.as_ref().map(|h| h.join(DEFAULT_HOME_NAME)),
        user_home,
        user_path,
    }
}

fn confirmed() -> Result<bool> {
    eprint!("Type {CONFIRM_WORD} to remove these, or anything else to stop: ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(answer.trim() == CONFIRM_WORD)
}

/// How an uninstall ended.
#[derive(Debug)]
pub enum Outcome {
    /// `--dry-run`: the inventory was printed.
    DryRun(Plan),
    /// The user did not confirm.
    Declined(Plan),
    /// Removals ran.
    Done(Plan, Removed),
}

/// Runs `hashi uninstall` for `home`.
///
/// # Errors
///
/// As for [`execute`].
pub fn run(home: &Path, opts: Options) -> Result<()> {
    let target = current_target(home);
    let tty = std::io::stdin().is_terminal();
    let outcome = execute(&target, opts, &is_hashi_process, tty, &mut confirmed)?;
    let (plan, removed) = match outcome {
        Outcome::DryRun(_) => {
            println!("Dry run: nothing was removed.");
            return Ok(());
        }
        Outcome::Declined(_) => {
            println!("Nothing was removed.");
            return Ok(());
        }
        Outcome::Done(plan, removed) => (plan, removed),
    };
    for path in &removed.done {
        println!("removed {}", terminal_safe(&path.display().to_string()));
    }
    for f in &removed.failed {
        eprintln!("could not remove {}", terminal_safe(f));
    }
    if plan.is_empty() {
        println!("Nothing to remove.");
    } else if removed.failed.is_empty() {
        println!("Hashi is uninstalled.");
    }
    if cfg!(windows)
        && plan
            .binary
            .as_ref()
            .is_some_and(|b| b.kind == BinaryKind::Delete)
    {
        println!("The binary is deleted once this command exits.");
    }
    print_leftovers(&plan);
    if !removed.failed.is_empty() {
        bail!("some items could not be removed");
    }
    Ok(())
}

/// The uninstall itself, on explicit paths: refuses while a server runs
/// from the home, builds and prints the inventory, stops for `--dry-run`,
/// confirms (unless `--yes`) and removes. `tty` says whether stdin is a
/// terminal; `confirm` asks the user.
///
/// # Errors
///
/// Fails while a live Hashi server owns `<home>/server.pid`, when the home is
/// unsafe to delete, or without a terminal and without `--yes`. Nothing is
/// removed then.
pub fn execute(
    target: &Target,
    opts: Options,
    is_hashi: &dyn Fn(u32) -> bool,
    tty: bool,
    confirm: &mut dyn FnMut() -> Result<bool>,
) -> Result<Outcome> {
    if let Some(pid) = running_server(&target.home, std::process::id(), is_hashi) {
        let msg = format!(
            "a Hashi server is running from this home (pid {pid}). Quit the dashboard \
             (press q) or stop the service, then run hashi uninstall again."
        );
        if !opts.dry_run {
            bail!("{msg}");
        }
        eprintln!("note: {msg}");
    }
    let plan = plan(target, opts.keep_data)?;
    println!("hashi uninstall will remove:");
    println!("{}", describe(&plan));
    if opts.dry_run {
        return Ok(Outcome::DryRun(plan));
    }
    if plan.is_empty() {
        return Ok(Outcome::Done(plan, Removed::default()));
    }
    if !opts.yes {
        if !tty {
            bail!("not removing anything without a terminal; pass --yes to confirm");
        }
        if !confirm()? {
            return Ok(Outcome::Declined(plan));
        }
    }
    let mut removed = remove_files(&plan, target.exe.as_deref());
    if let Some((dir, rest)) = &plan.path_entry {
        match write_user_path(rest) {
            Ok(()) => removed
                .done
                .push(PathBuf::from(format!("{dir} (user PATH)"))),
            Err(e) => removed.failed.push(format!("{dir} (user PATH): {e:#}")),
        }
    }
    Ok(Outcome::Done(plan, removed))
}

fn print_leftovers(plan: &Plan) {
    if let Some(b) = &plan.binary {
        match b.kind {
            BinaryKind::Cargo => println!("Remove the binary with: cargo uninstall hashi"),
            BinaryKind::Homebrew => println!("Remove the binary with: brew uninstall hashi"),
            BinaryKind::Delete => {}
        }
    }
    let helpers: Vec<(&str, PathBuf)> = ["cloudflared", "tailscale"]
        .into_iter()
        .filter_map(|t| crate::agent::locate(t).map(|p| (t, p)))
        .collect();
    let notes = leftovers(plan, &helpers, std::env::consts::OS);
    if !notes.is_empty() {
        println!();
        println!("Hashi leaves these alone:");
        for n in notes {
            println!("{n}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap_or_else(|e| unreachable!("tempdir: {e}"))
    }

    fn write(path: &Path, text: &str) {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).unwrap_or_else(|e| unreachable!("mkdir: {e}"));
        }
        std::fs::write(path, text).unwrap_or_else(|e| unreachable!("write: {e}"));
    }

    /// A fake installation: `<root>/bin/hashi` and a populated `<root>/home`.
    fn fake_install(root: &Path) -> Target {
        let exe = root.join("bin").join("hashi");
        write(&exe, "not really a binary");
        let home = root.join("home");
        write(&home.join("hashi.db"), "db");
        write(&home.join("logs").join("hashi.log"), "log line");
        write(
            &home
                .join("workspaces")
                .join("tok-1")
                .join("claude")
                .join("a.txt"),
            "x",
        );
        write(
            &home.join(crate::server::PUBLIC_URL_FILE),
            "https://x.example.com\n",
        );
        write(
            &home.join(crate::remote::REMOTES_FILE),
            r#"{"remotes":[{"name":"alice","baseUrl":"https://alice.example.com","token":"t"}]}"#,
        );
        Target {
            exe: Some(exe),
            home,
            default_home: None,
            user_home: Some(root.join("user")),
            user_path: None,
        }
    }

    fn err_text<T: std::fmt::Debug>(r: Result<T>) -> String {
        match r {
            Ok(v) => unreachable!("expected an error, got {v:?}"),
            Err(e) => format!("{e:#}"),
        }
    }

    #[test]
    fn inventory_lists_binary_home_and_remotes() {
        let root = tmp();
        let t = fake_install(root.path());
        write(&root.path().join("bin").join(crate::update::OLD_EXE), "old");
        let p = plan(&t, false).unwrap_or_else(|e| unreachable!("{e:#}"));
        let b = p.binary.as_ref().unwrap_or_else(|| unreachable!());
        assert_eq!(b.kind, BinaryKind::Delete);
        assert_eq!(b.size, "not really a binary".len() as u64);
        assert!(p.old_exe.is_some());
        let (home, size) = p.home.as_ref().unwrap_or_else(|| unreachable!());
        assert_eq!(home, &t.home);
        assert!(*size > 0);
        assert_eq!(
            p.remotes,
            vec![("alice".to_owned(), "https://alice.example.com".to_owned())]
        );
        let text = describe(&p);
        assert!(text.contains("binary"), "{text}");
        assert!(text.contains("old binary"), "{text}");
        assert!(text.contains("data"), "{text}");
        let notes = leftovers(&p, &[], "linux").join("\n");
        assert!(notes.contains("claude mcp remove alice"), "{notes}");
        assert!(notes.contains("codex mcp remove alice"), "{notes}");
        assert!(notes.contains("https://alice.example.com"), "{notes}");
    }

    #[test]
    fn removes_a_fake_binary_and_home() {
        let root = tmp();
        let t = fake_install(root.path());
        let p = plan(&t, false).unwrap_or_else(|e| unreachable!("{e:#}"));
        let r = remove_files(&p, None);
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        assert_eq!(r.done.len(), 2);
        assert!(!t.home.exists());
        assert!(t.exe.as_ref().is_some_and(|e| !e.exists()));
        // Only what was listed: the binary's directory stays.
        assert!(root.path().join("bin").is_dir());
    }

    fn never_asked() -> Result<bool> {
        unreachable!("confirm should not be asked")
    }

    fn intact(t: &Target) -> bool {
        t.home.join("hashi.db").is_file() && t.exe.as_ref().is_some_and(|e| e.is_file())
    }

    #[test]
    fn dry_run_deletes_nothing() {
        let root = tmp();
        let t = fake_install(root.path());
        let opts = Options {
            dry_run: true,
            yes: true,
            ..Options::default()
        };
        let out = execute(&t, opts, &|_| false, false, &mut never_asked);
        assert!(matches!(out, Ok(Outcome::DryRun(_))), "{out:?}");
        assert!(intact(&t));
    }

    #[test]
    fn execute_removes_with_yes() {
        let root = tmp();
        let t = fake_install(root.path());
        let opts = Options {
            yes: true,
            ..Options::default()
        };
        let out = execute(&t, opts, &|_| false, false, &mut never_asked);
        match out {
            Ok(Outcome::Done(_, r)) => assert!(r.failed.is_empty(), "{:?}", r.failed),
            other => unreachable!("{other:?}"),
        }
        assert!(!t.home.exists());
        assert!(t.exe.as_ref().is_some_and(|e| !e.exists()));
    }

    #[test]
    fn execute_needs_a_terminal_or_yes_and_honours_a_no() {
        let root = tmp();
        let t = fake_install(root.path());
        let e = err_text(execute(
            &t,
            Options::default(),
            &|_| false,
            false,
            &mut never_asked,
        ));
        assert!(e.contains("--yes"), "{e}");
        let out = execute(&t, Options::default(), &|_| false, true, &mut || Ok(false));
        assert!(matches!(out, Ok(Outcome::Declined(_))), "{out:?}");
        assert!(intact(&t));
    }

    #[test]
    fn execute_refuses_while_a_server_runs() {
        let root = tmp();
        let t = fake_install(root.path());
        write(&t.home.join(crate::server::PID_FILE), "4242\n");
        let opts = Options {
            yes: true,
            ..Options::default()
        };
        let e = err_text(execute(
            &t,
            opts,
            &|pid| pid == 4242,
            true,
            &mut never_asked,
        ));
        assert!(e.contains("pid 4242"), "{e}");
        assert!(intact(&t));
        // A stale file does not block.
        let out = execute(&t, opts, &|_| false, true, &mut never_asked);
        assert!(matches!(out, Ok(Outcome::Done(..))), "{out:?}");
        assert!(!t.home.exists());
    }

    #[test]
    fn keep_data_removes_only_the_binary() {
        let root = tmp();
        let t = fake_install(root.path());
        let p = plan(&t, true).unwrap_or_else(|e| unreachable!("{e:#}"));
        assert!(p.home.is_none());
        assert_eq!(p.kept_home.as_ref(), Some(&t.home));
        assert!(p.remotes.is_empty());
        let r = remove_files(&p, None);
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        assert!(t.home.join("hashi.db").is_file());
        assert!(t.exe.as_ref().is_some_and(|e| !e.exists()));
    }

    #[test]
    fn keep_data_skips_the_home_checks() {
        let root = tmp();
        let mut t = fake_install(root.path());
        t.home = root.path().to_path_buf();
        assert!(plan(&t, true).is_ok());
    }

    #[test]
    fn leaves_a_cargo_binary() {
        let root = tmp();
        let mut t = fake_install(root.path());
        let exe = root.path().join(".cargo").join("bin").join("hashi");
        write(&exe, "cargo build");
        t.exe = Some(exe.clone());
        let p = plan(&t, true).unwrap_or_else(|e| unreachable!("{e:#}"));
        assert_eq!(p.binary.as_ref().map(|b| b.kind), Some(BinaryKind::Cargo));
        assert!(p.is_empty());
        assert!(describe(&p).contains("cargo uninstall hashi"));
        let r = remove_files(&p, None);
        assert!(r.done.is_empty());
        assert!(exe.is_file());
    }

    #[test]
    fn missing_home_is_not_an_error() {
        let root = tmp();
        let mut t = fake_install(root.path());
        t.home = root.path().join("nowhere");
        let p = plan(&t, false).unwrap_or_else(|e| unreachable!("{e:#}"));
        assert!(p.home.is_none());
    }

    #[test]
    fn refuses_a_home_without_a_database() {
        let root = tmp();
        let home = root.path().join("stuff");
        write(&home.join("notes.txt"), "mine");
        let e = err_text(check_home(&home, None, None));
        assert!(e.contains("does not look like a Hashi home"), "{e}");
        assert!(e.contains("notes.txt"), "{e}");
        assert!(home.join("notes.txt").is_file());
    }

    #[test]
    fn the_default_home_needs_no_database() {
        let root = tmp();
        let home = root.path().join(".hashi");
        write(&home.join("logs").join("hashi.log"), "x");
        assert!(check_home(&home, Some(&home), None).is_ok_and(|exists| exists));
    }

    #[test]
    fn refuses_a_home_with_unrelated_files() {
        let root = tmp();
        let home = root.path().join("projects");
        write(&home.join("hashi.db"), "db");
        write(&home.join("thesis.docx"), "mine");
        let e = err_text(check_home(&home, None, None));
        assert!(e.contains("thesis.docx"), "{e}");
        assert!(!e.contains("hashi.db"), "{e}");
    }

    #[test]
    fn refuses_root_and_the_user_home() {
        let e = err_text(check_home(Path::new("/"), Some(Path::new("/")), None));
        assert!(e.contains("root"), "{e}");

        let root = tmp();
        let user = root.path().join("user");
        write(&user.join("hashi.db"), "db");
        let e = err_text(check_home(&user, Some(&user), Some(&user)));
        assert!(e.contains("home directory"), "{e}");
        // An ancestor of the user's home is refused too.
        let e = err_text(check_home(root.path(), Some(root.path()), Some(&user)));
        assert!(e.contains("home directory"), "{e}");
        assert!(user.join("hashi.db").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_symlinked_home() {
        let root = tmp();
        let real = root.path().join("real");
        write(&real.join("hashi.db"), "db");
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap_or_else(|e| unreachable!("{e}"));
        let e = err_text(check_home(&link, Some(&link), None));
        assert!(e.contains("symlink"), "{e}");
        assert!(real.join("hashi.db").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_inside_the_home_are_not_followed() {
        let root = tmp();
        let outside = root.path().join("outside");
        write(&outside.join("keep.txt"), "precious");
        let mut t = fake_install(root.path());
        t.exe = None;
        let ws = t.home.join("workspaces").join("tok-1").join("claude");
        std::os::unix::fs::symlink(&outside, ws.join("escape"))
            .unwrap_or_else(|e| unreachable!("{e}"));
        let p = plan(&t, false).unwrap_or_else(|e| unreachable!("{e:#}"));
        let r = remove_files(&p, None);
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        assert!(!t.home.exists());
        assert!(outside.join("keep.txt").is_file());
    }

    #[test]
    fn recognises_hashi_entries() {
        for n in [
            "hashi.db",
            "hashi.db-wal",
            "logs",
            "workspaces",
            "accounts",
            "remotes.json",
            ".remotes.json.123.tmp",
            "update-check.json",
            "update-0123abcd",
            "public-url",
            "server.pid",
        ] {
            assert!(is_hashi_entry(n), "{n}");
        }
        for n in [
            "Documents",
            "update-",
            "update-notes",
            ".ssh",
            "hashi.db.bak",
        ] {
            assert!(!is_hashi_entry(n), "{n}");
        }
    }

    #[test]
    fn a_live_hashi_pid_blocks_and_a_stale_one_does_not() {
        let root = tmp();
        let home = root.path();
        let own = 1;
        assert_eq!(running_server(home, own, &|_| true), None);

        write(&home.join(crate::server::PID_FILE), "4242\n");
        assert_eq!(running_server(home, own, &|pid| pid == 4242), Some(4242));
        // Stale: the pid is gone or belongs to something else.
        assert_eq!(running_server(home, own, &|_| false), None);
        // This very process is never counted.
        assert_eq!(running_server(home, 4242, &|_| true), None);
        write(&home.join(crate::server::PID_FILE), "garbage");
        assert_eq!(running_server(home, own, &|_| true), None);
    }

    #[test]
    fn checks_real_processes() {
        // The unit-test binary is named `hashi-<hash>`.
        assert!(is_hashi_process(std::process::id()));
        // A process that has exited is not alive.
        let mut child = std::process::Command::new(if cfg!(windows) { "cmd" } else { "true" })
            .args(if cfg!(windows) {
                &["/C", "exit"][..]
            } else {
                &[][..]
            })
            .spawn()
            .unwrap_or_else(|e| unreachable!("spawn: {e}"));
        let pid = child.id();
        let _ = child.wait();
        assert!(!is_hashi_process(pid));
        assert!(hashi_name("hashi.exe"));
        assert!(hashi_name("Hashi"));
        assert!(!hashi_name("hashicorp-vault"));
        assert!(!hashi_name("bash"));
    }

    #[test]
    fn removes_the_path_entry_only_on_an_exact_match() {
        let dir = r"C:\Users\me\AppData\Local\Programs\hashi\bin";
        let path = format!(r"C:\Windows;{dir};C:\Tools");
        assert_eq!(
            without_path_entry(&path, dir).as_deref(),
            Some(r"C:\Windows;C:\Tools")
        );
        // Case differs, as PowerShell's -contains ignores it.
        let upper = path.to_uppercase();
        assert_eq!(
            without_path_entry(&upper, dir).as_deref(),
            Some(r"C:\WINDOWS;C:\TOOLS")
        );
        // Only the entry itself, not a parent, child or near miss.
        for other in [
            r"C:\Users\me\AppData\Local\Programs\hashi",
            r"C:\Users\me\AppData\Local\Programs\hashi\bin\x",
            r"C:\Users\me\AppData\Local\Programs\hashi\bin2",
        ] {
            assert_eq!(
                without_path_entry(&format!("C:\\Windows;{other}"), dir),
                None
            );
        }
        assert_eq!(without_path_entry(dir, dir).as_deref(), Some(""));
        assert_eq!(
            without_path_entry(&format!("{dir};;C:\\A;"), dir).as_deref(),
            Some(r"C:\A")
        );
        assert_eq!(without_path_entry("", dir), None);
    }

    #[test]
    fn plans_the_path_entry_only_for_the_installer_dir() {
        let root = tmp();
        let mut t = fake_install(root.path());
        let bin = root.path().join("bin");
        let others = r"C:\Windows";
        t.user_path = Some((format!("{others};{}", bin.display()), bin.clone()));
        let p = plan(&t, true).unwrap_or_else(|e| unreachable!("{e:#}"));
        assert_eq!(
            p.path_entry,
            Some((bin.display().to_string(), others.to_owned()))
        );
        // A binary elsewhere leaves the PATH alone.
        t.user_path = Some((others.to_owned(), root.path().join("elsewhere")));
        assert!(plan(&t, true).is_ok_and(|p| p.path_entry.is_none()));
        assert_eq!(
            windows_install_dir(Path::new("L")),
            Path::new("L").join("Programs").join("hashi").join("bin")
        );
    }

    #[test]
    fn helper_removal_hints() {
        assert_eq!(
            removal_hint(
                "cloudflared",
                Path::new("/opt/homebrew/bin/cloudflared"),
                "macos"
            ),
            "brew uninstall cloudflared"
        );
        assert_eq!(
            removal_hint("cloudflared", Path::new(r"C:\x\cloudflared.exe"), "windows"),
            "winget uninstall --id Cloudflare.cloudflared"
        );
        assert!(
            removal_hint("tailscale", Path::new("/usr/bin/tailscale"), "linux")
                .contains("apt remove tailscale")
        );
    }
}
