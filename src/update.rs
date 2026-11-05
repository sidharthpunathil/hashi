//! Update checks and self-update from GitHub releases.
//!
//! The check asks GitHub for the latest release at most once a day and caches
//! the answer (or the failure) in `<home>/update-check.json`. The request
//! carries only a `hashi/<version>` User-Agent. `hashi update` downloads the
//! release archive for this platform, verifies its SHA-256 checksum, extracts
//! it with the system `tar` and replaces the running binary.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::events::{Bus, Event};

/// GitHub API endpoint for the latest release.
pub const LATEST_URL: &str = "https://api.github.com/repos/sidharthpunathil/hashi/releases/latest";
/// Base for release downloads: `<base>/<tag>/<asset>`.
pub const DOWNLOAD_BASE: &str = "https://github.com/sidharthpunathil/hashi/releases/download";
/// Cache file name inside the Hashi home.
pub const CACHE_FILE: &str = "update-check.json";
/// How long a cached answer (or failure) is trusted.
pub const CACHE_TTL_MS: i64 = 24 * 60 * 60 * 1000;
/// Environment variable that turns the automatic check off.
pub const OPT_OUT_ENV: &str = "HASHI_NO_UPDATE_CHECK";
/// Name the running Windows binary is moved to while it is replaced.
pub const OLD_EXE: &str = "hashi.old.exe";
/// How to install a Cargo-managed copy.
pub const CARGO_INSTALL: &str =
    "cargo install --locked --git https://github.com/sidharthpunathil/hashi --force";

const CHECK_TIMEOUT: Duration = Duration::from_secs(5);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_DOWNLOAD: u64 = 256 * 1024 * 1024;
const VERSION_TIMEOUT: Duration = Duration::from_secs(15);

// ── versions ───────────────────────────────────────────────────────────────

/// A `major.minor.patch` version. Pre-release tags are ignored except that a
/// pre-release sorts before the release with the same numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    /// Major number.
    pub major: u64,
    /// Minor number.
    pub minor: u64,
    /// Patch number.
    pub patch: u64,
    /// `false` for a pre-release such as `1.2.0-rc.1`.
    pub release: bool,
}

impl Version {
    /// Parses `1.2.3`, `v1.2.3`, `1.2.3-rc.1` or `1.2.3+build`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let s = s.strip_prefix('v').unwrap_or(s);
        let (s, _build) = s.split_once('+').unwrap_or((s, ""));
        let (core, pre) = match s.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (s, None),
        };
        if pre.is_some_and(str::is_empty) {
            return None;
        }
        let mut parts = core.split('.');
        let mut num = || -> Option<u64> {
            let p = parts.next()?;
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            p.parse().ok()
        };
        let (major, minor, patch) = (num()?, num()?, num()?);
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            major,
            minor,
            patch,
            release: pre.is_none(),
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.release {
            f.write_str("-pre")?;
        }
        Ok(())
    }
}

/// The version of this build.
#[must_use]
pub fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Version {
        major: 0,
        minor: 0,
        patch: 0,
        release: false,
    })
}

/// `true` when `latest` is newer than `current`.
#[must_use]
pub fn is_newer(latest: &Version, current: &Version) -> bool {
    latest > current
}

/// `true` when `HASHI_NO_UPDATE_CHECK` is set to anything but an empty or
/// false-like value (`0`, `false`, `no`, `off`).
#[must_use]
pub fn opted_out() -> bool {
    std::env::var(OPT_OUT_ENV).is_ok_and(|v| {
        let v = v.trim().to_ascii_lowercase();
        !matches!(v.as_str(), "" | "0" | "false" | "no" | "off" | "n" | "f")
    })
}

// ── latest release ─────────────────────────────────────────────────────────

/// A published release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Git tag, such as `v0.3.0`.
    pub tag: String,
    /// Parsed version.
    pub version: Version,
    /// Release page.
    pub url: String,
}

#[derive(Debug, Deserialize)]
struct LatestJson {
    tag_name: String,
    html_url: String,
}

fn user_agent() -> String {
    format!("hashi/{}", env!("CARGO_PKG_VERSION"))
}

fn agent(timeout: Duration) -> ureq::Agent {
    agent_with(timeout, false)
}

/// An agent that, with `https_only`, refuses any `http://` URL, also one a
/// redirect points at.
fn agent_with(timeout: Duration, https_only: bool) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .user_agent(user_agent())
        .https_only(https_only)
        .build()
        .into()
}

