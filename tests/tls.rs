//! Native HTTPS end to end: a real server with a throwaway certificate.

#![cfg(unix)]
// Test helpers outside #[test] functions are not covered by clippy.toml's
// test exemptions; panicking on setup failure is the desired behaviour here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use hashi::agent::{AgentConfig, AgentKind, Limits};
use hashi::events::Bus;
use hashi::server::{self, Running, ServerConfig};
use hashi::tls::TlsFiles;
use rustls_pki_types::{CertificateDer, ServerName};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

struct Tls {
    _dir: tempfile::TempDir,
    running: Running,
    port: u16,
    connector: TlsConnector,
}

async fn start() -> Tls {
    let dir = tempfile::tempdir().unwrap();
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_path = dir.path().join("cert.pem");
    let key_path = dir.path().join("key.pem");
    std::fs::write(&cert_path, cert.cert.pem()).unwrap();
    std::fs::write(&key_path, cert.signing_key.serialize_pem()).unwrap();

    let mut agents = HashMap::new();
    for kind in AgentKind::ALL {
        let mut cfg = AgentConfig::default_for(kind);
        cfg.bin = "/nonexistent".into();
        agents.insert(kind, cfg);
    }
    let cfg = ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        home: dir.path().join("home"),
        agents,
        default_agent: AgentKind::Claude,
        concurrency: 1,
        limits: Limits::default(),
        rate_limit: 0,
        queue_limit: 10,
        retention_days: 14,
        tunnel: false,
        public_url: None,
        name: "TLS test".into(),
        tls: Some(TlsFiles {
            cert: cert_path,
            key: key_path,
        }),
        tunnel_token: None,
    };
    let running = server::start(cfg, Bus::default()).await.unwrap();
    let url = running.state.runtime.local_url.clone();
    assert!(url.starts_with("https://"), "{url}");
    let port = url.rsplit(':').next().unwrap().parse().unwrap();

    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(cert.cert.der().to_vec()))
        .unwrap();
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let client = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Tls {
        _dir: dir,
        running,
        port,
        connector: TlsConnector::from(Arc::new(client)),
    }
}

async fn https_get(t: &Tls, path: &str) -> String {
    let tcp = TcpStream::connect(("127.0.0.1", t.port)).await.unwrap();
    let name = ServerName::try_from("localhost").unwrap();
    let mut tls = t.connector.connect(name, tcp).await.unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    tls.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    tls.read_to_string(&mut out).await.ok();
    out
}

#[tokio::test]
async fn serves_https_with_hsts() {
    let t = start().await;
    let res = https_get(&t, "/healthz").await;
    assert!(res.starts_with("HTTP/1.1 200"), "{res}");
    assert!(
        res.to_ascii_lowercase()
            .contains("strict-transport-security: max-age=31536000")
    );
    assert!(res.ends_with("ok"));
    t.running.shutdown().await;
}

#[tokio::test]
async fn plain_http_gets_no_answer() {
    let t = start().await;
    let mut tcp = TcpStream::connect(("127.0.0.1", t.port)).await.unwrap();
    tcp.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(3), tcp.read_to_end(&mut buf)).await;
    assert!(!String::from_utf8_lossy(&buf).contains("HTTP/1.1 200"));
    t.running.shutdown().await;
}

#[tokio::test]
async fn a_silent_client_does_not_block_others() {
    let t = start().await;
    // Opens a connection and never starts the handshake.
    let _idle = TcpStream::connect(("127.0.0.1", t.port)).await.unwrap();
    let res = tokio::time::timeout(Duration::from_secs(5), https_get(&t, "/healthz"))
        .await
        .expect("an idle connection must not stall the accept loop");
    assert!(res.starts_with("HTTP/1.1 200"), "{res}");
    t.running.shutdown().await;
}
