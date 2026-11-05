//! Going public through Tailscale and stopping the Cloudflare tunnel, against
//! a real server with fake `tailscale` and `cloudflared` executables. The
//! fakes log their arguments and print canned JSON; nothing here runs the
//! real tools or touches the network.
#![cfg(unix)]
// Test helpers outside #[test] functions are not covered by clippy.toml's
// test exemptions; panicking on setup failure is the desired behaviour here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hashi::agent::{AgentConfig, AgentKind, Limits};
use hashi::events::{Bus, Event};
use hashi::net::NetLimits;
use hashi::server::{self, PublicRoute, Running, ServerConfig};
use hashi::tailscale::Mode;
use hashi::tunnel::StartError;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::broadcast::Receiver;

const HOST: &str = "laptop.tail1234.ts.net";
const SIGNED_IN: &str = r#"{"Version":"1.90.1","BackendState":"Running",
  "Self":{"DNSName":"laptop.tail1234.ts.net.","HostName":"laptop","Online":true}}"#;

/// A fake `tailscale`: appends its argv to `argv.log` and answers from the
/// files next to it. `--bg` writes the listener it was asked for to `on`
/// (the live Serve config) unless `noapply` exists, and prints `approval` if it exists;
/// `off` removes `on`.
const FAKE_TAILSCALE: &str = r#"#!/bin/sh
d=$(dirname "$0")
echo "$*" >> "$d/argv.log"
case "$*" in
  "status --json") cat "$d/status.json" ;;
  "serve status --json")
    if [ -f "$d/on" ]; then cat "$d/on"; else cat "$d/serve.json"; fi ;;
  *" off") rm -f "$d/on" ;;
  *"--bg"*)
    if [ -f "$d/approval" ]; then cat "$d/approval"; fi
    if [ -f "$d/fail" ]; then echo "boom" >&2; exit 1; fi
    port=${3#--https=}
    if [ ! -f "$d/noapply" ]; then
      printf '{"TCP":{"%s":{"HTTPS":true}},"Web":{"laptop.tail1234.ts.net:%s":{"Handlers":{"/":{"Proxy":"%s"}}}}}' \
        "$port" "$port" "$4" > "$d/on"
    fi ;;
  *) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#;

/// A fake `cloudflared` that reports a quick-tunnel URL and stays up.
const FAKE_CLOUDFLARED: &str = r#"#!/bin/sh
echo "$*" >> "$(dirname "$0")/cloudflared.log"
echo "INF |  https://calm-river-bridge.trycloudflare.com  |" >&2
exec sleep 600
"#;

struct World {
    dir: tempfile::TempDir,
    running: Running,
    port: u16,
    events: Receiver<Event>,
}

