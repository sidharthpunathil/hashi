//! Resource guard: keeps the owner's machine usable while agents run.
//!
//! Three layers protect the machine, whoever submitted the work:
//!
//! 1. A global cap on concurrent runs across every agent. Workers take a
//!    slot *before* they claim a request, so held work stays `queued` and
//!    keeps its place in line.
//! 2. Admission by memory and disk. With a slot in hand, a worker checks
//!    that starting one more run still leaves the owner's reserve free and
//!    that the volume holding the Hashi home is above its floor. If not, it
//!    gives the slot back and tries again a few seconds later.
//! 3. A per-run watchdog (see [`MemoryWatch`]) that samples the run's whole
//!    process tree and stops it when it grows past its limit.
//!
//! All measurements go through the [`Probe`] trait, so tests can simulate a
//! busy machine without touching the real one.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::warn;

use crate::events::{Bus, Event};
use crate::util::human_bytes;

/// One mebibyte.
pub const MIB: u64 = 1024 * 1024;
/// One gibibyte.
pub const GIB: u64 = 1024 * MIB;

/// Memory a run is assumed to need when it starts, on top of the reserve.
pub const EXPECTED_RUN_MEMORY: u64 = 512 * MIB;
/// How often the watchdog samples a run's process tree.
pub const SAMPLE_EVERY: Duration = Duration::from_secs(2);
/// How long a held worker waits before checking again.
pub const RETRY_AFTER: Duration = Duration::from_secs(3);
/// Minimum gap between two "held" notices.
pub const HELD_NOTICE_EVERY: Duration = Duration::from_secs(60);
/// Default niceness for agent runs on Unix.
pub const DEFAULT_NICE: u8 = 10;
/// Default free space kept under the Hashi home.
pub const DEFAULT_DISK_FLOOR: Size = Size::Bytes(GIB);

// ── sizes ──────────────────────────────────────────────────────────────────

/// A size setting: an absolute number of bytes, or a share of a total.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Size {
    /// Exactly this many bytes.
    Bytes(u64),
    /// This percentage (0 to 100) of the relevant total.
    Percent(f64),
}

impl Size {
    /// Resolves the size against `total` (memory or disk, depending on use).
    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    pub fn resolve(self, total: u64) -> u64 {
        match self {
            Self::Bytes(b) => b,
            Self::Percent(p) => ((total as f64) * p / 100.0) as u64,
        }
    }

    /// Whether this size is zero (which disables the related check).
    #[must_use]
    pub fn is_zero(self) -> bool {
        match self {
            Self::Bytes(b) => b == 0,
            Self::Percent(p) => p == 0.0,
        }
    }
}

impl fmt::Display for Size {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bytes(b) => f.write_str(&human_bytes(*b)),
            Self::Percent(p) => write!(f, "{p}%"),
        }
    }
}

impl FromStr for Size {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_size(s)
    }
}

/// Parses `512M`, `4G`, `1.5GiB`, `25%` or `0`. Units are binary (`1K` is
/// 1024 bytes) and case does not matter.
///
/// # Errors
///
/// Rejects empty input, negative or malformed numbers, unknown units, a bare
/// number other than zero, and percentages outside 0 to 100.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn parse_size(s: &str) -> Result<Size, String> {
    let t = s.trim();
    if t.is_empty() {
        return Err("expected a size such as 512M, 4G or 25%".to_owned());
    }
    if let Some(p) = t.strip_suffix('%') {
        let v = parse_number(p.trim()).ok_or_else(|| format!("`{t}` is not a percentage"))?;
        if v > 100.0 {
            return Err("a percentage must be between 0 and 100".to_owned());
        }
        return Ok(Size::Percent(v));
    }
    let split = t
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let v = parse_number(num).ok_or_else(|| format!("`{t}` is not a size such as 512M or 4G"))?;
    let multiplier: u64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" if v == 0.0 => 1,
        "" => return Err(format!("`{t}` needs a unit, for example {t}M or {t}G")),
        "b" => 1,
        "k" | "kb" | "kib" => 1 << 10,
        "m" | "mb" | "mib" => 1 << 20,
        "g" | "gb" | "gib" => 1 << 30,
        "t" | "tb" | "tib" => 1 << 40,
        _ => {
            return Err(format!(
                "unknown unit `{}`; use K, M, G, T or %",
                unit.trim()
            ));
        }
    };
    #[allow(clippy::cast_precision_loss)]
    let bytes = v * multiplier as f64;
    #[allow(clippy::cast_precision_loss)]
    if !bytes.is_finite() || bytes >= u64::MAX as f64 {
        return Err(format!("`{t}` is too large"));
    }
    Ok(Size::Bytes(bytes as u64))
}

