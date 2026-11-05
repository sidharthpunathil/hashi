//! Connection-level protection: a cap on open connections, a deadline for
//! each request head, the client address seen by handlers, and the
//! open-file limit.
//!
//! `axum::serve` builds its hyper connections without a timer, which turns
//! hyper's header read timeout off, so a client could hold a socket forever
//! by dribbling header bytes. [`serve`] drives hyper directly instead, with a
//! Tokio timer and [`NetLimits::header_read_timeout`]. The timer also runs
//! while a keep-alive connection sits idle between requests, so idle sockets
//! close after the same deadline. Both the plain and the TLS listener hold a
//! [`Semaphore`] permit inside each connection's IO, so a slot frees exactly
//! when the socket is dropped.

use std::io;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request};
use axum::serve::Listener;
use hyper::body::Incoming;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::server::graceful::GracefulShutdown;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
use tower_service::Service;

/// Connections served at once unless `--max-connections` says otherwise.
pub const DEFAULT_MAX_CONNECTIONS: usize = 512;
/// How long a client gets to send a complete request head.
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Highest open-file soft limit Hashi raises itself to.
pub const FD_LIMIT_TARGET: u64 = 8192;
/// Header cloudflared sets to the visitor's address.
pub const CF_CONNECTING_IP: &str = "cf-connecting-ip";

/// Connection limits for [`serve`].
#[derive(Debug, Clone)]
pub struct NetLimits {
    /// Connections open at once. Further clients wait in the kernel backlog.
    pub max_connections: usize,
    /// Deadline for a request head, and for an idle keep-alive connection.
    pub header_read_timeout: Duration,
    /// Trust `CF-Connecting-IP` from loopback peers. The server turns this on
    /// when a tunnel is configured; tests can turn it on without one.
    pub trust_forwarded: bool,
}

impl Default for NetLimits {
    fn default() -> Self {
        Self {
            max_connections: DEFAULT_MAX_CONNECTIONS,
            header_read_timeout: HEADER_READ_TIMEOUT,
            trust_forwarded: false,
        }
    }
}

/// The connection a request arrived on, as a request extension.
#[derive(Debug, Clone, Copy)]
pub struct Peer {
    /// Socket peer address.
    pub addr: SocketAddr,
    /// Whether `CF-Connecting-IP` may be believed when the peer is loopback.
    pub trust_forwarded: bool,
}

/// Rate-limit key for the client behind a request.
///
/// The socket peer address, unless the peer is loopback, a tunnel is active
/// (`trust_forwarded`) and cloudflared supplied `CF-Connecting-IP`. The header
/// is ignored from any other peer, since anyone can send it. IPv6 addresses
/// are grouped by their /64, which one subscriber usually controls whole.
#[must_use]
pub fn client_key(peer: SocketAddr, headers: &HeaderMap, trust_forwarded: bool) -> String {
    let peer_ip = peer.ip().to_canonical();
    let ip = if trust_forwarded && peer_ip.is_loopback() {
        headers
            .get(CF_CONNECTING_IP)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<IpAddr>().ok())
            .map_or(peer_ip, |ip| ip.to_canonical())
    } else {
        peer_ip
    };
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => {
            let s = v6.segments();
            format!("{}/64", Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
        }
    }
}

/// Rate-limit key for a request, from the [`Peer`] extension that [`serve`]
/// attaches. Requests that did not come through [`serve`] share one key.
#[must_use]
pub fn request_client_key(extensions: &axum::http::Extensions, headers: &HeaderMap) -> String {
    match extensions.get::<Peer>() {
        Some(p) => client_key(p.addr, headers, p.trust_forwarded),
        None => "unknown".to_owned(),
    }
}

/// IO that holds a connection slot until it is dropped.
#[derive(Debug)]
pub struct Limited<T> {
    io: T,
    _permit: OwnedSemaphorePermit,
}

impl<T> Limited<T> {
    /// Wraps `io`, keeping `permit` for its lifetime.
    pub fn new(io: T, permit: OwnedSemaphorePermit) -> Self {
        Self {
            io,
            _permit: permit,
        }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for Limited<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for Limited<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().io).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().io).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.io.is_write_vectored()
    }
}

/// A plain TCP listener that stops accepting while `max` connections are open.
#[derive(Debug)]
pub struct LimitedListener {
    tcp: TcpListener,
    slots: Arc<Semaphore>,
}

impl LimitedListener {
    /// Wraps a bound listener. A `max` of zero is treated as one.
    #[must_use]
    pub fn new(tcp: TcpListener, max: usize) -> Self {
        Self {
            tcp,
            slots: Arc::new(Semaphore::new(max.max(1))),
        }
    }
}

