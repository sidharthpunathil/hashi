//! Which processes belong to a run, and how to stop every one of them.
//!
//! A run's leader starts in its own process group, but a descendant can move
//! to another group (`setpgid`) or session (`setsid`), and once its parent
//! exits it is reparented away from the run. Neither `killpg` on the leader's
//! group nor a walk over live parent links finds it then. So whenever the
//! watchdog samples a run, it records every process group and session it sees
//! the run's processes in (never Hashi's own), and when the run ends every
//! live process in any recorded group or session is killed. A group or
//! session id is not reused while any process is still in it, so a recorded
//! id cannot later point at an unrelated process.
//!
//! On Linux Hashi is also a child subreaper, so a descendant whose parent
//! exits is reparented to Hashi instead of to init. While only one run is in
//! progress, a reparented process in a session of its own (anything that
//! called `setsid`) is counted as part of that run, even if the watchdog
//! never saw the process that created it. With several runs at once such an
//! orphan cannot be traced to one of them, so it is left alone until the last
//! of them ends and its sweep, then the only run, kills it. Hashi reaps those
//! processes itself.
//!
//! Residual limit: on other Unix systems, a process that leaves its session
//! and whose parent exits between two samples cannot be traced back to the
//! run, so it is neither measured nor killed.

use std::collections::{BTreeSet, HashMap, HashSet};

use sysinfo::{Pid, Process};

/// Process groups and sessions a run's processes have been seen in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunScope {
    groups: BTreeSet<u32>,
    sessions: BTreeSet<u32>,
}

impl RunScope {
    /// The scope of a run whose leader `leader` heads its own process group.
    #[must_use]
    pub fn new(leader: u32) -> Self {
        let mut scope = Self::default();
        if leader > 1 {
            scope.groups.insert(leader);
        }
        scope
    }

    /// Recorded process group ids.
    pub fn groups(&self) -> impl Iterator<Item = u32> + '_ {
        self.groups.iter().copied()
    }

    /// Recorded session ids.
    pub fn sessions(&self) -> impl Iterator<Item = u32> + '_ {
        self.sessions.iter().copied()
    }
}