/// Like [`parse_size`], but zero is an error. Used where zero makes no sense,
/// such as the per-run memory limit.
///
/// # Errors
///
/// Everything [`parse_size`] rejects, plus zero.
pub fn parse_nonzero_size(s: &str) -> Result<Size, String> {
    let size = parse_size(s)?;
    if size.is_zero() {
        return Err("must be more than zero".to_owned());
    }
    Ok(size)
}

fn parse_number(s: &str) -> Option<f64> {
    // `f64::from_str` also takes `inf`, `nan`, `1e9` and a sign; sizes do not.
    if s.is_empty() || !s.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    s.parse::<f64>().ok().filter(|v| v.is_finite())
}

// ── probing ────────────────────────────────────────────────────────────────

/// Machine memory, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Memory {
    /// Installed (or cgroup-limited) memory.
    pub total: u64,
    /// Memory that can be given to new work without swapping.
    pub available: u64,
}

/// Space on one volume, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskSpace {
    /// Space available to this user.
    pub free: u64,
    /// Volume size.
    pub total: u64,
}

/// Source of resource measurements. The real one is [`SystemProbe`];
/// [`ManualProbe`] reports whatever a test tells it to.
pub trait Probe: Send + Sync + fmt::Debug {
    /// Current machine memory, or `None` if it cannot be read.
    fn memory(&self) -> Option<Memory>;
    /// Space on the volume that holds `path`.
    fn disk(&self, path: &Path) -> Option<DiskSpace>;
    /// Resident memory of process `pid` plus all of its descendants (and,
    /// on Unix, every member of its process group).
    fn tree_memory(&self, pid: u32) -> Option<u64>;
}

/// Measures the real machine through `sysinfo`.
#[derive(Debug)]
pub struct SystemProbe {
    sys: Mutex<sysinfo::System>,
    disks: Mutex<Option<(Instant, sysinfo::Disks)>>,
}

impl SystemProbe {
    /// Disk lists are refreshed at most this often.
    const DISK_TTL: Duration = Duration::from_secs(5);

    /// Creates a probe. Nothing is measured until asked.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sys: Mutex::new(sysinfo::System::new()),
            disks: Mutex::new(None),
        }
    }
}

impl Default for SystemProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl Probe for SystemProbe {
    fn memory(&self) -> Option<Memory> {
        let mut sys = self.sys.lock().ok()?;
        sys.refresh_memory();
        let mut total = sys.total_memory();
        let mut available = sys.available_memory();
        // Inside a container the cgroup limit is the real ceiling.
        if let Some(cg) = sys.cgroup_limits()
            && cg.total_memory > 0
            && cg.total_memory < total
        {
            total = cg.total_memory;
            available = available.min(cg.free_memory);
        }
        (total > 0).then_some(Memory {
            total,
            available: available.min(total),
        })
    }

    fn disk(&self, path: &Path) -> Option<DiskSpace> {
        // Resolve symlinks on Unix (macOS keeps temp dirs behind `/var`). On
        // Windows `canonicalize` returns `\\?\C:\...`, which never matches a
        // mount point such as `C:\`, so only make the path absolute there.
        #[cfg(unix)]
        let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        #[cfg(not(unix))]
        let target = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let mut cache = self.disks.lock().ok()?;
        let stale = cache
            .as_ref()
            .is_none_or(|(at, _)| at.elapsed() >= Self::DISK_TTL);
        if stale {
            *cache = Some((Instant::now(), sysinfo::Disks::new_with_refreshed_list()));
        }
        let (_, disks) = cache.as_ref()?;
        disks
            .list()
            .iter()
            .filter(|d| target.starts_with(d.mount_point()))
            .max_by_key(|d| d.mount_point().as_os_str().len())
            .map(|d| DiskSpace {
                free: d.available_space(),
                total: d.total_space(),
            })
    }

    fn tree_memory(&self, pid: u32) -> Option<u64> {
        use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate};

