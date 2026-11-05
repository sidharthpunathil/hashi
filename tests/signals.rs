//! The real binary under termination signals: a SIGTERM (or SIGHUP) must
//! stop every agent run and leave its request queued exactly once.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Server {
    child: Child,
    lines: mpsc::Receiver<String>,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Starts `hashi serve --headless` with every agent pointed at `claude` or a
/// missing path, so no real agent CLI is ever probed.
fn start(dir: &Path, claude: &Path, port: u16) -> Server {
    let missing = dir.join("missing-agent");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_hashi"));
    cmd.arg("--home")
        .arg(dir.join("home"))
        .args(["serve", "--headless", "--port", &port.to_string()])
        .arg("--claude-bin")
        .arg(claude);
    for flag in [
        "--codex-bin",
        "--gemini-bin",
        "--opencode-bin",
        "--qwen-bin",
        "--cursor-bin",
    ] {
        cmd.arg(flag).arg(&missing);
    }
    cmd.args([
        "--disk-floor",
        "0",
        "--memory-reserve",
        "0",
        "--rate-limit",
        "0",
        "--timeout",
        "60",
    ]);
    for (key, _) in std::env::vars() {
        if key.starts_with("HASHI_") {
            cmd.env_remove(key);
        }
    }
    cmd.env("HOME", dir)
        .env("PATH", "/usr/bin:/bin")
        .env("HASHI_LOG", "hashi=info")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    Server { child, lines }
}

fn admin_token(server: &Server) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let Ok(line) = server.lines.recv_timeout(left) else {
            break;
        };
        if let Some(t) = line.split_whitespace().find(|w| w.starts_with("hashi_")) {
            return t.to_owned();
        }
    }
    panic!("the server never printed its admin token");
}

fn wait_exit(child: &mut Child, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn read_pid(pidfile: &Path) -> String {
    for _ in 0..200 {
        if let Ok(p) = std::fs::read_to_string(pidfile)
            && !p.trim().is_empty()
        {
            return p.trim().to_owned();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the agent never started");
}

fn stop_with(signal: &str) {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("agent.pid");
    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"--version\" ]; then echo '9.9.9 (Fake Claude)'; exit 0; fi\n\
         echo $$ > '{}'\n\
         sleep 120\n",
        pidfile.display()
    );
    let claude = write_script(dir.path(), "claude", &script);
    let port = free_port();
    let mut server = start(dir.path(), &claude, port);
    let admin = admin_token(&server);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let id = rt.block_on(async {
        let res = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/v1/ask"))
            .bearer_auth(&admin)
            .json(&json!({"message": "long job"}))
            .send()
            .await
            .unwrap();
        let v: Value = res.json().await.unwrap();
        v["id"].as_str().unwrap().to_owned()
    });
    let agent = read_pid(&pidfile);
    assert!(alive(&agent));

    let status = Command::new("kill")
        .args([&format!("-{signal}"), &server.child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let exited = wait_exit(&mut server.child, Duration::from_secs(15));
    let mut gone = false;
    for _ in 0..40 {
        if !alive(&agent) {
            gone = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if !gone {
        let _ = Command::new("kill").args(["-9", &agent]).status();
    }
    assert!(exited, "hashi did not exit on SIG{signal}");
    assert!(gone, "agent {agent} outlived hashi after SIG{signal}");

    let db = rusqlite::Connection::open(dir.path().join("home/hashi.db")).unwrap();
    let (status, attempts): (String, i64) = db
        .query_row(
            "SELECT status, attempts FROM requests WHERE id = ?1",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (status.as_str(), attempts),
        ("queued", 0),
        "a clean stop puts the request back once, without counting a crash"
    );
}

#[test]
fn sigterm_stops_agent_runs_and_requeues_them() {
    stop_with("TERM");
}

#[test]
fn sighup_stops_agent_runs_and_requeues_them() {
    stop_with("HUP");
}