/// Every process that belongs to the run led by `root`, as found in `procs`.
///
/// On Unix this is the root (when `root_alive`), every process in a recorded
/// group or session, every reparented orphan in a session of its own (Linux),
/// and all of their descendants. The group and session of each one are added
/// to `scope`, and the search repeats until it finds nothing new. Elsewhere
/// it is the root and its descendants.
#[must_use]
pub fn members(
    procs: &HashMap<Pid, Process>,
    root: u32,
    root_alive: bool,
    scope: &mut RunScope,
) -> HashSet<Pid> {
    let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
    for (p, proc_) in procs {
        if let Some(parent) = proc_.parent() {
            children.entry(parent).or_default().push(*p);
        }
    }
    let mut seen = HashSet::new();
    let mut stack = Vec::new();
    if root_alive {
        stack.push(Pid::from_u32(root));
    }
    #[cfg(unix)]
    {
        let own = unix::Ids::own();
        let leaders = unix::leaders();
        let sole_run = leaders.iter().all(|l| *l == root);
        let ids: HashMap<Pid, unix::Ids> = procs
            .keys()
            .filter(|p| p.as_u32() > 1)
            .map(|p| (*p, unix::Ids::of(p.as_u32())))
            .collect();
        loop {
            for (p, id) in &ids {
                if seen.contains(p) {
                    continue;
                }
                let in_scope = id.pgid.is_some_and(|g| scope.groups.contains(&g))
                    || id.sid.is_some_and(|s| scope.sessions.contains(&s));
                // Only a subreaper inherits other processes' orphans. An
                // orphan no run recorded cannot be traced to one, so it is
                // claimed only while this is the sole run in progress: with
                // several runs, one run must never be charged for, or kill,
                // another's processes. The last run to end sweeps it up.
                let adopted = cfg!(target_os = "linux")
                    && sole_run
                    && procs.get(p).and_then(Process::parent) == Some(Pid::from_u32(own.pid))
                    && !leaders.contains(&p.as_u32())
                    && id.sid.is_some_and(|s| Some(s) != own.sid);
                if in_scope || adopted {
                    stack.push(*p);
                }
            }
            if stack.is_empty() {
                break;
            }
            while let Some(p) = stack.pop() {
                if p.as_u32() == own.pid && p.as_u32() != root {
                    continue;
                }
                if !seen.insert(p) {
                    continue;
                }
                if let Some(id) = ids.get(&p) {
                    if let Some(g) = id.pgid.filter(|g| *g > 1 && Some(*g) != own.pgid) {
                        scope.groups.insert(g);
                    }
                    if let Some(s) = id.sid.filter(|s| *s > 1 && Some(*s) != own.sid) {
                        scope.sessions.insert(s);
                    }
                }
                if let Some(kids) = children.get(&p) {
                    stack.extend(kids.iter().copied());
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = scope;
        while let Some(p) = stack.pop() {
            if seen.insert(p)
                && let Some(kids) = children.get(&p)
            {
                stack.extend(kids.iter().copied());
            }
        }
    }
    seen.retain(|p| procs.contains_key(p));
    seen
}

#[cfg(unix)]
pub(crate) use unix::{register_leader, sweep};

#[cfg(unix)]
mod unix {
    use std::collections::BTreeSet;
    use std::sync::{Mutex, PoisonError};
    use std::time::{Duration, Instant};

    use nix::sys::signal::{Signal, kill, killpg};
    use nix::unistd::{Pid as NixPid, getpgid, getpgrp, getsid};
    use sysinfo::{ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System};

    use super::{RunScope, members};

    /// Leaders of the runs in progress. Their own Hashi task waits for them,
    /// so they are never treated as orphans or reaped here.
    static LEADERS: Mutex<BTreeSet<u32>> = Mutex::new(BTreeSet::new());

    /// Records `pid` as a run leader until [`sweep`] has run for it, and on
    /// Linux makes Hashi a child subreaper the first time.
    pub(crate) fn register_leader(pid: u32) {
        #[cfg(target_os = "linux")]
        {
            static SUBREAPER: std::sync::Once = std::sync::Once::new();
            SUBREAPER.call_once(|| {
                if let Err(e) = nix::sys::prctl::set_child_subreaper(true) {
                    tracing::warn!(error = %e, "could not become a child subreaper");
                }
            });
        }
        LEADERS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(pid);
    }

    pub(super) fn leaders() -> BTreeSet<u32> {
        LEADERS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Process group and session of one process.
    #[derive(Debug, Clone, Copy)]
    pub(super) struct Ids {
        pub(super) pid: u32,
        pub(super) pgid: Option<u32>,
        pub(super) sid: Option<u32>,
    }

    fn raw(pid: u32) -> Option<NixPid> {
        i32::try_from(pid)
            .ok()
            .filter(|p| *p > 1)
            .map(NixPid::from_raw)
    }

    fn id(pid: nix::Result<NixPid>) -> Option<u32> {
        pid.ok().and_then(|p| u32::try_from(p.as_raw()).ok())
    }

    impl Ids {
        pub(super) fn own() -> Self {
            Self {
                pid: std::process::id(),
                pgid: id(Ok(getpgrp())),
                sid: id(getsid(None)),
            }
        }

        pub(super) fn of(pid: u32) -> Self {
            let target = raw(pid);
            Self {
                pid,
                pgid: target.and_then(|p| id(getpgid(Some(p)))),
                sid: target.and_then(|p| id(getsid(Some(p)))),
            }
        }

        fn in_scope(&self, scope: &RunScope) -> bool {
            self.pgid.is_some_and(|g| scope.groups.contains(&g))
                || self.sid.is_some_and(|s| scope.sessions.contains(&s))
        }
    }

    /// Kills every process of the run led by `root`: its recorded groups,
    /// then each live member found by a fresh scan, repeated until a scan
    /// finds nothing left alive. `exited` says the leader itself has already
    /// been waited for, so its pid is no longer used as a starting point.
    pub(crate) fn sweep(root: u32, exited: bool, scope: &mut RunScope) {
        let own = Ids::own();
        let mut sys = System::new();
        let mut killed: BTreeSet<u32> = BTreeSet::new();
        for _ in 0..5 {
            for g in scope.groups() {
                if Some(g) != own.pgid
                    && let Some(g) = raw(g)
                {
                    // ESRCH (nothing left in the group) is the normal case.
                    let _ = killpg(g, Signal::SIGKILL);
                }
            }
            sys.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::nothing().without_tasks(),
            );
            let procs = sys.processes();
            let found = members(procs, root, !exited, scope);
            let leaders = leaders();
            let mut alive = 0usize;
            for p in found {
                let pid = p.as_u32();
                if pid == own.pid || (pid != root && leaders.contains(&pid)) {
                    continue;
                }
                if pid != root {
                    killed.insert(pid);
                }
                if procs
                    .get(&p)
                    .is_some_and(|x| x.status() == ProcessStatus::Zombie)
                {
                    continue;
                }
                if let Some(target) = raw(pid) {
                    let _ = kill(target, Signal::SIGKILL);
                    alive += 1;
                }
            }
            if alive == 0 {
                break;
            }
        }
        LEADERS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&root);
        if cfg!(target_os = "linux") && !killed.is_empty() {
            let scope = scope.clone();
            let _ = std::thread::Builder::new()
                .name("hashi-reaper".to_owned())
                .spawn(move || reap(&killed, &scope));
        }
    }

    /// Waits for killed run processes that end up as Hashi's children (as a
    /// subreaper inherits them), so none is left a zombie. A pid is only
    /// waited for while it is still in the run's groups or sessions, so a
    /// reused pid that now belongs to someone else is never touched.
    fn reap(pids: &BTreeSet<u32>, scope: &RunScope) {
        use nix::errno::Errno;
        use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};

        let mut pending: Vec<u32> = pids.iter().copied().collect();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !pending.is_empty() && Instant::now() < deadline {
            pending.retain(|&pid| {
                let Some(target) = raw(pid) else {
                    return false;
                };
                if !Ids::of(pid).in_scope(scope) {
                    return false;
                }
                match waitpid(target, Some(WaitPidFlag::WNOHANG)) {
                    Ok(WaitStatus::StillAlive) => true,
                    Ok(_) => false,
                    // Not (yet) Hashi's child: keep it while it still exists,
                    // since it may be reparented to Hashi once its parent dies.
                    Err(Errno::ECHILD) => kill(target, None).is_ok(),
                    Err(_) => false,
                }
            });
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
