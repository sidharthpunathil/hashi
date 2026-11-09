//! How a share link reaches people: through the Cloudflare tunnel,
//! through Tailscale, over the local wifi, or only on this machine.
//!
//! Every way to share (an invite, a login share, a pool) builds its link
//! base here, from the running server in the dashboard and from what the
//! server recorded in the CLI.

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use crate::api::Runtime;
use crate::server::PublicRoute;

/// How a link reaches people.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Via {
    /// The public route if one is up, else this machine.
    #[default]
    Auto,
    /// The Cloudflare tunnel: anyone, over HTTPS.
    Cloudflare,
    /// Tailscale: the tailnet (serve) or anyone (funnel), over HTTPS.
    Tailscale,
    /// The local network: everyone on this wifi, over plain HTTP.
    Lan,
    /// This machine only.
    Local,
}

impl Via {
    /// Every choice, in the order forms cycle through them.
    pub const ALL: [Self; 5] = [
        Self::Auto,
        Self::Cloudflare,
        Self::Tailscale,
        Self::Lan,
        Self::Local,
    ];

    /// Stable name, as `--via` takes it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Cloudflare => "cloudflare",
            Self::Tailscale => "tailscale",
            Self::Lan => "lan",
            Self::Local => "local",
        }
    }

    /// For people, in forms.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "whatever is public now",
            Self::Cloudflare => "Cloudflare tunnel (anyone)",
            Self::Tailscale => "Tailscale (your tailnet)",
            Self::Lan => "this wifi (local network)",
            Self::Local => "this machine only",
        }
    }

    /// The route to start for this choice, when it needs one.
    #[must_use]
    pub const fn route(self) -> Option<PublicRoute> {
        match self {
            Self::Cloudflare => Some(PublicRoute::Cloudflare),
            Self::Tailscale => Some(PublicRoute::Tailscale(crate::tailscale::Mode::Serve)),
            _ => None,
        }
    }
}

impl fmt::Display for Via {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Via {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "cloudflare" | "tunnel" => Ok(Self::Cloudflare),
            "tailscale" => Ok(Self::Tailscale),
            "lan" | "wifi" => Ok(Self::Lan),
            "local" => Ok(Self::Local),
            other => Err(format!(
                "unknown route `{other}` (expected auto, cloudflare, tailscale, lan or local)"
            )),
        }
    }
}

/// Why a link cannot be made for a route yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotReady {
    /// The route is not up; starting it makes the link.
    Start(PublicRoute),
    /// The server listens on this machine only.
    NotOnLan,
    /// This machine has no address on a local network.
    NoLan,
}

impl fmt::Display for NotReady {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Start(route) => write!(f, "{} is not running yet", route.label()),
            Self::NotOnLan => f.write_str(
                "the server listens on this machine only; listen on 0.0.0.0 \
                 (`hashi serve --bind 0.0.0.0`, or Port and bind address in the dashboard)",
            ),
            Self::NoLan => f.write_str("this machine is not on a local network"),
        }
    }
}

/// Whether `url`'s host is a Tailscale name.
#[must_use]
pub fn is_tailscale_url(url: &str) -> bool {
    crate::net::url_host(url).is_some_and(|h| h.ends_with(".ts.net"))
}

/// `scheme://<lan ip>:<port>`, when this machine is on a local network.
#[must_use]
pub fn lan_base(port: u16, tls: bool) -> Option<String> {
    let ip = crate::pool::lan_ip()?;
    let scheme = if tls { "https" } else { "http" };
    Some(match ip {
        std::net::IpAddr::V4(v4) => format!("{scheme}://{v4}:{port}"),
        std::net::IpAddr::V6(v6) => format!("{scheme}://[{v6}]:{port}"),
    })
}

/// Whether listening on `ip` reaches the local network.
fn reaches_lan(ip: std::net::IpAddr) -> bool {
    ip.is_unspecified() || crate::pool::is_lan_host(&ip.to_string())
}