impl Listener for LimitedListener {
    type Io = Limited<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            // Waiting for a slot before accepting leaves extra clients in the
            // kernel backlog, where they cost Hashi no file descriptor.
            let Ok(permit) = Arc::clone(&self.slots).acquire_owned().await else {
                // The semaphore is never closed.
                return std::future::pending().await;
            };
            match self.tcp.accept().await {
                Ok((stream, peer)) => return (Limited::new(stream, permit), peer),
                Err(e) => {
                    // Usually EMFILE: back off instead of spinning.
                    tracing::debug!(error = %e, "accept failed");
                    drop(permit);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.tcp.local_addr()
    }
}

/// Serves `app` on `listener` until `shutdown` fires, then waits for open
/// connections to finish their current request.
///
/// Each request carries [`ConnectInfo`] and [`Peer`] extensions with the
/// socket peer address.
pub async fn serve<L>(mut listener: L, app: Router, limits: &NetLimits, shutdown: CancellationToken)
where
    L: Listener<Addr = SocketAddr>,
{
    let graceful = GracefulShutdown::new();
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(limits.header_read_timeout);
    let trust_forwarded = limits.trust_forwarded;
    loop {
        let (io, addr) = tokio::select! {
            () = shutdown.cancelled() => break,
            conn = listener.accept() => conn,
        };
        let app = app.clone();
        let peer = Peer {
            addr,
            trust_forwarded,
        };
        let svc = hyper::service::service_fn(move |mut req: Request<Incoming>| {
            req.extensions_mut().insert(ConnectInfo(addr));
            req.extensions_mut().insert(peer);
            let mut app = app.clone();
            async move { app.call(req).await }
        });
        let conn = graceful.watch(builder.serve_connection(TokioIo::new(io), svc));
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                tracing::trace!(peer = %addr, error = %e, "connection ended with an error");
            }
        });
    }
    // Stop accepting before waiting, so new clients are refused promptly.
    drop(listener);
    graceful.shutdown().await;
}

/// Raises the open-file soft limit toward the hard limit, up to
/// [`FD_LIMIT_TARGET`]. Rust does not do this itself, and macOS starts GUI
/// and launchd processes at 256. Returns the soft limit in effect afterwards.
#[cfg(unix)]
pub fn raise_fd_limit() -> Option<u64> {
    use nix::sys::resource::{Resource, getrlimit, setrlimit};
    let (soft, hard) = match getrlimit(Resource::RLIMIT_NOFILE) {
        Ok(pair) => pair,
        Err(e) => {
            tracing::debug!(error = %e, "cannot read the open-file limit");
            return None;
        }
    };
    let target = FD_LIMIT_TARGET.min(hard);
    if soft >= target {
        tracing::debug!(soft, hard, "open-file limit already high enough");
        return Some(soft);
    }
    match setrlimit(Resource::RLIMIT_NOFILE, target, hard) {
        Ok(()) => {
            tracing::debug!(
                before = soft,
                after = target,
                hard,
                "raised open-file limit"
            );
            Some(target)
        }
        Err(e) => {
            tracing::debug!(soft, hard, error = %e, "cannot raise open-file limit");
            Some(soft)
        }
    }
}

/// Windows has no per-process descriptor limit to raise.
#[cfg(not(unix))]
pub fn raise_fd_limit() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(cf: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(v) = cf {
            h.insert(CF_CONNECTING_IP, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    fn sock(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn uses_the_peer_address_by_default() {
        let key = client_key(sock("192.0.2.7:5000"), &headers(None), false);
        assert_eq!(key, "192.0.2.7");
    }

    #[test]
    fn trusts_the_tunnel_header_from_loopback() {
        let h = headers(Some("203.0.113.9"));
        assert_eq!(client_key(sock("127.0.0.1:5000"), &h, true), "203.0.113.9");
        assert_eq!(client_key(sock("[::1]:5000"), &h, true), "203.0.113.9");
    }

    #[test]
    fn ignores_the_header_without_a_tunnel() {
        let h = headers(Some("203.0.113.9"));
        assert_eq!(client_key(sock("127.0.0.1:5000"), &h, false), "127.0.0.1");
    }

    #[test]
    fn ignores_a_spoofed_header_from_a_remote_peer() {
        let h = headers(Some("203.0.113.9"));
        assert_eq!(client_key(sock("192.0.2.7:5000"), &h, true), "192.0.2.7");
        assert_eq!(
            client_key(sock("[2001:db8:1:2:3:4:5:6]:5000"), &h, true),
            "2001:db8:1:2::/64"
        );
    }

    #[test]
    fn falls_back_to_the_peer_on_a_garbage_header() {
        let h = headers(Some("not an address"));
        assert_eq!(client_key(sock("127.0.0.1:5000"), &h, true), "127.0.0.1");
    }

    #[test]
    fn groups_ipv6_by_prefix_and_unmaps_ipv4() {
        let a = client_key(sock("[2001:db8:1:2::1]:1"), &headers(None), false);
        let b = client_key(sock("[2001:db8:1:2:ffff::9]:1"), &headers(None), false);
        assert_eq!(a, b);
        let mapped = client_key(sock("[::ffff:192.0.2.7]:1"), &headers(None), false);
        assert_eq!(mapped, "192.0.2.7");
    }

    #[test]
    fn requests_without_a_peer_share_a_key() {
        let ext = axum::http::Extensions::new();
        assert_eq!(request_client_key(&ext, &headers(None)), "unknown");
    }

    #[cfg(unix)]
    #[test]
    fn raising_the_fd_limit_never_lowers_it() {
        use nix::sys::resource::{Resource, getrlimit};
        let (before, _) = getrlimit(Resource::RLIMIT_NOFILE).unwrap();
        let after = raise_fd_limit().unwrap();
        assert!(after >= before.min(FD_LIMIT_TARGET));
    }
}