        let mut sys = self.sys.lock().ok()?;
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_memory(),
        );
        let procs = sys.processes();
        let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
        for (p, proc_) in procs {
            if let Some(parent) = proc_.parent() {
                children.entry(parent).or_default().push(*p);
            }
        }
        let root = Pid::from_u32(pid);
        let mut stack = vec![root];
        // Descendants that double-forked away from the tree are still in the
        // run's process group, so count those too.
        #[cfg(unix)]
        stack.extend(procs.keys().copied().filter(|p| same_group(*p, pid)));
        let mut seen = HashSet::new();
        while let Some(p) = stack.pop() {
            if seen.insert(p)
                && let Some(kids) = children.get(&p)
            {
                stack.extend(kids.iter().copied());
            }
        }
        let mut found = false;
        let mut total = 0u64;
        for p in &seen {
            if let Some(proc_) = procs.get(p) {
                found = true;
                total = total.saturating_add(proc_.memory());
            }
        }
        found.then_some(total)
    }
}

#[cfg(unix)]
fn same_group(p: sysinfo::Pid, group: u32) -> bool {
    use nix::unistd::{Pid, getpgid};
    let (Ok(p), Ok(g)) = (i32::try_from(p.as_u32()), i32::try_from(group)) else {
        return false;
    };
    getpgid(Some(Pid::from_raw(p))).is_ok_and(|pg| pg == Pid::from_raw(g))
}

/// A probe that reports fixed, adjustable figures. For tests and demos.
#[derive(Debug)]
pub struct ManualProbe {
    total: AtomicU64,
    available: AtomicU64,
    disk_free: AtomicU64,
    disk_total: AtomicU64,
    /// `u64::MAX` means "unknown".
    tree: AtomicU64,
}

impl ManualProbe {
    /// A machine with `total` memory of which `available` is free, and a
    /// 1 TiB disk with 500 GiB free. Process trees measure as unknown.
    #[must_use]
    pub fn new(total: u64, available: u64) -> Self {
        Self {
            total: AtomicU64::new(total),
            available: AtomicU64::new(available),
            disk_free: AtomicU64::new(500 * GIB),
            disk_total: AtomicU64::new(1024 * GIB),
            tree: AtomicU64::new(u64::MAX),
        }
    }

    /// A roomy 64 GiB machine with 48 GiB free.
    #[must_use]
    pub fn roomy() -> Self {
        Self::new(64 * GIB, 48 * GIB)
    }

    /// Changes the available memory.
    pub fn set_available(&self, bytes: u64) {
        self.available.store(bytes, Ordering::SeqCst);
    }

    /// Changes the free disk space.
    pub fn set_disk_free(&self, bytes: u64) {
        self.disk_free.store(bytes, Ordering::SeqCst);
    }

    /// Sets what every process tree measures, or `None` for unknown.
    pub fn set_tree_memory(&self, bytes: Option<u64>) {
        self.tree.store(bytes.unwrap_or(u64::MAX), Ordering::SeqCst);
    }
}

impl Probe for ManualProbe {
    fn memory(&self) -> Option<Memory> {
        Some(Memory {
            total: self.total.load(Ordering::SeqCst),
            available: self.available.load(Ordering::SeqCst),
        })
    }

    fn disk(&self, _path: &Path) -> Option<DiskSpace> {
        Some(DiskSpace {
            free: self.disk_free.load(Ordering::SeqCst),
            total: self.disk_total.load(Ordering::SeqCst),
        })
    }

    fn tree_memory(&self, _pid: u32) -> Option<u64> {
        let v = self.tree.load(Ordering::SeqCst);
        (v != u64::MAX).then_some(v)
    }
}

// ── settings ───────────────────────────────────────────────────────────────

/// What the owner asked for. `None` picks the default for this machine.
#[derive(Debug, Clone, Default)]
pub struct ResourceSettings {
    /// Most runs at once, across all agents.
    pub max_running: Option<usize>,
    /// Memory kept free for the owner. Zero disables memory admission.
    pub memory_reserve: Option<Size>,
    /// Memory one run's process tree may use.
    pub run_memory_limit: Option<Size>,
    /// Free space kept on the volume holding the Hashi home. Zero disables.
    pub disk_floor: Option<Size>,
    /// Niceness for agent runs (Unix; on Windows any value above zero means
    /// below-normal priority). Zero disables.
    pub nice: Option<u8>,
    /// Measurement source; the real machine when `None`.
    pub probe: Option<Arc<dyn Probe>>,
    /// Wait between admission retries; [`RETRY_AFTER`] when `None`.
    pub retry_after: Option<Duration>,
    /// Watchdog sampling interval; [`SAMPLE_EVERY`] when `None`.
    pub sample_every: Option<Duration>,
}

