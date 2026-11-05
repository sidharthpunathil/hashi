//! Native HTTPS, for running without a tunnel or reverse proxy.
//!
//! Point `--tls-cert` and `--tls-key` at PEM files from Let's Encrypt,
//! `mkcert`, `tailscale cert` or your own CA. Handshakes run on their own
//! tasks with a timeout, so a slow or silent client cannot hold up others.
//! The same connection cap as plain HTTP applies: a slot is taken before
//! `accept` and held by the connection's IO until it closes.

use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, mpsc};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::server::TlsStream;

use crate::net::Limited;

/// How long a client gets to finish the TLS handshake.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Handshakes allowed in flight at once.
pub const MAX_HANDSHAKES: usize = 256;

/// Paths to a PEM certificate chain and its private key.
#[derive(Debug, Clone)]
pub struct TlsFiles {
    /// Certificate chain, leaf first.
    pub cert: PathBuf,
    /// Private key (PKCS#8, PKCS#1 or SEC1).
    pub key: PathBuf,
}

/// Problems loading the certificate or key.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    /// The certificate file could not be read or parsed.
    #[error("cannot read certificate {path}: {reason}")]
    Cert {
        /// File that failed.
        path: String,
        /// Why.
        reason: String,
    },
    /// The key file could not be read or parsed.
    #[error("cannot read private key {path}: {reason}")]
    Key {
        /// File that failed.
        path: String,
        /// Why.
        reason: String,
    },
    /// rustls rejected the pair, for example because they do not match.
    #[error("certificate and key do not work together: {0}")]
    Config(String),
}

fn show(p: &Path) -> String {
    p.display().to_string()
}

/// Loads a server configuration from PEM files.
///
/// # Errors
///
/// Returns a [`TlsError`] naming the file and the problem.
pub fn load(files: &TlsFiles) -> Result<Arc<ServerConfig>, TlsError> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(&files.cert)
        .map_err(|e| TlsError::Cert {
            path: show(&files.cert),
            reason: e.to_string(),
        })?
        .collect::<Result<_, _>>()
        .map_err(|e| TlsError::Cert {
            path: show(&files.cert),
            reason: e.to_string(),
        })?;
    if certs.is_empty() {
        return Err(TlsError::Cert {
            path: show(&files.cert),
            reason: "no PEM certificates found".to_owned(),
        });
    }
    let key = PrivateKeyDer::from_pem_file(&files.key).map_err(|e| TlsError::Key {
        path: show(&files.key),
        reason: e.to_string(),
    })?;

    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| TlsError::Config(e.to_string()))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| TlsError::Config(e.to_string()))?;
    // The server speaks HTTP/1.1 only.
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// A listener that yields connections only after their TLS handshake succeeds.
#[derive(Debug)]
pub struct TlsListener {
    rx: mpsc::Receiver<(Limited<TlsStream<TcpStream>>, SocketAddr)>,
    local: SocketAddr,
}

impl TlsListener {
    /// Wraps a bound TCP listener, serving at most `max_connections` at once
    /// (zero is treated as one).
    ///
    /// # Errors
    ///
    /// Fails if the listener has no local address.
    pub fn new(
        tcp: TcpListener,
        config: Arc<ServerConfig>,
        max_connections: usize,
    ) -> io::Result<Self> {
        let local = tcp.local_addr()?;
        let (tx, rx) = mpsc::channel(64);
        let acceptor = TlsAcceptor::from(config);
        let permits = Arc::new(Semaphore::new(MAX_HANDSHAKES));
        let slots = Arc::new(Semaphore::new(max_connections.max(1)));
        tokio::spawn(async move {
            loop {
                // Take a connection slot first, so clients past the cap wait in
                // the kernel backlog instead of holding a descriptor here.
                let slot = tokio::select! {
                    () = tx.closed() => return,
                    s = Arc::clone(&slots).acquire_owned() => s,
                };
                let Ok(slot) = slot else { return };
                let accepted = tokio::select! {
                    () = tx.closed() => return,
                    a = tcp.accept() => a,
                };
                let (stream, peer) = match accepted {
                    Ok(pair) => pair,
                    Err(e) => {
                        tracing::debug!(error = %e, "accept failed");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                };
                let Ok(permit) = Arc::clone(&permits).acquire_owned().await else {
                    return;
                };
                let acceptor = acceptor.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                        Ok(Ok(tls)) => {
                            let _ = tx.send((Limited::new(tls, slot), peer)).await;
                        }
                        Ok(Err(e)) => tracing::debug!(%peer, error = %e, "TLS handshake failed"),
                        Err(_) => tracing::debug!(%peer, "TLS handshake timed out"),
                    }
                });
            }
        });
        Ok(Self { rx, local })
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = Limited<TlsStream<TcpStream>>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.rx.recv().await {
            Some(conn) => conn,
            // The accept task only stops when this listener is dropped.
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(self.local)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn load_accepts_a_matching_pair() {
        let dir = tempfile::tempdir().unwrap();
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let files = TlsFiles {
            cert: write(dir.path(), "c.pem", &cert.cert.pem()),
            key: write(dir.path(), "k.pem", &cert.signing_key.serialize_pem()),
        };
        assert!(load(&files).is_ok());
    }

    #[test]
    fn load_names_the_missing_file() {
        let files = TlsFiles {
            cert: "/nope/cert.pem".into(),
            key: "/nope/key.pem".into(),
        };
        let err = load(&files).unwrap_err().to_string();
        assert!(err.contains("/nope/cert.pem"), "{err}");
    }

    #[test]
    fn load_rejects_a_mismatched_key() {
        let dir = tempfile::tempdir().unwrap();
        let a = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let b = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let files = TlsFiles {
            cert: write(dir.path(), "c.pem", &a.cert.pem()),
            key: write(dir.path(), "k.pem", &b.signing_key.serialize_pem()),
        };
        assert!(matches!(load(&files), Err(TlsError::Config(_))));
    }

    #[test]
    fn load_rejects_a_file_without_certificates() {
        let dir = tempfile::tempdir().unwrap();
        let files = TlsFiles {
            cert: write(dir.path(), "c.pem", "not a certificate"),
            key: write(dir.path(), "k.pem", "not a key"),
        };
        assert!(matches!(load(&files), Err(TlsError::Cert { .. })));
    }
}