/// A tag is only used in URLs and file names, so keep it to plain characters.
fn safe_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 64
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+' | b'_'))
}

fn release_from(tag: String, url: String) -> Result<Release> {
    if !safe_tag(&tag) {
        bail!("the latest release has an unexpected tag");
    }
    let version =
        Version::parse(&tag).ok_or_else(|| anyhow!("cannot read a version from tag {tag}"))?;
    let url = if url.starts_with("https://") && !url.contains(char::is_control) {
        url
    } else {
        String::new()
    };
    Ok(Release { tag, version, url })
}

/// Asks `api_url` (normally [`LATEST_URL`]) for the latest release.
///
/// # Errors
///
/// Fails on a network error, a non-success status or an unexpected body.
pub fn fetch_latest(api_url: &str) -> Result<Release> {
    let mut resp = agent(CHECK_TIMEOUT)
        .get(api_url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .context("asking GitHub for the latest release")?;
    let body: LatestJson = resp
        .body_mut()
        .with_config()
        .limit(1024 * 1024)
        .read_json()
        .context("reading the latest release")?;
    release_from(body.tag_name, body.html_url)
}

// ── cache ──────────────────────────────────────────────────────────────────

/// What `update-check.json` holds.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Cache {
    /// When the check ran, in Unix milliseconds.
    pub checked_at_ms: i64,
    /// Latest tag, when the check succeeded.
    #[serde(default)]
    pub tag: Option<String>,
    /// Release page, when the check succeeded.
    #[serde(default)]
    pub url: Option<String>,
    /// Why the check failed.
    #[serde(default)]
    pub error: Option<String>,
}

impl Cache {
    /// `true` while the entry is younger than [`CACHE_TTL_MS`]. An entry from
    /// the future (a clock that moved back) is stale.
    #[must_use]
    pub fn is_fresh(&self, now_ms: i64) -> bool {
        let age = now_ms - self.checked_at_ms;
        (0..CACHE_TTL_MS).contains(&age)
    }

    fn result(&self) -> Result<Release> {
        match (&self.tag, &self.error) {
            (Some(tag), None) => release_from(tag.clone(), self.url.clone().unwrap_or_default()),
            (_, Some(e)) => Err(anyhow!("{e}")),
            (None, None) => Err(anyhow!("no cached answer")),
        }
    }
}

/// Reads the cache, if there is a readable one.
#[must_use]
pub fn read_cache(home: &Path) -> Option<Cache> {
    let text = std::fs::read_to_string(home.join(CACHE_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_cache(home: &Path, cache: &Cache) {
    if !home.is_dir() {
        return;
    }
    if let Ok(text) = serde_json::to_string(cache) {
        let _ = crate::diagnostics::write_private(&home.join(CACHE_FILE), &text);
    }
}

/// Returns the latest release, from the cache when it is fresh and `force`
/// is not set, otherwise from `api_url`. Successes and failures are both
/// cached, so an offline machine does not retry on every start. The cache is
/// only written when `home` already exists.
///
/// # Errors
///
/// Returns the (possibly cached) reason the check failed.
pub fn check(home: &Path, api_url: &str, now_ms: i64, force: bool) -> Result<Release> {
    if !force
        && let Some(cache) = read_cache(home)
        && cache.is_fresh(now_ms)
    {
        return cache.result();
    }
    let fetched = fetch_latest(api_url);
    let cache = match &fetched {
        Ok(r) => Cache {
            checked_at_ms: now_ms,
            tag: Some(r.tag.clone()),
            url: Some(r.url.clone()),
            error: None,
        },
        Err(e) => Cache {
            checked_at_ms: now_ms,
            tag: None,
            url: None,
            error: Some(format!("{e:#}")),
        },
    };
    write_cache(home, &cache);
    fetched
}

/// The latest release when it is newer than this build.
#[must_use]
pub fn available(home: &Path, api_url: &str, now_ms: i64) -> Option<Release> {
    check(home, api_url, now_ms, false)
        .ok()
        .filter(|r| is_newer(&r.version, &current_version()))
}

/// Runs the cached check on its own thread and emits
/// [`Event::UpdateAvailable`] when a newer release exists. Never blocks the
/// caller and stays silent on failure. The thread is detached, so a slow
/// network never holds up shutdown.
pub fn spawn_check(home: PathBuf, api_url: String, bus: Bus) {
    let spawned = std::thread::Builder::new()
        .name("hashi-update-check".to_owned())
        .spawn(move || {
            if let Some(r) = available(&home, &api_url, crate::util::now_ms()) {
                bus.emit(Event::UpdateAvailable {
                    version: r.version.to_string(),
                    url: r.url,
                });
            }
        });
    if let Err(e) = spawned {
        tracing::debug!(error = %e, "could not start the update check");
    }
}

// ── platforms ──────────────────────────────────────────────────────────────

/// Maps an (os, arch, env) triple to the release target it has a build for.
#[must_use]
pub fn target_for(os: &str, arch: &str, env: &str) -> Option<&'static str> {
    match (os, arch, env) {
        ("linux", "x86_64", "gnu") => Some("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64", "gnu") => Some("aarch64-unknown-linux-gnu"),
        ("macos", "x86_64", _) => Some("x86_64-apple-darwin"),
        ("macos", "aarch64", _) => Some("aarch64-apple-darwin"),
        ("windows", "x86_64", "msvc") => Some("x86_64-pc-windows-msvc"),
        _ => None,
    }
}

const TARGET_ENV: &str = if cfg!(target_env = "gnu") {
    "gnu"
} else if cfg!(target_env = "msvc") {
    "msvc"
} else if cfg!(target_env = "musl") {
    "musl"
} else {
    ""
};

/// The release target for this build.
///
/// # Errors
///
/// Fails with "no prebuilt binary for this platform" elsewhere.
pub fn current_target() -> Result<&'static str> {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    target_for(os, arch, TARGET_ENV).ok_or_else(|| {
        anyhow!(
            "no prebuilt binary for this platform ({os} {arch} {TARGET_ENV}); \
             build from source with: {CARGO_INSTALL}"
        )
    })
}

