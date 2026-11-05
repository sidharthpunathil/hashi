//! Connection-level protection: a cap on open connections and a share of it
//! per client, deadlines for each request head and body, a bounded graceful
//! shutdown, the client address seen by handlers, and the open-file limit.
//!
//! `axum::serve` builds its hyper connections without a timer, which turns
//! hyper's header read timeout off, so a client could hold a socket forever
//! by dribbling header bytes. [`serve`] drives hyper directly instead, with a
//! Tokio timer and [`NetLimits::header_read_timeout`]. The timer also runs
//! while a keep-alive connection sits idle between requests, so idle sockets
//! close after the same deadline. Request bodies get their own total
//! deadline, [`NetLimits::body_read_timeout`], so a client that sends a head
//! and then stalls (or dribbles) the body gives its slot back. Long polls are
//! unaffected: they wait on the response, not the body. Both the plain and
//! the TLS listener hold a [`Semaphore`] permit inside each connection's IO,
//! so a slot frees exactly when the socket is dropped, and no remote client
//! address may hold more than [`per_client_share`] of the slots.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request};
use axum::serve::Listener;
use hyper::body::{Body, Frame, Incoming, SizeHint};
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
/// How long a client gets to send a complete request body, once the handler
/// starts reading it.
pub const BODY_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// How long shutdown waits for open requests before dropping them.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);
/// Fewest connections one client address may hold, whatever the cap.
pub const MIN_PER_CLIENT_CONNECTIONS: usize = 4;
/// Highest open-file soft limit Hashi raises itself to.
pub const FD_LIMIT_TARGET: u64 = 8192;
/// Header cloudflared sets to the visitor's address.
pub const CF_CONNECTING_IP: &str = "cf-connecting-ip";
/// Header a reverse proxy appends the client's address to.
pub const X_FORWARDED_FOR: &str = "x-forwarded-for";

/// Connection and HTTP edge settings for [`serve`] and the router.
#[derive(Debug, Clone)]
pub struct NetLimits {
    /// Connections open at once. Further clients wait in the kernel backlog.
    pub max_connections: usize,
    /// Deadline for a request head, and for an idle keep-alive connection.
    pub header_read_timeout: Duration,
    /// Deadline for a whole request body, counted from the first read.
    pub body_read_timeout: Duration,
    /// How long shutdown lets open requests finish before dropping them.
    pub shutdown_grace: Duration,
    /// Trust `CF-Connecting-IP` from loopback peers. The server turns this on
    /// when a tunnel is configured; tests can turn it on without one.
    pub trust_forwarded: bool,
    /// Trust the last `X-Forwarded-For` hop from loopback peers
    /// (`--trust-proxy`), for a reverse proxy on the same host.
    pub trust_proxy: bool,
    /// Web origins allowed to call the API from a browser (`--cors-origin`).
    /// Empty means no cross-origin access.
    pub cors_origins: Vec<String>,
    /// Extra host names accepted in the `Host` header (`--allowed-host`),
    /// besides IP addresses, `localhost` and the public URL's host.
    pub allowed_hosts: Vec<String>,
}

impl Default for NetLimits {
    fn default() -> Self {
        Self {
            max_connections: DEFAULT_MAX_CONNECTIONS,
            header_read_timeout: HEADER_READ_TIMEOUT,
            body_read_timeout: BODY_READ_TIMEOUT,
            shutdown_grace: SHUTDOWN_GRACE,
            trust_forwarded: false,
            trust_proxy: false,
            cors_origins: Vec::new(),
            allowed_hosts: Vec::new(),
        }
    }
}

/// Which forwarded-address headers to believe from a loopback peer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Trust {
    /// `CF-Connecting-IP`, set by cloudflared.
    pub tunnel: bool,
    /// The last `X-Forwarded-For` hop, set by a reverse proxy.
    pub proxy: bool,
}

