//! Update check and self-update against a local fake GitHub. No real network.

// Test helpers outside #[test] functions are not covered by clippy.toml's
// test exemptions; panicking on setup failure is the desired behaviour here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hashi::events::{Bus, Event};
use hashi::update::{self, Sources};

type Routes = Arc<Mutex<HashMap<String, (u16, Vec<u8>)>>>;

/// A tiny HTTP/1.1 server: path to (status, body).
struct Fake {
    base: String,
    routes: Routes,
    hits: Arc<AtomicUsize>,
    agents: Arc<Mutex<Vec<String>>>,
}

impl Fake {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let routes: Routes = Arc::default();
        let hits = Arc::new(AtomicUsize::new(0));
        let agents: Arc<Mutex<Vec<String>>> = Arc::default();
        let (r, h, a) = (routes.clone(), hits.clone(), agents.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                if reader.read_line(&mut first).is_err() {
                    continue;
                }
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some((k, v)) = line.split_once(':')
                        && k.eq_ignore_ascii_case("user-agent")
                    {
                        a.lock().unwrap().push(v.trim().to_owned());
                    }
                }
                h.fetch_add(1, Ordering::SeqCst);
                let path = first.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let (status, body) = r
                    .lock()
                    .unwrap()
                    .get(&path)
                    .cloned()
                    .unwrap_or((404, b"not found".to_vec()));
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        Self {
            base,
            routes,
            hits,
            agents,
        }
    }

    fn route(&self, path: &str, status: u16, body: impl Into<Vec<u8>>) {
        self.routes
            .lock()
            .unwrap()
            .insert(path.to_owned(), (status, body.into()));
    }

    fn latest(&self, tag: &str) {
        self.route(
            "/latest",
            200,
            format!(
                r#"{{"tag_name":"{tag}","html_url":"https://github.com/sidharthpunathil/hashi/releases/tag/{tag}","assets":[]}}"#
            ),
        );
    }

    fn sources(&self) -> Sources {
        Sources {
            api: format!("{}/latest", self.base),
            downloads: format!("{}/dl", self.base),
        }
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

#[test]
fn check_uses_and_respects_the_cache() {
    let fake = Fake::start();
    fake.latest("v99.1.0");
    let home = tempfile::tempdir().unwrap();
    let api = fake.sources().api;
    let now = 1_700_000_000_000;

    let r = update::check(home.path(), &api, now, false).unwrap();
    assert_eq!(r.tag, "v99.1.0");
    assert_eq!(r.version.to_string(), "99.1.0");
    assert_eq!(fake.hits(), 1);
    assert_eq!(
        fake.agents.lock().unwrap()[0],
        format!("hashi/{}", env!("CARGO_PKG_VERSION"))
    );

    // Fresh: answered from the cache.
    let r = update::check(home.path(), &api, now + 1000, false).unwrap();
    assert_eq!(r.tag, "v99.1.0");
    assert_eq!(fake.hits(), 1);

    // Forced (doctor) or stale: asks again.
    update::check(home.path(), &api, now + 1000, true).unwrap();
    assert_eq!(fake.hits(), 2);
    update::check(home.path(), &api, now + 1000 + update::CACHE_TTL_MS, false).unwrap();
    assert_eq!(fake.hits(), 3);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(home.path().join(update::CACHE_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn a_failed_check_is_cached_too() {
    let fake = Fake::start();
    fake.route("/latest", 500, "boom");
    let home = tempfile::tempdir().unwrap();
    let api = fake.sources().api;
    let now = 1_700_000_000_000;
    assert!(update::check(home.path(), &api, now, false).is_err());
    assert_eq!(fake.hits(), 1);
    assert!(update::check(home.path(), &api, now + 60_000, false).is_err());
    assert_eq!(
        fake.hits(),
        1,
        "an offline machine must not retry every start"
    );
    assert!(update::available(home.path(), &api, now + 60_000).is_none());
}

#[test]
fn no_cache_is_written_without_a_home() {
    let fake = Fake::start();
    fake.latest("v99.1.0");
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("missing");
    update::check(&home, &fake.sources().api, 0, false).unwrap();
    assert!(!home.exists());
}

#[test]
fn background_check_emits_update_available() {
    let fake = Fake::start();
    fake.latest("v99.2.0");
    let home = tempfile::tempdir().unwrap();
    let bus = Bus::default();
    let mut rx = bus.subscribe();
    update::spawn_check(home.path().to_path_buf(), fake.sources().api, bus);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match rx.try_recv() {
            Ok(Event::UpdateAvailable { version, url }) => {
                assert_eq!(version, "99.2.0");
                assert!(url.ends_with("/tag/v99.2.0"));
                break;
            }
            Ok(_) => {}
            Err(_) => {
                assert!(Instant::now() < deadline, "no UpdateAvailable event");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

#[test]
fn no_update_when_already_latest() {
    let fake = Fake::start();
    fake.latest("v0.0.1");
    let home = tempfile::tempdir().unwrap();
    assert!(update::available(home.path(), &fake.sources().api, 0).is_none());
}

#[test]
fn replace_aside_moves_the_old_binary_out_and_cleans_up() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("hashi.exe");
    let new = dir.path().join("new.exe");
    std::fs::write(&target, "old").unwrap();
    std::fs::write(&new, "new").unwrap();
    update::replace_aside(&new, &target).unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
    let old = dir.path().join(update::OLD_EXE);
    assert_eq!(std::fs::read_to_string(&old).unwrap(), "old");
    update::cleanup_old_exe(&target);
    assert!(!old.exists());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
}

#[test]
fn replace_by_rename_swaps_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("hashi");
    let new = dir.path().join("downloaded");
    std::fs::write(&target, "old").unwrap();
    std::fs::write(&new, "new").unwrap();
    update::replace_by_rename(&new, &target).unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
    let leftovers = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(".hashi-update"))
        .count();
    assert_eq!(leftovers, 0);
}

#[test]
fn install_refuses_a_cargo_binary_before_any_download() {
    if update::current_target().is_err() {
        return;
    }
    let fake = Fake::start();
    fake.latest("v99.0.0");
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join(".cargo").join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let exe = bin.join("hashi");
    std::fs::write(&exe, "old").unwrap();
    let err = update::install(
        &dir.path().join("home"),
        &fake.sources(),
        &exe,
        None,
        &mut |_| true,
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("cargo install --locked --git"));
    assert_eq!(fake.hits(), 0);
    assert_eq!(std::fs::read_to_string(&exe).unwrap(), "old");
}

// End to end. Unix only: the fake release binary is a shell script.
#[cfg(unix)]
mod end_to_end {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use hashi::update::{self, Outcome};
    use sha2::{Digest, Sha256};

    use super::Fake;

    pub struct Setup {
        pub fake: Fake,
        pub _dir: tempfile::TempDir,
        pub exe: PathBuf,
        pub home: PathBuf,
    }

    /// Serves release `tag` whose binary prints `hashi <reports>`.
    fn setup(tag: &str, reports: &str, corrupt_sum: bool) -> Option<Setup> {
        let target = update::current_target().ok()?;
        let fake = Fake::start();
        fake.latest(tag);
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("pkg");
        let inner = pkg.join(format!("hashi-{target}"));
        std::fs::create_dir_all(&inner).unwrap();
        write_script(
            &inner.join("hashi"),
            &format!("#!/bin/sh\necho \"hashi {reports}\"\n"),
        );
        std::fs::write(inner.join("README.md"), "readme").unwrap();
        let asset = update::asset_name(target);
        let archive = dir.path().join(&asset);
        let status = std::process::Command::new("tar")
            .arg("-C")
            .arg(&pkg)
            .arg("-czf")
            .arg(&archive)
            .arg(format!("hashi-{target}"))
            .status()
            .unwrap();
        assert!(status.success());
        let bytes = std::fs::read(&archive).unwrap();
        let sum = if corrupt_sum {
            "0".repeat(64)
        } else {
            hex::encode(Sha256::digest(&bytes))
        };
        fake.route(&format!("/dl/{tag}/{asset}"), 200, bytes);
        fake.route(
            &format!("/dl/{tag}/{asset}.sha256"),
            200,
            format!("{sum}  {asset}\n"),
        );

        let bindir = dir.path().join("bin");
        std::fs::create_dir_all(&bindir).unwrap();
        let exe = bindir.join("hashi");
        write_script(&exe, "#!/bin/sh\necho old\n");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        Some(Setup {
            fake,
            _dir: dir,
            exe,
            home,
        })
    }

    fn write_script(path: &Path, body: &str) {
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The cache file is `update-check.json`; temp dirs are `update-<uuid>/`.
    fn home_is_clean(home: &Path) -> bool {
        std::fs::read_dir(home)
            .unwrap()
            .filter_map(Result::ok)
            .all(|e| !(e.path().is_dir() && e.file_name().to_string_lossy().starts_with("update-")))
    }

    fn exe_text(s: &Setup) -> String {
        std::fs::read_to_string(&s.exe).unwrap()
    }

    #[test]
    fn install_downloads_verifies_and_replaces() {
        let Some(s) = setup("v99.0.0", "99.0.0", false) else {
            return;
        };
        let mut asked = 0;
        let out = update::install(&s.home, &s.fake.sources(), &s.exe, None, &mut |plan| {
            asked += 1;
            assert_eq!(plan.tag, "v99.0.0");
            true
        })
        .unwrap();
        assert_eq!(out, Outcome::Updated("v99.0.0".into()));
        assert_eq!(asked, 1);
        assert!(exe_text(&s).contains("hashi 99.0.0"));
        let mode = std::fs::metadata(&s.exe).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
        assert!(home_is_clean(&s.home));
    }

    #[test]
    fn install_a_pinned_version() {
        let Some(s) = setup("v0.0.1", "0.0.1", false) else {
            return;
        };
        // The latest is older than this build, so a plain update does nothing...
        let out = update::install(&s.home, &s.fake.sources(), &s.exe, None, &mut |_| true).unwrap();
        assert!(matches!(out, Outcome::UpToDate(_)));
        assert!(exe_text(&s).contains("old"));
        // ...but --version installs it anyway.
        let out = update::install(
            &s.home,
            &s.fake.sources(),
            &s.exe,
            Some("0.0.1"),
            &mut |_| true,
        )
        .unwrap();
        assert_eq!(out, Outcome::Updated("v0.0.1".into()));
        assert!(exe_text(&s).contains("hashi 0.0.1"));
    }

    #[test]
    fn install_refuses_a_checksum_mismatch() {
        let Some(s) = setup("v99.0.0", "99.0.0", true) else {
            return;
        };
        let err =
            update::install(&s.home, &s.fake.sources(), &s.exe, None, &mut |_| true).unwrap_err();
        assert!(format!("{err:#}").contains("checksum mismatch"), "{err:#}");
        assert!(exe_text(&s).contains("old"));
        assert!(home_is_clean(&s.home));
    }

    #[test]
    fn install_refuses_a_binary_reporting_the_wrong_version() {
        let Some(s) = setup("v99.0.0", "98.0.0", false) else {
            return;
        };
        let err =
            update::install(&s.home, &s.fake.sources(), &s.exe, None, &mut |_| true).unwrap_err();
        assert!(format!("{err:#}").contains("expected 99.0.0"), "{err:#}");
        assert!(exe_text(&s).contains("old"));
        assert!(home_is_clean(&s.home));
    }

    #[test]
    fn install_stops_when_declined() {
        let Some(s) = setup("v99.0.0", "99.0.0", false) else {
            return;
        };
        let before = s.fake.hits();
        let out =
            update::install(&s.home, &s.fake.sources(), &s.exe, None, &mut |_| false).unwrap();
        assert_eq!(out, Outcome::Declined);
        assert_eq!(s.fake.hits(), before + 1, "only the release lookup");
        assert!(exe_text(&s).contains("old"));
    }

    #[test]
    fn install_refuses_a_read_only_directory() {
        let Some(s) = setup("v99.0.0", "99.0.0", false) else {
            return;
        };
        let bindir = s.exe.parent().unwrap().to_path_buf();
        std::fs::set_permissions(&bindir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let probe = std::fs::write(bindir.join("probe"), "x").is_ok();
        let result = update::install(&s.home, &s.fake.sources(), &s.exe, None, &mut |_| true);
        std::fs::set_permissions(&bindir, std::fs::Permissions::from_mode(0o755)).unwrap();
        if probe {
            // Running as root: permissions do not apply.
            return;
        }
        let err = result.unwrap_err();
        assert!(format!("{err:#}").contains("cannot write to"), "{err:#}");
        assert_eq!(s.fake.hits(), 0);
    }
}
