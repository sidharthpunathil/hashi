//! Peak memory of `runner::run` capturing one large stdout.
//!
//! usage: runner [BYTES]
use std::ffi::OsString;
use std::path::Path;
use std::time::Instant;

use hashi::agent::runner::{Limits, run};
use nix::sys::resource::{UsageWho, getrusage};

/// The process's peak resident size so far, in KiB.
fn max_rss_kib() -> i64 {
    let Ok(u) = getrusage(UsageWho::RUSAGE_SELF) else {
        return 0;
    };
    // macOS reports bytes, Linux KiB.
    if cfg!(target_os = "macos") {
        u.max_rss() / 1024
    } else {
        u.max_rss()
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let bytes: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(10_000_000);
    let script = format!("head -c {bytes} /dev/zero | tr '\\0' a");
    let args: Vec<OsString> = vec!["-c".into(), script.into()];
    let before = max_rss_kib();
    let started = Instant::now();
    let out = run(
        Path::new("/bin/sh"),
        &args,
        &[],
        &std::env::temp_dir(),
        Limits::default(),
    )
    .await;
    let ms = started.elapsed().as_secs_f64() * 1e3;
    match out {
        Ok(out) if out.stdout.len() == bytes => println!(
            "{{\"bytes\": {bytes}, \"peak_growth_kib\": {}, \"ms\": {ms:.1}}}",
            max_rss_kib() - before
        ),
        Ok(out) => eprintln!("short output: {} bytes", out.stdout.len()),
        Err(e) => eprintln!("run failed: {e}"),
    }
}