fn read_argv(bin: &Path) -> Vec<String> {
    std::fs::read_to_string(bin.join("argv.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn write_exe(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

impl World {
    fn bin_dir(&self) -> PathBuf {
        self.dir.path().join("bin")
    }

    fn put(&self, name: &str, body: &str) {
        std::fs::write(self.bin_dir().join(name), body).unwrap();
    }

    /// Shuts the server down and returns every argv the fake saw.
    async fn shutdown(self) -> Vec<String> {
        let World { dir, running, .. } = self;
        running.shutdown().await;
        read_argv(&dir.path().join("bin"))
    }

    fn argv(&self) -> Vec<String> {
        read_argv(&self.bin_dir())
    }

    fn ours(&self) -> String {
        format!(
            r#"{{"TCP":{{"443":{{"HTTPS":true}}}},
              "Web":{{"{HOST}:443":{{"Handlers":{{"/":{{"Proxy":"http://127.0.0.1:{}"}}}}}}}}}}"#,
            self.port
        )
    }

    /// The next tunnel or approval event, skipping log lines.
    async fn next(&mut self) -> Event {
        let wait = async {
            loop {
                match self.events.recv().await.unwrap() {
                    Event::Log(_) => {}
                    ev => return ev,
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(20), wait)
            .await
            .expect("no event in time")
    }

    /// Sends `GET /healthz` with this `Host` header and returns the status line.
    async fn get_with_host(&self, host: &str) -> String {
        let mut tcp = TcpStream::connect(("127.0.0.1", self.port)).await.unwrap();
        let req = format!("GET /healthz HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
        tcp.write_all(req.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        tcp.read_to_end(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned()
    }
}

async fn start() -> World {
    start_with(|_| {}).await.unwrap()
}

async fn start_with(tweak: impl FnOnce(&mut ServerConfig)) -> anyhow::Result<World> {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    write_exe(&bin.join("tailscale"), FAKE_TAILSCALE);
    write_exe(&bin.join("cloudflared"), FAKE_CLOUDFLARED);
    std::fs::write(bin.join("status.json"), SIGNED_IN).unwrap();
    std::fs::write(bin.join("serve.json"), "{}").unwrap();
    let mut agents = HashMap::new();
    for kind in AgentKind::ALL {
        let mut cfg = AgentConfig::default_for(kind);
        cfg.bin = "/nonexistent".into();
        agents.insert(kind, cfg);
    }
    let mut cfg = ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        home: dir.path().join("home"),
        agents,
        default_agent: AgentKind::Claude,
        concurrency: 1,
        resources: hashi::guard::ResourceSettings::with_probe(std::sync::Arc::new(
            hashi::guard::ManualProbe::roomy(),
        )),
        limits: Limits::default(),
        rate_limit: 0,
        queue_limit: 10,
        retention_days: 14,
        tunnel: false,
        public_url: None,
        name: "Tailscale test".into(),
        tls: None,
        tunnel_token: None,
        update_check: false,
        tailscale: None,
        helpers: server::Helpers {
            tailscale_port: 443,
            tailscale_bin: Some(bin.join("tailscale")),
            cloudflared_bin: Some(bin.join("cloudflared")),
        },
    };
    tweak(&mut cfg);
    let bus = Bus::default();
    let events = bus.subscribe();
    let running = server::start_with(cfg, NetLimits::default(), bus).await?;
    let port = running
        .state
        .runtime
        .local_url
        .rsplit(':')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let w = World {
        dir,
        running,
        port,
        events,
    };
    Ok(w)
}

fn go(w: &World, mode: Mode) {
    let st = &w.running.state;
    server::go_tailscale(&st.runtime, &st.bus, mode).unwrap();
}

#[tokio::test]
async fn serve_turns_on_then_off_exactly_its_own_listener() {
    let mut w = start().await;
    let rt = std::sync::Arc::clone(&w.running.state.runtime);
    assert_eq!(
        w.get_with_host(HOST).await,
        "HTTP/1.1 421 Misdirected Request"
    );
    go(&w, Mode::Serve);
    let url = format!("https://{HOST}");
    assert_eq!(w.next().await, Event::TunnelReady(url.clone()));
    assert_eq!(rt.base_url(), url);
    assert_eq!(
        server::read_public_url(&w.dir.path().join("home")).as_deref(),
        Some(url.as_str())
    );
    // The device name is accepted, and X-Forwarded-For trusted, only now.
    assert_eq!(w.get_with_host(HOST).await, "HTTP/1.1 200 OK");
    assert!(rt.tailscale.trust().get());
    assert_eq!(
        server::active_route(&rt),
        Some(PublicRoute::Tailscale(Mode::Serve))
    );

    assert_eq!(
        server::stop_public(&rt),
        Some(PublicRoute::Tailscale(Mode::Serve))
    );
    assert!(matches!(w.next().await, Event::TunnelDown(m) if m.contains("turned off")));
    let target = format!("http://127.0.0.1:{}", w.port);
    assert_eq!(
        w.argv(),
        [
            "status --json".to_owned(),
            "serve status --json".to_owned(),
            format!("serve --bg --https=443 {target}"),
            "serve status --json".to_owned(),
            "serve --https=443 off".to_owned(),
        ]
    );
    assert!(!w.argv().iter().any(|a| a.contains("reset")));
    assert_eq!(rt.base_url(), rt.local_url);
    assert_eq!(server::read_public_url(&w.dir.path().join("home")), None);
    assert!(!rt.tailscale.trust().get());
    assert_eq!(
        w.get_with_host(HOST).await,
        "HTTP/1.1 421 Misdirected Request"
    );
    w.running.shutdown().await;
}

#[tokio::test]
async fn shutdown_runs_only_the_matching_off_command() {
    let mut w = start().await;
    go(&w, Mode::Funnel);
    assert!(matches!(w.next().await, Event::TunnelReady(_)));
    let argv = w.shutdown().await;
    let offs: Vec<&String> = argv.iter().filter(|a| a.ends_with(" off")).collect();
    assert_eq!(offs, ["funnel --https=443 off"]);
    assert_eq!(argv.last().unwrap(), "funnel --https=443 off");
}

#[tokio::test]
async fn a_port_serving_something_else_is_left_alone() {
    let mut w = start().await;
    w.put(
        "serve.json",
        &format!(
            r#"{{"TCP":{{"443":{{"HTTPS":true}}}},
              "Web":{{"{HOST}:443":{{"Handlers":{{"/":{{"Proxy":"http://127.0.0.1:3000"}}}}}}}}}}"#
        ),
    );
    go(&w, Mode::Serve);
    let Event::TunnelDown(why) = w.next().await else {
        panic!("expected a refusal")
    };
    assert!(why.contains("Hashi did not set up"), "{why}");
    assert!(why.contains("--tailscale-port"), "{why}");
    assert_eq!(w.argv(), ["status --json", "serve status --json"]);
    assert!(!w.running.state.runtime.tailscale.trust().get());
    // Nothing was turned on, so nothing is turned off.
    assert!(!w.shutdown().await.iter().any(|a| a.ends_with(" off")));
}

#[tokio::test]
async fn a_signed_out_device_says_how_to_sign_in() {
    let mut w = start().await;
    w.put(
        "status.json",
        r#"{"BackendState":"NeedsLogin","Self":null}"#,
    );
    go(&w, Mode::Serve);
    let Event::TunnelDown(why) = w.next().await else {
        panic!("expected a refusal")
    };
    assert!(why.contains("not connected (NeedsLogin)"), "{why}");
    assert!(why.contains("tailscale up"), "{why}");
    assert_eq!(w.argv(), ["status --json"]);
    w.running.shutdown().await;
}

#[tokio::test]
async fn the_approval_url_reaches_the_owner_as_printed() {
    let mut w = start().await;
    let approval = "https://login.tailscale.com/f/funnel?node=nAbC123";
    w.put(
        "approval",
        &format!("\nFunnel is not enabled on your tailnet.\n\n         {approval}\n\n"),
    );
    // Like the real CLI when it does not wait: prints the link, exits 0,
    // and sets nothing up.
    w.put("noapply", "");
    go(&w, Mode::Funnel);
    assert_eq!(w.next().await, Event::ApprovalNeeded(approval.to_owned()));
    let Event::TunnelDown(why) = w.next().await else {
        panic!("expected a failure")
    };
    assert!(why.contains(&format!("approve it at {approval}")), "{why}");
    w.running.shutdown().await;
}

#[tokio::test]
async fn a_failed_command_reports_what_tailscale_said() {
    let mut w = start().await;
    w.put("fail", "");
    go(&w, Mode::Serve);
    let Event::TunnelDown(why) = w.next().await else {
        panic!("expected a failure")
    };
    assert!(why.contains("tailscale serve failed: boom"), "{why}");
    w.running.shutdown().await;
}

#[tokio::test]
async fn startup_refuses_what_tailscale_cannot_do() {
    let err = start_with(|c| {
        c.tailscale = Some(Mode::Funnel);
        c.helpers.tailscale_port = 7443;
    })
    .await
    .err()
    .unwrap();
    assert!(err.to_string().contains("443, 8443 and 10000"), "{err}");
    let err = start_with(|c| {
        c.tailscale = Some(Mode::Serve);
        c.tunnel = true;
    })
    .await
    .err()
    .unwrap();
    assert!(err.to_string().contains("not both"), "{err}");
}

#[tokio::test]
async fn startup_flag_brings_the_route_up_with_the_server() {
    let mut w = start_with(|c| c.tailscale = Some(Mode::Serve))
        .await
        .unwrap();
    assert_eq!(
        w.next().await,
        Event::TunnelReady(format!("https://{HOST}"))
    );
    assert_eq!(w.shutdown().await.last().unwrap(), "serve --https=443 off");
}

#[tokio::test]
async fn only_one_route_is_public_at_a_time_and_cloudflare_can_stop() {
    let mut w = start().await;
    let st = w.running.state.clone();
    server::go_public(&st.runtime, &st.bus).unwrap();
    let cf = "https://calm-river-bridge.trycloudflare.com".to_owned();
    assert_eq!(w.next().await, Event::TunnelReady(cf.clone()));
    assert_eq!(st.runtime.base_url(), cf);
    assert!(matches!(
        server::go_tailscale(&st.runtime, &st.bus, Mode::Serve),
        Err(StartError::OtherRoute(_))
    ));

    // Switching stops cloudflared, then brings Tailscale up.
    server::switch_public(&st.runtime, &st.bus, PublicRoute::Tailscale(Mode::Serve)).unwrap();
    assert!(matches!(w.next().await, Event::TunnelDown(m) if m.contains("stopped")));
    assert!(!st.runtime.tunnel.trust().get());
    assert_eq!(
        w.next().await,
        Event::TunnelReady(format!("https://{HOST}"))
    );
    assert!(matches!(
        server::go_public(&st.runtime, &st.bus),
        Err(StartError::OtherRoute(_))
    ));
    assert_eq!(
        server::stop_public(&st.runtime),
        Some(PublicRoute::Tailscale(Mode::Serve))
    );
    assert!(matches!(w.next().await, Event::TunnelDown(_)));
    assert_eq!(server::active_route(&st.runtime), None);
    assert_eq!(st.runtime.base_url(), st.runtime.local_url);
    w.running.shutdown().await;
}

#[tokio::test]
async fn a_listener_left_by_an_earlier_run_is_reused_and_turned_off() {
    let mut w = start().await;
    w.put("serve.json", &w.ours());
    w.put("noapply", "");
    w.put("on", &w.ours());
    go(&w, Mode::Serve);
    assert!(matches!(w.next().await, Event::TunnelReady(_)));
    assert_eq!(w.shutdown().await.last().unwrap(), "serve --https=443 off");
}