/// The link base for `via` on the running server.
///
/// # Errors
///
/// Says what has to happen first.
pub fn base(rt: &Runtime, via: Via) -> Result<String, NotReady> {
    let active = crate::server::active_route(rt);
    let public = rt.public_url();
    match via {
        Via::Auto => Ok(rt.base_url()),
        Via::Local => Ok(rt.local_url()),
        Via::Cloudflare => match (active, public) {
            (Some(PublicRoute::Cloudflare), Some(url)) => Ok(url),
            // A named tunnel or a configured URL is public without a route.
            (None, Some(url)) if !is_tailscale_url(&url) => Ok(url),
            _ => Err(NotReady::Start(PublicRoute::Cloudflare)),
        },
        Via::Tailscale => match (active, public) {
            (Some(PublicRoute::Tailscale(_)), Some(url)) => Ok(url),
            _ => Err(NotReady::Start(PublicRoute::Tailscale(
                crate::tailscale::Mode::Serve,
            ))),
        },
        Via::Lan => {
            let addr = rt.listen.addr().ok_or(NotReady::NotOnLan)?;
            if !reaches_lan(addr.ip()) {
                return Err(NotReady::NotOnLan);
            }
            lan_base(addr.port(), rt.listen.tls()).ok_or(NotReady::NoLan)
        }
    }
}

/// The link base for `via` from the CLI, which only has what the server
/// recorded in `home`: its public URL, and the bind address and port saved
/// from the dashboard (or `port`, `bind` given here).
///
/// # Errors
///
/// Says how to make the route work.
pub fn cli_base(
    home: &Path,
    via: Via,
    public_url: Option<&str>,
    port: Option<u16>,
) -> Result<(String, bool), String> {
    let saved = crate::settings::load(home).ok();
    let (port, _) = crate::settings::resolve(
        port,
        saved.as_ref().and_then(|s| s.port),
        crate::settings::DEFAULT_PORT,
    );
    let public = public_url
        .map(|u| u.trim_end_matches('/').to_owned())
        .or_else(|| crate::server::read_public_url(home));
    let local = format!("http://127.0.0.1:{port}");
    match via {
        Via::Auto => Ok(public.map_or((local, true), |p| (p, false))),
        Via::Local => Ok((local, true)),
        Via::Cloudflare => match public {
            Some(p) if !is_tailscale_url(&p) => Ok((p, false)),
            _ => Err(
                "the Cloudflare tunnel is not running: press t in the dashboard, or start with \
                 `hashi serve --tunnel`"
                    .to_owned(),
            ),
        },
        Via::Tailscale => match public {
            Some(p) if is_tailscale_url(&p) => Ok((p, false)),
            _ => Err(
                "Tailscale is not serving Hashi: pick Tailscale serve in the dashboard's \
                 Commands, or start with `hashi serve --tailscale serve`"
                    .to_owned(),
            ),
        },
        Via::Lan => {
            let bind = saved.and_then(|s| s.bind);
            if !bind.is_some_and(reaches_lan) {
                return Err(NotReady::NotOnLan.to_string());
            }
            lan_base(port, false)
                .map(|b| (b, false))
                .ok_or_else(|| NotReady::NoLan.to_string())
        }
    }
}

/// What to tell people about a link over `via`.
#[must_use]
pub fn note(via: Via) -> Option<&'static str> {
    match via {
        Via::Lan => Some(
            "this link is plain http on your local network: anyone on this wifi can read what \
             it carries. Use the Cloudflare tunnel or Tailscale on a wifi you do not trust.",
        ),
        Via::Local => Some("this link only works on this machine."),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_parse_and_print() {
        for v in Via::ALL {
            assert_eq!(v.as_str().parse::<Via>().unwrap(), v);
        }
        assert_eq!("wifi".parse::<Via>().unwrap(), Via::Lan);
        assert_eq!("tunnel".parse::<Via>().unwrap(), Via::Cloudflare);
        assert!("carrier-pigeon".parse::<Via>().is_err());
    }

    #[test]
    fn tailscale_urls_are_told_apart() {
        assert!(is_tailscale_url("https://studio.tail1234.ts.net"));
        assert!(!is_tailscale_url("https://abc.trycloudflare.com"));
    }

    #[test]
    fn the_cli_explains_a_route_that_is_not_up() {
        let home = tempfile::tempdir().unwrap();
        let e = cli_base(home.path(), Via::Cloudflare, None, Some(7878)).unwrap_err();
        assert!(e.contains("--tunnel"), "{e}");
        let e = cli_base(home.path(), Via::Lan, None, Some(7878)).unwrap_err();
        assert!(e.contains("0.0.0.0"), "{e}");
        let (b, local) = cli_base(home.path(), Via::Auto, None, Some(7878)).unwrap();
        assert_eq!((b.as_str(), local), ("http://127.0.0.1:7878", true));
        let (b, _) = cli_base(
            home.path(),
            Via::Tailscale,
            Some("https://box.tail1.ts.net/"),
            None,
        )
        .unwrap();
        assert_eq!(b, "https://box.tail1.ts.net");
    }
}