impl ResourceSettings {
    /// Defaults everywhere, measured through `probe`.
    #[must_use]
    pub fn with_probe(probe: Arc<dyn Probe>) -> Self {
        Self {
            probe: Some(probe),
            ..Self::default()
        }
    }
}

/// Defaults derived from the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Defaults {
    /// `clamp(total_gib / 8, 1, cpus)`.
    pub max_running: usize,
    /// `max(2 GiB, 20% of total)`.
    pub memory_reserve: u64,
    /// `max(2 GiB, 40% of total)`.
    pub run_memory_limit: u64,
}

/// Computes the defaults for a machine with `total` bytes of memory and
/// `cpus` logical CPUs.
#[must_use]
pub fn defaults(total: u64, cpus: usize) -> Defaults {
    let per_eight = usize::try_from(total / GIB / 8).unwrap_or(usize::MAX);
    Defaults {
        max_running: per_eight.clamp(1, cpus.max(1)),
        memory_reserve: (2 * GIB).max(total / 5),
        run_memory_limit: (2 * GIB).max(total / 5 * 2),
    }
}

/// Settings after defaults are applied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GuardConfig {
    /// Most runs at once.
    pub max_running: usize,
    /// Bytes kept free for the owner.
    pub memory_reserve: u64,
    /// Bytes one run's process tree may use.
    pub run_memory_limit: u64,
    /// Free space kept under the Hashi home (a percentage is of the volume).
    pub disk_floor: Size,
    /// Memory a new run is assumed to need.
    pub expected_need: u64,
    /// Niceness for agent runs.
    pub nice: u8,
    /// Wait between admission retries.
    pub retry_after: Duration,
    /// Watchdog sampling interval.
    pub sample_every: Duration,
}

impl GuardConfig {
    /// Applies the defaults for a machine with `total` memory and `cpus` CPUs.
    #[must_use]
    pub fn resolve(s: &ResourceSettings, total: u64, cpus: usize) -> Self {
        let d = defaults(total, cpus);
        Self {
            max_running: s.max_running.unwrap_or(d.max_running).max(1),
            memory_reserve: s
                .memory_reserve
                .map_or(d.memory_reserve, |v| v.resolve(total)),
            run_memory_limit: s
                .run_memory_limit
                .map_or(d.run_memory_limit, |v| v.resolve(total))
                .max(1),
            disk_floor: s.disk_floor.unwrap_or(DEFAULT_DISK_FLOOR),
            expected_need: EXPECTED_RUN_MEMORY,
            nice: s.nice.unwrap_or(if cfg!(unix) { DEFAULT_NICE } else { 0 }),
            retry_after: s.retry_after.unwrap_or(RETRY_AFTER),
            sample_every: s.sample_every.unwrap_or(SAMPLE_EVERY),
        }
    }
}

// ── the guard ──────────────────────────────────────────────────────────────

/// Why admission is holding work back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HoldReason {
    /// Starting a run would eat into the owner's memory reserve.
    Memory,
    /// The volume holding the Hashi home is below its floor.
    Disk,
}

impl HoldReason {
    /// Short label for the dashboard.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Memory => "low memory",
            Self::Disk => "disk",
        }
    }
}

impl fmt::Display for HoldReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// A failed admission check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hold {
    /// Which resource is short.
    pub reason: HoldReason,
    /// Human-readable figures, for the owner's log only.
    pub detail: String,
}

/// Per-run memory ceiling handed to the runner.
#[derive(Debug, Clone)]
pub struct MemoryWatch {
    /// Bytes the run's process tree may use.
    pub limit: u64,
    /// Sampling interval.
    pub every: Duration,
    /// Measurement source.
    pub probe: Arc<dyn Probe>,
}

/// A slot in the global run cap. Dropping it frees the slot.
#[derive(Debug)]
pub struct RunPermit {
    _permit: OwnedSemaphorePermit,
    running: Option<Arc<AtomicUsize>>,
}