/// Archive name for a target: `.zip` on Windows, `.tar.gz` elsewhere.
#[must_use]
pub fn asset_name(target: &str) -> String {
    if target.contains("windows") {
        format!("hashi-{target}.zip")
    } else {
        format!("hashi-{target}.tar.gz")
    }
}

fn binary_name(target: &str) -> &'static str {
    if target.contains("windows") {
        "hashi.exe"
    } else {
        "hashi"
    }
}

// ── safety checks ──────────────────────────────────────────────────────────

/// Refuses a binary that Cargo installed (under `.cargo/bin` or
/// `$CARGO_HOME/bin`): Cargo should keep managing it.
///
/// # Errors
///
/// Fails with the `cargo install` command to use instead.
pub fn refuse_cargo_install(exe: &Path) -> Result<()> {
    let Some(dir) = exe.parent() else {
        return Ok(());
    };
    let dotcargo = dir.file_name().is_some_and(|n| n == "bin")
        && dir
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|n| n == ".cargo");
    let cargo_home = std::env::var_os("CARGO_HOME")
        .is_some_and(|h| !h.is_empty() && Path::new(&h).join("bin") == dir);
    if dotcargo || cargo_home {
        bail!(
            "{} was installed by Cargo, so update it with Cargo:\n  {CARGO_INSTALL}",
            exe.display()
        );
    }
    Ok(())
}

/// Checks that new files can be created in `dir`.
///
/// # Errors
///
/// Fails with guidance when the directory is not writable.
pub fn check_writable(dir: &Path) -> Result<()> {
    let probe = dir.join(format!(".hashi-write-test-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(e) => bail!(
            "cannot write to {} ({e}); reinstall with the installer, or run hashi update \
             as a user who can write there",
            dir.display()
        ),
    }
}

/// Checks `bytes` against a `.sha256` file (`<hex>  <name>`, as written by
/// `shasum -a 256` or the Windows release job).
///
/// # Errors
///
/// Fails when the file is malformed or the digest does not match.
pub fn verify_sha256(bytes: &[u8], sha_file: &str) -> Result<()> {
    let expected = sha_file
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("the checksum file is malformed; refusing to install");
    }
    let actual = hex::encode(Sha256::digest(bytes));
    if actual != expected {
        bail!("checksum mismatch (expected {expected}, got {actual}); refusing to install");
    }
    Ok(())
}

// ── download, extract, verify ──────────────────────────────────────────────