/// The connection a request arrived on, as a request extension.
#[derive(Debug, Clone, Copy)]
pub struct Peer {
    /// Socket peer address.
    pub addr: SocketAddr,
    /// Forwarded-address headers that may be believed when the peer is loopback.
    pub trust: Trust,
}

/// The rightmost address in the `X-Forwarded-For` headers: the one the
/// nearest proxy appended. Earlier entries come from the client and prove
/// nothing.
fn last_forwarded_hop(headers: &HeaderMap) -> Option<IpAddr> {
    let last = headers
        .get_all(X_FORWARDED_FOR)
        .iter()
        .next_back()?
        .to_str()
        .ok()?
        .rsplit(',')
        .next()?
        .trim();
    last.parse::<IpAddr>()
        .ok()
        .or_else(|| last.parse::<SocketAddr>().ok().map(|s| s.ip()))
}

/// Groups an address the way rate limits and connection shares count it:
/// IPv4 as is, IPv6 by its /64, which one subscriber usually controls whole.
fn group(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => {
            let s = v6.segments();
            IpAddr::V6(Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
        }
        v4 => v4,
    }
}

/// Rate-limit key for the client behind a request.
///
/// The socket peer address, unless the peer is loopback and a trusted
/// forwarder supplied the client's address: `CF-Connecting-IP` when a tunnel
/// is active, or the last `X-Forwarded-For` hop with `--trust-proxy`. The
/// headers are ignored from any other peer, since anyone can send them. IPv6
/// addresses are grouped by their /64.
#[must_use]
pub fn client_key(peer: SocketAddr, headers: &HeaderMap, trust: Trust) -> String {
    let peer_ip = peer.ip().to_canonical();
    let mut ip = peer_ip;
    if peer_ip.is_loopback() {
        let tunnel = trust
            .tunnel
            .then(|| headers.get(CF_CONNECTING_IP))
            .flatten()
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<IpAddr>().ok());
        let proxy = || trust.proxy.then(|| last_forwarded_hop(headers)).flatten();
        if let Some(forwarded) = tunnel.or_else(proxy) {
            ip = forwarded;
        }
    }
    match group(ip) {
        IpAddr::V6(v6) => format!("{v6}/64"),
        v4 => v4.to_string(),
    }
}

/// Rate-limit key for a request, from the [`Peer`] extension that [`serve`]
/// attaches. Requests that did not come through [`serve`] share one key.
#[must_use]
pub fn request_client_key(extensions: &axum::http::Extensions, headers: &HeaderMap) -> String {
    match extensions.get::<Peer>() {
        Some(p) => client_key(p.addr, headers, p.trust),
        None => "unknown".to_owned(),
    }
}

/// Connections one remote client address may hold at once: an eighth of the
/// cap, but at least [`MIN_PER_CLIENT_CONNECTIONS`] and never more than the cap.
#[must_use]
pub fn per_client_share(max_connections: usize) -> usize {
    let max = max_connections.max(1);
    (max / 8).max(MIN_PER_CLIENT_CONNECTIONS).min(max)
}

/// Open connections per grouped client address.
type OpenCounts = Arc<Mutex<HashMap<IpAddr, usize>>>;

/// Counts open connections per client address, so one client cannot take
/// every slot. Loopback peers are not counted: behind a tunnel or reverse
/// proxy every client arrives from loopback, and the proxy has its own limits.
#[derive(Debug, Clone)]
pub struct ClientShares {
    limit: usize,
    open: OpenCounts,
}

impl ClientShares {
    /// Shares for a listener capped at `max_connections`.
    #[must_use]
    pub fn new(max_connections: usize) -> Self {
        Self {
            limit: per_client_share(max_connections),
            open: Arc::default(),
        }
    }