impl RunPermit {
    /// Counts this slot as running until it is dropped.
    pub fn mark_running(&mut self, counter: &Arc<AtomicUsize>) {
        if self.running.is_none() {
            counter.fetch_add(1, Ordering::SeqCst);
            self.running = Some(Arc::clone(counter));
        }
    }
}

impl Drop for RunPermit {
    fn drop(&mut self) {
        if let Some(c) = self.running.take() {
            c.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// What the admin stats and the dashboard show. Never exposed to non-admins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSnapshot {
    /// Machine memory, when known.
    pub memory_total: Option<u64>,
    /// Available memory, when known.
    pub memory_available: Option<u64>,
    /// Memory kept free for the owner.
    pub reserve: u64,
    /// Per-run memory limit.
    pub run_memory_limit: u64,
    /// Runs in progress.
    pub running: usize,
    /// Most runs at once.
    pub max_running: usize,
    /// Why work is held back, if it is.
    pub held: Option<HoldReason>,
    /// Free space under the Hashi home, when known.
    pub disk_free: Option<u64>,
    /// Free space kept under the Hashi home, when known.
    pub disk_floor: Option<u64>,
}

#[derive(Debug, Default)]
struct HeldState {
    reason: Option<HoldReason>,
    last_notice: Option<Instant>,
}

/// Shared admission control for every worker.
#[derive(Debug)]
pub struct ResourceGuard {
    cfg: GuardConfig,
    probe: Arc<dyn Probe>,
    home: PathBuf,
    permits: Arc<Semaphore>,
    running: Arc<AtomicUsize>,
    held: Mutex<HeldState>,
    bus: Bus,
}

impl ResourceGuard {
    /// Builds the guard, measuring the machine once for the defaults.
    #[must_use]
    pub fn new(settings: &ResourceSettings, home: PathBuf, bus: Bus) -> Self {
        let probe: Arc<dyn Probe> = settings
            .probe
            .clone()
            .unwrap_or_else(|| Arc::new(SystemProbe::new()));
        let total = probe.memory().map_or(0, |m| m.total);
        let cpus = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        let cfg = GuardConfig::resolve(settings, total, cpus);
        Self {
            permits: Arc::new(Semaphore::new(cfg.max_running)),
            cfg,
            probe,
            home,
            running: Arc::new(AtomicUsize::new(0)),
            held: Mutex::new(HeldState::default()),
            bus,
        }
    }

    /// The resolved settings.
    #[must_use]
    pub const fn config(&self) -> &GuardConfig {
        &self.cfg
    }

    /// One-line summary for the startup log.
    #[must_use]
    pub fn describe(&self) -> String {
        let c = &self.cfg;
        let total = self.probe.memory().map(|m| m.total);
        let mut s = format!(
            "resource guard: up to {} run{} at once, {} kept free, {} per run, disk floor {}",
            c.max_running,
            if c.max_running == 1 { "" } else { "s" },
            human_bytes(c.memory_reserve),
            human_bytes(c.run_memory_limit),
            c.disk_floor,
        );
        if let Some(total) = total
            && c.memory_reserve.saturating_add(c.expected_need) >= total
        {
            s.push_str(" (warning: the reserve leaves no room for any run)");
        }
        s
    }

    /// Waits for a free slot. `None` only if the guard is shutting down.
    pub async fn acquire(&self) -> Option<RunPermit> {
        let permit = Arc::clone(&self.permits).acquire_owned().await.ok()?;
        Some(RunPermit {
            _permit: permit,
            running: None,
        })
    }

    /// Counts `permit` as a running run.
    pub fn start(&self, permit: &mut RunPermit) {
        permit.mark_running(&self.running);
    }

    /// Checks memory and disk for one more run.
    ///
    /// # Errors
    ///
    /// Returns the [`Hold`] that should keep the work queued. A probe that
    /// cannot measure never holds work back.
    pub fn check(&self) -> Result<(), Hold> {
        if let Some(m) = self.probe.memory() {
            let after = m.available.saturating_sub(self.cfg.expected_need);
            if after < self.cfg.memory_reserve {
                return Err(Hold {
                    reason: HoldReason::Memory,
                    detail: format!(
                        "{} available, {} kept free for you",
                        human_bytes(m.available),
                        human_bytes(self.cfg.memory_reserve)
                    ),
                });
            }
        }
        if !self.cfg.disk_floor.is_zero()
            && let Some(d) = self.probe.disk(&self.home)
        {
            let floor = self.cfg.disk_floor.resolve(d.total);
            if d.free < floor {
                return Err(Hold {
                    reason: HoldReason::Disk,
                    detail: format!(
                        "{} free under the Hashi home, floor is {}",
                        human_bytes(d.free),
                        human_bytes(floor)
                    ),
                });
            }
        }
        Ok(())
    }

    /// Records that work is held, telling the owner at most once a minute.
    pub fn hold(&self, hold: &Hold) {
        let Ok(mut st) = self.held.lock() else {
            return;
        };
        st.reason = Some(hold.reason);
        if st
            .last_notice
            .is_none_or(|t| t.elapsed() >= HELD_NOTICE_EVERY)
        {
            st.last_notice = Some(Instant::now());
            warn!(reason = %hold.reason, detail = %hold.detail, "holding queued work");
            self.bus.emit(Event::Held {
                reason: hold.reason,
                detail: hold.detail.clone(),
            });
        }
    }

    /// Clears the held state after a successful check.
    pub fn clear_hold(&self) {
        let was = self.held.lock().ok().and_then(|mut st| st.reason.take());
        if let Some(reason) = was {
            self.bus
                .log(format!("resources recovered ({reason}); held work resumes"));
        }
    }

    /// Why work is held back right now, if it is.
    #[must_use]
    pub fn held(&self) -> Option<HoldReason> {
        self.held.lock().ok().and_then(|st| st.reason)
    }

    /// Runs in progress.
    #[must_use]
    pub fn running(&self) -> usize {
        self.running.load(Ordering::SeqCst)
    }

    /// The watchdog settings for one run.
    #[must_use]
    pub fn watch(&self) -> MemoryWatch {
        MemoryWatch {
            limit: self.cfg.run_memory_limit,
            every: self.cfg.sample_every,
            probe: Arc::clone(&self.probe),
        }
    }

    /// Current figures for admins.
    #[must_use]
    pub fn snapshot(&self) -> ResourceSnapshot {
        let mem = self.probe.memory();
        let disk = self.probe.disk(&self.home);
        ResourceSnapshot {
            memory_total: mem.map(|m| m.total),
            memory_available: mem.map(|m| m.available),
            reserve: self.cfg.memory_reserve,
            run_memory_limit: self.cfg.run_memory_limit,
            running: self.running(),
            max_running: self.cfg.max_running,
            held: self.held(),
            disk_free: disk.map(|d| d.free),
            disk_floor: disk.map(|d| self.cfg.disk_floor.resolve(d.total)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("512M"), Ok(Size::Bytes(512 * MIB)));
        assert_eq!(parse_size("4G"), Ok(Size::Bytes(4 * GIB)));
        assert_eq!(parse_size(" 4gib "), Ok(Size::Bytes(4 * GIB)));
        assert_eq!(parse_size("1.5G"), Ok(Size::Bytes(3 * GIB / 2)));
        assert_eq!(parse_size("64k"), Ok(Size::Bytes(64 * 1024)));
        assert_eq!(parse_size("2T"), Ok(Size::Bytes(2048 * GIB)));
        assert_eq!(parse_size("100b"), Ok(Size::Bytes(100)));
        assert_eq!(parse_size("25%"), Ok(Size::Percent(25.0)));
        assert_eq!(parse_size("12.5 %"), Ok(Size::Percent(12.5)));
        assert_eq!(parse_size("0"), Ok(Size::Bytes(0)));
        assert_eq!(parse_size("0%"), Ok(Size::Percent(0.0)));
    }

    #[test]
    fn rejects_garbage_sizes() {
        for bad in [
            "", " ", "abc", "G", "4X", "-1G", "+1G", "1.2.3G", "1e9", "inf", "nan%", "101%", "-5%",
            "4 G B", "12", "0x10M",
        ] {
            assert!(parse_size(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn nonzero_sizes_reject_zero() {
        assert!(parse_nonzero_size("0").is_err());
        assert!(parse_nonzero_size("0G").is_err());
        assert!(parse_nonzero_size("0%").is_err());
        assert_eq!(parse_nonzero_size("1G"), Ok(Size::Bytes(GIB)));
    }

    #[test]
    fn sizes_resolve_against_a_total() {
        assert_eq!(Size::Percent(25.0).resolve(16 * GIB), 4 * GIB);
        assert_eq!(Size::Bytes(7).resolve(16 * GIB), 7);
        assert_eq!(Size::Bytes(2 * GIB).to_string(), "2.0 GiB");
        assert_eq!(Size::Percent(25.0).to_string(), "25%");
    }

    #[test]
    fn defaults_fit_a_16_gib_laptop() {
        let d = defaults(16 * GIB, 8);
        assert_eq!(d.max_running, 2);
        assert_eq!(d.memory_reserve, 16 * GIB / 5);
        assert_eq!(d.run_memory_limit, 16 * GIB / 5 * 2);
    }

    #[test]
    fn defaults_clamp_to_cpus_and_floors() {
        assert_eq!(defaults(4 * GIB, 8).max_running, 1);
        assert_eq!(defaults(4 * GIB, 8).memory_reserve, 2 * GIB);
        assert_eq!(defaults(4 * GIB, 8).run_memory_limit, 2 * GIB);
        assert_eq!(defaults(256 * GIB, 4).max_running, 4);
        assert_eq!(defaults(0, 0).max_running, 1);
    }

    #[test]
    fn explicit_settings_win() {
        let s = ResourceSettings {
            max_running: Some(5),
            memory_reserve: Some(Size::Percent(50.0)),
            run_memory_limit: Some(Size::Bytes(GIB)),
            nice: Some(0),
            ..ResourceSettings::default()
        };
        let c = GuardConfig::resolve(&s, 16 * GIB, 2);
        assert_eq!(c.max_running, 5);
        assert_eq!(c.memory_reserve, 8 * GIB);
        assert_eq!(c.run_memory_limit, GIB);
        assert_eq!(c.nice, 0);
        assert_eq!(c.disk_floor, DEFAULT_DISK_FLOOR);
    }

    fn guard(probe: &Arc<ManualProbe>) -> ResourceGuard {
        let p: Arc<dyn Probe> = probe.clone();
        ResourceGuard::new(
            &ResourceSettings::with_probe(p),
            PathBuf::from("."),
            Bus::default(),
        )
    }

    #[test]
    fn admission_keeps_the_reserve_free() {
        let probe = Arc::new(ManualProbe::new(16 * GIB, 8 * GIB));
        let g = guard(&probe);
        assert!(g.check().is_ok());
        // Reserve is 3.2 GiB and a run is assumed to need 512 MiB.
        probe.set_available(3 * GIB);
        let hold = g.check().unwrap_err();
        assert_eq!(hold.reason, HoldReason::Memory);
        probe.set_available(8 * GIB);
        probe.set_disk_free(100 * MIB);
        assert_eq!(g.check().unwrap_err().reason, HoldReason::Disk);
    }

    #[test]
    fn held_state_is_reported_and_cleared() {
        let probe = Arc::new(ManualProbe::new(16 * GIB, GIB));
        let g = guard(&probe);
        let bus_rx = g.bus.subscribe();
        let hold = g.check().unwrap_err();
        g.hold(&hold);
        g.hold(&hold);
        assert_eq!(g.held(), Some(HoldReason::Memory));
        assert_eq!(g.snapshot().held, Some(HoldReason::Memory));
        // Only one notice for the two holds within a minute.
        let mut rx = bus_rx;
        let mut notices = 0;
        while let Ok(ev) = rx.try_recv() {
            if matches!(ev, Event::Held { .. }) {
                notices += 1;
            }
        }
        assert_eq!(notices, 1);
        g.clear_hold();
        assert_eq!(g.held(), None);
    }

    #[tokio::test]
    async fn permits_cap_running_work() {
        let probe = Arc::new(ManualProbe::new(16 * GIB, 8 * GIB));
        let g = guard(&probe);
        assert_eq!(
            g.config().max_running,
            2.min(std::thread::available_parallelism().map_or(1, std::num::NonZero::get))
        );
        let mut a = g.acquire().await.unwrap();
        g.start(&mut a);
        assert_eq!(g.running(), 1);
        drop(a);
        assert_eq!(g.running(), 0);
    }

    #[test]
    fn system_probe_measures_this_process() {
        let probe = SystemProbe::new();
        let mem = probe.memory().unwrap();
        assert!(mem.total > 0 && mem.available <= mem.total);
        let own = probe.tree_memory(std::process::id()).unwrap();
        assert!(own > 0);
        assert!(probe.disk(Path::new(".")).is_some());
    }
}