fn download(url: &str, https_only: bool) -> Result<Vec<u8>> {
    let mut resp = agent_with(DOWNLOAD_TIMEOUT, https_only)
        .get(url)
        .call()
        .with_context(|| format!("downloading {url}"))?;
    resp.body_mut()
        .with_config()
        .limit(MAX_DOWNLOAD)
        .read_to_vec()
        .with_context(|| format!("downloading {url}"))
}

fn tar_program() -> PathBuf {
    #[cfg(windows)]
    {
        // Windows 10 and later ship bsdtar here; never pick one up from PATH.
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        Path::new(&root).join("System32").join("tar.exe")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("tar")
    }
}

/// Extracts `archive` into `dest` with the system `tar` and returns a
/// private copy of the Hashi binary in it, ready to run. The archive's entry
/// must be a regular file in a real directory: a symlink or hard link is
/// refused, so nothing outside `dest` is ever made executable or run.
///
/// # Errors
///
/// Fails when `tar` fails, or the binary is missing or not a regular file.
pub fn extract(archive: &Path, dest: &Path, target: &str) -> Result<PathBuf> {
    let flags = if archive.extension().is_some_and(|e| e == "zip") {
        "-xf"
    } else {
        "-xzf"
    };
    let out = Command::new(tar_program())
        .arg(flags)
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .stdin(Stdio::null())
        .output()
        .context("running tar")?;
    if !out.status.success() {
        bail!(
            "tar could not extract the archive: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    // Unix archives hold `hashi-<target>/hashi`; the Windows zip holds the
    // files at its root.
    let bin = binary_name(target);
    let entry = [
        dest.join(format!("hashi-{target}")).join(bin),
        dest.join(bin),
    ]
    .into_iter()
    .find(|p| std::fs::symlink_metadata(p).is_ok())
    .ok_or_else(|| anyhow!("the archive has no {bin}"))?;
    let meta = std::fs::symlink_metadata(&entry)?;
    let in_real_dir = entry
        .parent()
        .and_then(|d| std::fs::symlink_metadata(d).ok())
        .is_some_and(|m| m.file_type().is_dir());
    if !meta.file_type().is_file() || !in_real_dir {
        bail!(
            "the archive's {bin} is not a regular file (a link or a directory); refusing to install"
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() > 1 {
            bail!("the archive's {bin} is a hard link; refusing to install");
        }
    }
    stage(&entry, dest, bin)
}

/// Copies `entry` to a new file in `dest` and makes that copy executable.
/// `create_new` refuses a name that already exists, a link included.
fn stage(entry: &Path, dest: &Path, bin: &str) -> Result<PathBuf> {
    let staged = dest.join(format!(".staged-{bin}"));
    let mut from = std::fs::File::open(entry).context("opening the extracted binary")?;
    let mut to = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged)
        .with_context(|| format!("creating {}", staged.display()))?;
    std::io::copy(&mut from, &mut to).context("copying the extracted binary")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        to.set_permissions(std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(staged)
}

/// Runs `bin --version` and checks that it reports `expected` (a tag such
/// as `v0.3.0`).
///
/// # Errors
///
/// Fails when the binary is not a regular file, does not run, hangs, or
/// reports another version.
pub fn verify_binary(bin: &Path, expected: &str) -> Result<()> {
    if !std::fs::symlink_metadata(bin)?.file_type().is_file() {
        bail!(
            "{} is not a regular file; refusing to run it",
            bin.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(bin, std::fs::Permissions::from_mode(0o755))?;
    }
    let mut child = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("running the downloaded binary")?;
    let started = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if started.elapsed() > VERSION_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            bail!("the downloaded binary did not answer --version");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let want = expected.strip_prefix('v').unwrap_or(expected);
    let got = text.split_whitespace().last().unwrap_or_default();
    if !out.status.success() || got != want {
        bail!(
            "the downloaded binary reports {:?}, expected {want}; refusing to install",
            crate::util::truncate(text.trim(), 80)
        );
    }
    Ok(())
}

// ── replace ────────────────────────────────────────────────────────────────

/// Replaces `target` with `new`. On Unix the new binary is copied next to
/// the target, made executable and renamed over it. On Windows the target is
/// moved aside to [`OLD_EXE`] first, because a running exe cannot be
/// overwritten.
///
/// # Errors
///
/// Fails when a copy or rename fails; the original is left in place.
pub fn replace_exe(new: &Path, target: &Path) -> Result<()> {
    if cfg!(windows) {
        replace_aside(new, target)
    } else {
        replace_by_rename(new, target)
    }
}

fn staging_path(target: &Path) -> Result<PathBuf> {
    let dir = target
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", target.display()))?;
    Ok(dir.join(format!(".hashi-update-{}", std::process::id())))
}

/// The Unix replace: copy next to the target, set 0755, rename over it.
///
/// # Errors
///
/// As for [`replace_exe`].
pub fn replace_by_rename(new: &Path, target: &Path) -> Result<()> {
    let staged = staging_path(target)?;
    let result = (|| -> Result<()> {
        std::fs::copy(new, &staged)
            .with_context(|| format!("copying the new binary to {}", staged.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
        }
        std::fs::rename(&staged, target).with_context(|| format!("replacing {}", target.display()))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result
}

/// The Windows replace: move the running exe to [`OLD_EXE`] and move the
/// new one into its place, restoring the original if that fails.
///
/// # Errors
///
/// As for [`replace_exe`].
pub fn replace_aside(new: &Path, target: &Path) -> Result<()> {
    let staged = staging_path(target)?;
    let dir = staged.parent().unwrap_or(Path::new("."));
    let old = dir.join(OLD_EXE);
    std::fs::copy(new, &staged)
        .with_context(|| format!("copying the new binary to {}", staged.display()))?;
    if old.exists() {
        let _ = std::fs::remove_file(&old);
    }
    if let Err(e) = std::fs::rename(target, &old) {
        let _ = std::fs::remove_file(&staged);
        return Err(e).with_context(|| format!("moving {} aside", target.display()));
    }
    if let Err(e) = std::fs::rename(&staged, target) {
        let _ = std::fs::rename(&old, target);
        let _ = std::fs::remove_file(&staged);
        return Err(e).with_context(|| format!("replacing {}", target.display()));
    }
    Ok(())
}

/// Deletes a [`OLD_EXE`] left next to `exe` by an earlier Windows update.
pub fn cleanup_old_exe(exe: &Path) {
    if let Some(dir) = exe.parent() {
        let old = dir.join(OLD_EXE);
        if old.is_file() {
            let _ = std::fs::remove_file(old);
        }
    }
}

// ── hashi update ───────────────────────────────────────────────────────────

/// Where releases are looked up and downloaded from. Tests point these at a
/// local server.
#[derive(Debug, Clone)]
pub struct Sources {
    /// Latest-release endpoint.
    pub api: String,
    /// Download base: `<base>/<tag>/<asset>`.
    pub downloads: String,
    /// Refuse plain `http://` downloads, redirects included. Only tests
    /// against a local server turn this off.
    pub https_only: bool,
}

impl Default for Sources {
    fn default() -> Self {
        Self {
            api: LATEST_URL.to_owned(),
            downloads: DOWNLOAD_BASE.to_owned(),
            https_only: true,
        }
    }
}

/// What an update is about to do, shown before asking to confirm.
#[derive(Debug, Clone)]
pub struct Plan {
    /// Tag to install.
    pub tag: String,
    /// Archive to download.
    pub asset: String,
    /// Binary to replace.
    pub exe: PathBuf,
    /// The tag is older than this build (allowed by `--allow-downgrade`).
    pub downgrade: bool,
}

/// Whether `hashi update` should ask before installing: not with `--yes`,
/// and never silently without a terminal.
///
/// # Errors
///
/// Fails without a terminal and without `--yes`.
pub fn needs_prompt(yes: bool, tty: bool) -> Result<bool> {
    if yes {
        Ok(false)
    } else if tty {
        Ok(true)
    } else {
        bail!("not updating without a terminal to confirm on; pass --yes")
    }
}

/// The warning shown before installing `plan` when it is a downgrade.
#[must_use]
pub fn downgrade_warning(plan: &Plan) -> Option<String> {
    plan.downgrade.then(|| {
        format!(
            "warning: {} is older than this hashi (v{}). This build keeps its database at \
             schema version {}, which an older release may not support: it may refuse to \
             start, or misread the database. Back up the Hashi home first.",
            plan.tag,
            env!("CARGO_PKG_VERSION"),
            crate::store::SCHEMA_VERSION
        )
    })
}

/// How `hashi update` ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Already on the latest release.
    UpToDate(String),
    /// The user said no.
    Declined,
    /// Installed this tag.
    Updated(String),
}

/// Normalises a `--version` value to a tag such as `v0.3.0`.
///
/// # Errors
///
/// Fails when it is not a version.
pub fn tag_for(version: &str) -> Result<String> {
    let v = version.trim();
    if Version::parse(v).is_none() || !safe_tag(v) {
        bail!("{v:?} is not a version like v0.3.0");
    }
    Ok(if v.starts_with('v') {
        v.to_owned()
    } else {
        format!("v{v}")
    })
}

/// Removes its directory when dropped.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Updates `exe` to `version` (a tag), or to the latest release. A version
/// older than this build needs `allow_downgrade`. `confirm` is asked once
/// everything is known and before anything is downloaded.
///
/// # Errors
///
/// Fails when the binary was installed by Cargo, its directory is not
/// writable, the platform has no build, `version` is older than this build
/// without `allow_downgrade`, or a download, checksum, extraction or replace
/// step fails. The binary is untouched on failure.
pub fn install(
    home: &Path,
    sources: &Sources,
    exe: &Path,
    version: Option<&str>,
    allow_downgrade: bool,
    confirm: &mut dyn FnMut(&Plan) -> bool,
) -> Result<Outcome> {
    let target = current_target()?;
    refuse_cargo_install(exe)?;
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", exe.display()))?;
    check_writable(dir)?;

    let tag = match version {
        Some(v) => tag_for(v)?,
        None => {
            let latest = check(home, &sources.api, crate::util::now_ms(), true)?;
            if !is_newer(&latest.version, &current_version()) {
                return Ok(Outcome::UpToDate(latest.tag));
            }
            latest.tag
        }
    };
    let current = current_version();
    let downgrade = Version::parse(&tag).is_some_and(|v| v < current);
    if downgrade && !allow_downgrade {
        bail!(
            "{tag} is older than this hashi (v{}); pass --allow-downgrade to install it anyway",
            env!("CARGO_PKG_VERSION")
        );
    }
    let asset = asset_name(target);
    let plan = Plan {
        tag: tag.clone(),
        asset: asset.clone(),
        exe: exe.to_path_buf(),
        downgrade,
    };
    if !confirm(&plan) {
        return Ok(Outcome::Declined);
    }

    let base = sources.downloads.trim_end_matches('/');
    let url = format!("{base}/{tag}/{asset}");
    let archive = download(&url, sources.https_only)?;
    let sums = download(&format!("{url}.sha256"), sources.https_only)
        .context("could not download the checksum; refusing to install")?;
    verify_sha256(&archive, &String::from_utf8_lossy(&sums))?;

    std::fs::create_dir_all(home)?;
    let tmp = TempDir(home.join(format!("update-{}", uuid::Uuid::new_v4().simple())));
    std::fs::create_dir(&tmp.0).with_context(|| format!("creating {}", tmp.0.display()))?;
    let archive_path = tmp.0.join(&asset);
    std::fs::write(&archive_path, &archive)?;
    let extracted = tmp.0.join("x");
    std::fs::create_dir(&extracted)?;
    let bin = extract(&archive_path, &extracted, target)?;
    verify_binary(&bin, &tag)?;
    replace_exe(&bin, exe)?;
    Ok(Outcome::Updated(tag))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| unreachable!("bad test version {s}"))
    }

    #[test]
    fn parses_versions() {
        assert_eq!(
            v("v1.2.3"),
            Version {
                major: 1,
                minor: 2,
                patch: 3,
                release: true
            }
        );
        assert!(!v("1.2.3-rc.1").release);
        assert!(v("1.2.3+build.5").release);
        for bad in [
            "", "1.2", "1.2.3.4", "a.b.c", "1.2.3-", "v", "1..3", "-1.2.3",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn compares_versions() {
        assert!(is_newer(&v("0.2.1"), &v("0.2.0")));
        assert!(is_newer(&v("0.10.0"), &v("0.9.9")));
        assert!(is_newer(&v("1.0.0"), &v("0.99.99")));
        assert!(!is_newer(&v("0.2.0"), &v("0.2.0")));
        assert!(!is_newer(&v("0.1.9"), &v("0.2.0")));
        // A pre-release build is older than the release it leads to.
        assert!(is_newer(&v("0.3.0"), &v("0.3.0-rc.1")));
        assert!(!is_newer(&v("0.3.0-rc.1"), &v("0.3.0")));
        // Pre-release tags themselves are ignored.
        assert!(!is_newer(&v("0.3.0-rc.2"), &v("0.3.0-rc.1")));
    }

    #[test]
    fn cache_freshness() {
        let c = Cache {
            checked_at_ms: 1_000_000,
            ..Cache::default()
        };
        assert!(c.is_fresh(1_000_000));
        assert!(c.is_fresh(1_000_000 + CACHE_TTL_MS - 1));
        assert!(!c.is_fresh(1_000_000 + CACHE_TTL_MS));
        assert!(!c.is_fresh(999_999));
    }

    #[test]
    fn maps_targets() {
        assert_eq!(
            target_for("linux", "x86_64", "gnu"),
            Some("x86_64-unknown-linux-gnu")
        );
        assert_eq!(
            target_for("linux", "aarch64", "gnu"),
            Some("aarch64-unknown-linux-gnu")
        );
        assert_eq!(
            target_for("macos", "x86_64", ""),
            Some("x86_64-apple-darwin")
        );
        assert_eq!(
            target_for("macos", "aarch64", ""),
            Some("aarch64-apple-darwin")
        );
        assert_eq!(
            target_for("windows", "x86_64", "msvc"),
            Some("x86_64-pc-windows-msvc")
        );
        assert_eq!(target_for("linux", "x86_64", "musl"), None);
        assert_eq!(target_for("windows", "aarch64", "msvc"), None);
        assert_eq!(target_for("freebsd", "x86_64", ""), None);
        assert_eq!(
            asset_name("x86_64-pc-windows-msvc"),
            "hashi-x86_64-pc-windows-msvc.zip"
        );
        assert_eq!(
            asset_name("aarch64-apple-darwin"),
            "hashi-aarch64-apple-darwin.tar.gz"
        );
    }

    #[test]
    fn refuses_cargo_installs() {
        let exe = Path::new("/home/me/.cargo/bin/hashi");
        let err = refuse_cargo_install(exe).err().map(|e| e.to_string());
        assert!(
            err.as_deref().is_some_and(|e| e.contains("cargo install")),
            "{err:?}"
        );
        assert!(refuse_cargo_install(Path::new("/home/me/.local/bin/hashi")).is_ok());
        assert!(refuse_cargo_install(Path::new("/home/me/.cargo/hashi")).is_ok());
    }

    #[test]
    fn checksums() {
        let data = b"hello";
        let good = format!("{}  hashi.tar.gz\n", hex::encode(Sha256::digest(data)));
        assert!(verify_sha256(data, &good).is_ok());
        assert!(verify_sha256(data, &good.to_uppercase()).is_ok());
        let bad = format!("{}  hashi.tar.gz", "0".repeat(64));
        assert!(
            verify_sha256(data, &bad)
                .err()
                .is_some_and(|e| e.to_string().contains("mismatch"))
        );
        assert!(verify_sha256(data, "not a hash").is_err());
        assert!(verify_sha256(data, "").is_err());
    }

    #[test]
    fn without_a_terminal_only_yes_updates() {
        assert!(needs_prompt(false, false).is_err());
        assert!(!needs_prompt(true, false).unwrap());
        assert!(needs_prompt(false, true).unwrap());
        assert!(!needs_prompt(true, true).unwrap());
    }

    #[test]
    fn the_download_agent_refuses_plain_http() {
        let e = download("http://127.0.0.1:9/hashi.tar.gz", true).unwrap_err();
        assert!(format!("{e:#}").to_lowercase().contains("https"), "{e:#}");
    }

    #[test]
    fn a_downgrade_warns_about_the_schema() {
        let plan = Plan {
            tag: "v0.0.1".into(),
            asset: "a".into(),
            exe: PathBuf::from("hashi"),
            downgrade: true,
        };
        let w = downgrade_warning(&plan).unwrap();
        assert!(
            w.contains(&format!("schema version {}", crate::store::SCHEMA_VERSION)),
            "{w}"
        );
        assert!(
            downgrade_warning(&Plan {
                downgrade: false,
                ..plan
            })
            .is_none()
        );
    }

    #[test]
    fn normalises_tags() {
        assert_eq!(tag_for("0.3.0").ok().as_deref(), Some("v0.3.0"));
        assert_eq!(tag_for("v0.3.0").ok().as_deref(), Some("v0.3.0"));
        assert!(tag_for("latest").is_err());
        assert!(tag_for("v0.3.0/../x").is_err());
    }
}