    /// Takes a share for `peer`, or returns `None` when that client already
    /// holds its full share.
    #[must_use]
    pub fn claim(&self, peer: SocketAddr) -> Option<ClientShare> {
        if peer.ip().to_canonical().is_loopback() {
            return Some(ClientShare { held: None });
        }
        let ip = group(peer.ip());
        let Ok(mut open) = self.open.lock() else {
            return None;
        };
        let n = open.entry(ip).or_insert(0);
        if *n >= self.limit {
            return None;
        }
        *n += 1;
        Some(ClientShare {
            held: Some((Arc::clone(&self.open), ip)),
        })
    }
}

/// One client's hold on a connection, released on drop.
#[derive(Debug)]
pub struct ClientShare {
    held: Option<(OpenCounts, IpAddr)>,
}

impl Drop for ClientShare {
    fn drop(&mut self) {
        if let Some((open, ip)) = self.held.take()
            && let Ok(mut open) = open.lock()
            && let Some(n) = open.get_mut(&ip)
        {
            *n = n.saturating_sub(1);
            if *n == 0 {
                open.remove(&ip);
            }
        }
    }
}

/// The body deadline ran out.
#[derive(Debug, thiserror::Error)]
#[error("request body not received in time")]
pub struct BodyTimedOut;

/// A request body that fails once `timeout` has passed since its first read,
/// however steadily bytes arrive.
#[derive(Debug)]
pub struct DeadlineBody<B> {
    inner: B,
    timeout: Duration,
    deadline: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl<B> DeadlineBody<B> {
    /// Wraps `inner` with a total read deadline of `timeout`.
    pub fn new(inner: B, timeout: Duration) -> Self {
        Self {
            inner,
            timeout,
            deadline: None,
        }
    }
}

impl<B> Body for DeadlineBody<B>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: Into<axum::BoxError>,
{
    type Data = Bytes;
    type Error = axum::BoxError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        if let Poll::Ready(frame) = Pin::new(&mut this.inner).poll_frame(cx) {
            return Poll::Ready(frame.map(|r| r.map_err(Into::into)));
        }
        let timeout = this.timeout;
        let deadline = this
            .deadline
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(timeout)));
        match deadline.as_mut().poll(cx) {
            Poll::Ready(()) => Poll::Ready(Some(Err(Box::new(BodyTimedOut)))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// IO that holds a connection slot, and its client's share, until it is
/// dropped.
#[derive(Debug)]
pub struct Limited<T> {
    io: T,
    _permit: OwnedSemaphorePermit,
    _share: Option<ClientShare>,
}

impl<T> Limited<T> {
    /// Wraps `io`, keeping `permit` for its lifetime.
    pub fn new(io: T, permit: OwnedSemaphorePermit) -> Self {
        Self {
            io,
            _permit: permit,
            _share: None,
        }
    }

    /// Wraps `io`, keeping `permit` and the client's `share` for its lifetime.
    pub fn with_share(io: T, permit: OwnedSemaphorePermit, share: ClientShare) -> Self {
        Self {
            io,
            _permit: permit,
            _share: Some(share),
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

/// A plain TCP listener that stops accepting while `max` connections are
/// open, and closes new connections from a client that already holds its
/// [`per_client_share`].
#[derive(Debug)]
pub struct LimitedListener {
    tcp: TcpListener,
    slots: Arc<Semaphore>,
    shares: ClientShares,
}

impl LimitedListener {
    /// Wraps a bound listener. A `max` of zero is treated as one.
    #[must_use]
    pub fn new(tcp: TcpListener, max: usize) -> Self {
        Self {
            tcp,
            slots: Arc::new(Semaphore::new(max.max(1))),
            shares: ClientShares::new(max),
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
                Ok((stream, peer)) => match self.shares.claim(peer) {
                    Some(share) => return (Limited::with_share(stream, permit, share), peer),
                    None => {
                        // Over its share: close at once and free the slot.
                        tracing::debug!(%peer, "client holds its share of connections");
                        drop(stream);
                        drop(permit);
                    }
                },
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

/// Serves `app` on `listener` until `shutdown` fires, then gives open
/// connections [`NetLimits::shutdown_grace`] to finish their current request
/// and drops whatever is left.
///
/// Each request carries [`ConnectInfo`] and [`Peer`] extensions with the
/// socket peer address, and a body that fails after
/// [`NetLimits::body_read_timeout`].
pub async fn serve<L>(mut listener: L, app: Router, limits: &NetLimits, shutdown: CancellationToken)
where
    L: Listener<Addr = SocketAddr>,
{
    let graceful = GracefulShutdown::new();
    // Fires when the grace period is over, to drop stragglers.
    let abandon = CancellationToken::new();
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(limits.header_read_timeout);
    let trust = Trust {
        tunnel: limits.trust_forwarded,
        proxy: limits.trust_proxy,
    };
    let body_timeout = limits.body_read_timeout;
    loop {
        let (io, addr) = tokio::select! {
            () = shutdown.cancelled() => break,
            conn = listener.accept() => conn,
        };
        let app = app.clone();
        let peer = Peer { addr, trust };
        let svc = hyper::service::service_fn(move |req: Request<Incoming>| {
            let mut req = req.map(|body| DeadlineBody::new(body, body_timeout));
            req.extensions_mut().insert(ConnectInfo(addr));
            req.extensions_mut().insert(peer);
            let mut app = app.clone();
            async move { app.call(req).await }
        });
        let conn = graceful.watch(builder.serve_connection(TokioIo::new(io), svc));
        let abandon = abandon.clone();
        tokio::spawn(async move {
            tokio::select! {
                res = conn => {
                    if let Err(e) = res {
                        tracing::trace!(peer = %addr, error = %e, "connection ended with an error");
                    }
                }
                // Dropping the connection closes its socket and frees its slot.
                () = abandon.cancelled() => {}
            }
        });
    }
    // Stop accepting before waiting, so new clients are refused promptly.
    drop(listener);
    if tokio::time::timeout(limits.shutdown_grace, graceful.shutdown())
        .await
        .is_err()
    {
        tracing::debug!("dropping connections still open after the shutdown grace period");
        abandon.cancel();
    }
}

/// The host name in a `Host` header or URL authority: lowercased, without
/// port, IPv6 brackets or a trailing dot.
#[must_use]
pub fn host_name(authority: &str) -> Option<String> {
    let a = authority.trim();
    let name = if let Some(rest) = a.strip_prefix('[') {
        let (inner, after) = rest.split_once(']')?;
        if !(after.is_empty() || after.starts_with(':')) {
            return None;
        }
        inner
    } else if a.matches(':').count() == 1 {
        a.split_once(':').map_or(a, |(h, _)| h)
    } else {
        a
    };
    let name = name.strip_suffix('.').unwrap_or(name);
    if name.is_empty() || name.contains(['/', '@', ' ', '?', '#']) {
        return None;
    }
    Some(name.to_ascii_lowercase())
}

/// The host name of an `http(s)://host[:port][/path]` URL.
#[must_use]
pub fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    host_name(rest.split(['/', '?', '#']).next().unwrap_or(rest))
}

/// Whether a `Host` header names this server, which defeats DNS rebinding: a
/// page on a rebound domain still sends that domain as its `Host`.
///
/// IP addresses (the bind address, loopback, LAN addresses) and `localhost`
/// are always accepted, as are the public URL's host and `extra` names. A
/// request without a `Host` header is accepted; browsers always send one.
#[must_use]
pub fn host_allowed(host: Option<&str>, public_host: Option<&str>, extra: &[String]) -> bool {
    let Some(host) = host else { return true };
    let Some(name) = host_name(host) else {
        return false;
    };
    name.parse::<IpAddr>().is_ok()
        || name == "localhost"
        || public_host == Some(name.as_str())
        || extra
            .iter()
            .any(|e| host_name(e).as_deref() == Some(name.as_str()))
}

/// Parses a `--cors-origin` value: `http(s)://host[:port]`, no path.
///
/// # Errors
///
/// Returns a message for anything that is not a single web origin.
pub fn parse_origin(s: &str) -> Result<String, String> {
    let s = s.trim().trim_end_matches('/');
    let rest = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .ok_or_else(|| format!("{s:?} is not an origin: use http(s)://host[:port]"))?;
    if rest.is_empty() || rest.contains(['/', '?', '#', '*', ' ', '@']) {
        return Err(format!(
            "{s:?} is not an origin: use http(s)://host[:port] with no path or wildcard"
        ));
    }
    Ok(s.to_ascii_lowercase())
}

/// Parses an `--allowed-host` value: a host name, optionally with a port.
///
/// # Errors
///
/// Returns a message for anything that is not a host name.
pub fn parse_allowed_host(s: &str) -> Result<String, String> {
    host_name(s).ok_or_else(|| format!("{s:?} is not a host name"))
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

    const NONE: Trust = Trust {
        tunnel: false,
        proxy: false,
    };
    const TUNNEL: Trust = Trust {
        tunnel: true,
        proxy: false,
    };
    const PROXY: Trust = Trust {
        tunnel: false,
        proxy: true,
    };

    fn xff(values: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for v in values {
            h.append(X_FORWARDED_FOR, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn trust_proxy_uses_the_last_forwarded_hop_from_loopback() {
        let h = xff(&["198.51.100.1, 203.0.113.9"]);
        assert_eq!(client_key(sock("127.0.0.1:1"), &h, PROXY), "203.0.113.9");
        let h = xff(&["198.51.100.1", "203.0.113.10:4444"]);
        assert_eq!(client_key(sock("[::1]:1"), &h, PROXY), "203.0.113.10");
        let h = xff(&["2001:db8:1:2::7"]);
        assert_eq!(
            client_key(sock("127.0.0.1:1"), &h, PROXY),
            "2001:db8:1:2::/64"
        );
    }

    #[test]
    fn forwarded_for_is_ignored_without_trust_proxy_or_from_remote_peers() {
        let h = xff(&["203.0.113.9"]);
        assert_eq!(client_key(sock("127.0.0.1:1"), &h, NONE), "127.0.0.1");
        assert_eq!(client_key(sock("127.0.0.1:1"), &h, TUNNEL), "127.0.0.1");
        assert_eq!(client_key(sock("192.0.2.7:1"), &h, PROXY), "192.0.2.7");
        let h = xff(&["garbage"]);
        assert_eq!(client_key(sock("127.0.0.1:1"), &h, PROXY), "127.0.0.1");
    }

    #[test]
    fn the_client_share_is_an_eighth_of_the_cap_within_bounds() {
        assert_eq!(per_client_share(512), 64);
        assert_eq!(per_client_share(16), 4);
        assert_eq!(per_client_share(2), 2);
        assert_eq!(per_client_share(0), 1);
    }

    #[test]
    fn client_shares_cap_one_remote_address_and_free_on_drop() {
        let shares = ClientShares::new(16);
        let held: Vec<_> = (0..4)
            .map(|i| shares.claim(sock(&format!("192.0.2.7:{}", 1000 + i))))
            .collect();
        assert!(held.iter().all(Option::is_some));
        assert!(shares.claim(sock("192.0.2.7:2000")).is_none());
        // The same /64 counts as one client; another address does not.
        let v6: Vec<_> = (1..=4)
            .map(|i| shares.claim(sock(&format!("[2001:db8:1:2::{i}]:1"))))
            .collect();
        assert!(v6.iter().all(Option::is_some));
        assert!(shares.claim(sock("[2001:db8:1:2:ffff::1]:1")).is_none());
        assert!(shares.claim(sock("192.0.2.8:1")).is_some());
        drop(held);
        assert!(shares.claim(sock("192.0.2.7:2000")).is_some());
    }

    #[test]
    fn loopback_peers_are_not_counted_against_a_share() {
        let shares = ClientShares::new(8);
        let held: Vec<_> = (0..32)
            .map(|i| shares.claim(sock(&format!("127.0.0.1:{}", 1000 + i))))
            .collect();
        assert!(held.iter().all(Option::is_some));
    }

    #[test]
    fn host_names_drop_ports_brackets_and_case() {
        assert_eq!(
            host_name("Example.COM:8080").as_deref(),
            Some("example.com")
        );
        assert_eq!(host_name("[::1]:7878").as_deref(), Some("::1"));
        assert_eq!(host_name("::1").as_deref(), Some("::1"));
        assert_eq!(host_name("localhost.").as_deref(), Some("localhost"));
        assert_eq!(host_name("").as_deref(), None);
        assert_eq!(host_name("[::1]x").as_deref(), None);
        assert_eq!(
            url_host("https://hashi.example.com:8443/base").as_deref(),
            Some("hashi.example.com")
        );
    }

    #[test]
    fn hosts_are_checked_against_ips_localhost_the_public_url_and_extras() {
        let extra = vec!["box.lan".to_owned()];
        let public = Some("hashi.example.com");
        for ok in [
            "127.0.0.1:7878",
            "[::1]:7878",
            "10.0.0.5",
            "localhost:7878",
            "hashi.example.com",
            "BOX.lan:1",
        ] {
            assert!(host_allowed(Some(ok), public, &extra), "{ok}");
        }
        for bad in [
            "attacker.example",
            "hashi.example.com.attacker.example",
            "x",
            "",
        ] {
            assert!(!host_allowed(Some(bad), public, &extra), "{bad}");
        }
        assert!(host_allowed(None, None, &[]));
    }

    #[test]
    fn origins_are_validated() {
        assert_eq!(
            parse_origin("https://App.example:8443/").unwrap(),
            "https://app.example:8443"
        );
        for bad in [
            "*",
            "app.example",
            "https://app.example/path",
            "https://*.example",
        ] {
            assert!(parse_origin(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn uses_the_peer_address_by_default() {
        let key = client_key(sock("192.0.2.7:5000"), &headers(None), NONE);
        assert_eq!(key, "192.0.2.7");
    }

    #[test]
    fn trusts_the_tunnel_header_from_loopback() {
        let h = headers(Some("203.0.113.9"));
        assert_eq!(
            client_key(sock("127.0.0.1:5000"), &h, TUNNEL),
            "203.0.113.9"
        );
        assert_eq!(client_key(sock("[::1]:5000"), &h, TUNNEL), "203.0.113.9");
    }

    #[test]
    fn ignores_the_header_without_a_tunnel() {
        let h = headers(Some("203.0.113.9"));
        assert_eq!(client_key(sock("127.0.0.1:5000"), &h, NONE), "127.0.0.1");
    }

    #[test]
    fn ignores_a_spoofed_header_from_a_remote_peer() {
        let h = headers(Some("203.0.113.9"));
        assert_eq!(client_key(sock("192.0.2.7:5000"), &h, TUNNEL), "192.0.2.7");
        assert_eq!(
            client_key(sock("[2001:db8:1:2:3:4:5:6]:5000"), &h, TUNNEL),
            "2001:db8:1:2::/64"
        );
    }

    #[test]
    fn falls_back_to_the_peer_on_a_garbage_header() {
        let h = headers(Some("not an address"));
        assert_eq!(client_key(sock("127.0.0.1:5000"), &h, TUNNEL), "127.0.0.1");
    }

    #[test]
    fn groups_ipv6_by_prefix_and_unmaps_ipv4() {
        let a = client_key(sock("[2001:db8:1:2::1]:1"), &headers(None), NONE);
        let b = client_key(sock("[2001:db8:1:2:ffff::9]:1"), &headers(None), NONE);
        assert_eq!(a, b);
        let mapped = client_key(sock("[::ffff:192.0.2.7]:1"), &headers(None), NONE);
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
