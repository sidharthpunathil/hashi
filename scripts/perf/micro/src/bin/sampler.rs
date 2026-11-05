//! Cost of one watchdog sample, `SystemProbe::tree_memory`, which the runner
//! calls every two seconds for each run, with a ten-process tree below this
//! one. Also times the raw sysinfo refreshes it is made of.
//!
//! usage: sampler [SAMPLES]
use std::time::Instant;

use hashi::guard::{Probe, SystemProbe};
use nix::sys::resource::{UsageWho, getrusage};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

fn cpu_secs() -> f64 {
    let Ok(u) = getrusage(UsageWho::RUSAGE_SELF) else {
        return 0.0;
    };
    let secs = |tv: nix::sys::time::TimeVal| tv.tv_sec() as f64 + tv.tv_usec() as f64 / 1e6;
    secs(u.user_time()) + secs(u.system_time())
}

fn bench(label: &str, n: usize, mut f: impl FnMut()) {
    f();
    let (wall, cpu) = (Instant::now(), cpu_secs());
    for _ in 0..n {
        f();
    }
    let wall_ms = wall.elapsed().as_secs_f64() / n as f64 * 1e3;
    let cpu_ms = (cpu_secs() - cpu) / n as f64 * 1e3;
    println!("{label}: wall {wall_ms:.2} ms, cpu {cpu_ms:.2} ms per sample");
}

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);
    let me = std::process::id();
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    println!("processes: {}", sys.processes().len());

    let mut kids: Vec<_> = (0..10)
        .filter_map(|_| std::process::Command::new("sleep").arg("120").spawn().ok())
        .collect();
    let probe = SystemProbe::new();
    println!("tree memory: {:?} bytes", probe.tree_memory(me));
    bench("SystemProbe::tree_memory", n, || {
        std::hint::black_box(probe.tree_memory(me));
    });
    for k in &mut kids {
        let _ = k.kill();
        let _ = k.wait();
    }

    let quiet = ProcessRefreshKind::nothing().without_tasks();
    for (label, kind) in [
        ("refresh all, nothing", quiet),
        ("refresh all, memory", quiet.with_memory()),
    ] {
        let mut sys = System::new();
        bench(label, n, || {
            sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
        });
    }
    let mut sys = System::new();
    let one = [Pid::from_u32(me)];
    bench("refresh one pid, memory", n, || {
        sys.refresh_processes_specifics(ProcessesToUpdate::Some(&one), true, quiet.with_memory());
    });
}
