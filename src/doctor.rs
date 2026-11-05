//! The checks `hashi doctor` prints and the dashboard's doctor popup shows:
//! agents, helpers, accounts, the update state and where Hashi keeps its
//! data. Each check is a mark, a name and one line of detail.

use std::path::Path;

use crate::accounts::AccountKind;
use crate::agent::AgentKind;
use crate::api::AgentInfo;
use crate::diagnostics::terminal_safe;
use crate::store::Store;
use crate::tailscale::SelfStatus;
use crate::util::{now_ms, truncate};

/// How a check went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// Found and working.
    Ok,
    /// Missing but optional, or only informational.
    Info,
    /// Needs the owner's attention.
    Warn,
}

impl Mark {
    /// The symbol printed before the check.
    #[must_use]
    pub const fn glyph(self) -> &'static str {
        match self {
            Self::Ok => "✓",
            Self::Info => "·",
            Self::Warn => "!",
        }
    }
}

/// One line of the doctor's report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// How it went.
    pub mark: Mark,
    /// What was checked, such as `cloudflared`.
    pub name: String,
    /// What was found.
    pub detail: String,
}

impl Check {
    fn new(mark: Mark, name: &str, detail: impl Into<String>) -> Self {
        Self {
            mark,
            name: name.to_owned(),
            detail: detail.into(),
        }
    }

    /// The line as `hashi doctor` prints it, without the indent.
    #[must_use]
    pub fn line(&self) -> String {
        format!("{} {:<12} {}", self.mark.glyph(), self.name, self.detail)
    }
}

/// The agents found, with versions, and how many were found.
#[must_use]
pub fn agents(found: &[AgentInfo]) -> (Vec<Check>, usize) {
    let mut n = 0;
    let checks = found
        .iter()
        .map(|info| match &info.version {
            Some(v) => {
                n += 1;
                Check::new(
                    Mark::Ok,
                    info.agent.display_name(),
                    format!("{v}  ({})", info.path),
                )
            }
            None => Check::new(
                Mark::Info,
                info.agent.display_name(),
                "not installed (optional)",
            ),
        })
        .collect();
    (checks, n)
}

/// Crush's line when it is not among the configured agents: installed but
/// not enabled, or neither. `enabled` is `Some(allow_newer)` when it is on.
#[must_use]
pub fn crush(enabled: Option<bool>, installed: Option<&Path>) -> Check {
    let name = AgentKind::Crush.display_name();
    match (enabled, installed) {
        (Some(newer), _) => Check::new(
            Mark::Info,
            name,
            format!(
                "enabled (--enable-crush){}",
                if newer {
                    ", newer releases allowed (--crush-allow-newer)"
                } else {
                    ""
                }
            ),
        ),
        (None, Some(p)) => Check::new(
            Mark::Info,
            name,
            format!(
                "installed ({}), not enabled: start with --enable-crush to offer it",
                p.display()
            ),
        ),
        (None, None) => Check::new(
            Mark::Info,
            name,
            "not enabled (opt-in: --enable-crush; hashi install crush)",
        ),
    }
}

/// cloudflared's line.
#[must_use]
pub fn cloudflared(path: Option<&Path>) -> Check {
    match path {
        Some(p) => Check::new(Mark::Ok, "cloudflared", p.display().to_string()),
        None => Check::new(
            Mark::Info,
            "cloudflared",
            "not installed (only needed for --tunnel): hashi install cloudflared",
        ),
    }
}

/// Tailscale's line, with its sign-in state when it was read. `status` is
/// `None` while it has not been asked yet.
#[must_use]
pub fn tailscale(bin: Option<&Path>, status: Option<Result<SelfStatus, String>>) -> Check {
    let Some(bin) = bin else {
        return Check::new(
            Mark::Info,
            "tailscale",
            "not installed (only needed for --tailscale): hashi install tailscale",
        );
    };
    let shown = bin.display();
    match status {
        Some(Ok(me)) if me.running() => match me.dns_name {
            Some(name) => Check::new(
                Mark::Ok,
                "tailscale",
                format!("{shown}, signed in as {name}"),
            ),
            None => Check::new(
                Mark::Warn,
                "tailscale",
                format!("{shown}, signed in but no DNS name: turn on MagicDNS"),
            ),
        },
        Some(Ok(me)) => Check::new(
            Mark::Warn,
            "tailscale",
            format!(
                "{shown}, not connected ({}): {}",
                terminal_safe(&me.backend_state),
                crate::tailscale::sign_in_hint()
            ),
        ),
        Some(Err(e)) => Check::new(
            Mark::Info,
            "tailscale",
            format!(
                "{shown}, sign-in state unknown ({})",
                terminal_safe(&truncate(&e, 80))
            ),
        ),
        None => Check::new(
            Mark::Info,
            "tailscale",
            format!("{shown}, sign-in state not read yet"),
        ),
    }
}

/// The accounts each agent has besides the default.
#[must_use]
pub fn accounts(store: Option<&Store>) -> Vec<Check> {
    let none = || {
        vec![Check::new(
            Mark::Info,
            "accounts",
            "none: every agent uses your own login (hashi account add)",
        )]
    };
    let Some(store) = store else {
        return none();
    };
    let now = now_ms();
    let list: Vec<_> = store
        .list_accounts()
        .unwrap_or_default()
        .into_iter()
        .filter(|a| a.kind != AccountKind::Default)
        .collect();
    if list.is_empty() {
        return none();
    }
    AgentKind::ALL
        .into_iter()
        .filter_map(|agent| {
            let names: Vec<String> = list
                .iter()
                .filter(|a| a.agent == agent)
                .map(|a| format!("{} ({}, {})", a.name, a.kind, a.status(now).as_str()))
                .collect();
            (!names.is_empty()).then(|| {
                Check::new(
                    Mark::Ok,
                    "accounts",
                    format!("{}: {}", agent.as_str(), names.join(", ")),
                )
            })
        })
        .collect()
}

/// What is known about newer releases.
#[derive(Debug, Clone)]
pub enum UpdateState {
    /// `HASHI_NO_UPDATE_CHECK` is set.
    OptedOut,
    /// The latest release, or why it could not be found.
    Latest(Result<crate::update::Version, String>),
    /// Not asked.
    Unknown,
}

/// Hashi's own line: the running version and the latest one.
#[must_use]
pub fn update(state: &UpdateState) -> Check {
    let current = env!("CARGO_PKG_VERSION");
    match state {
        UpdateState::OptedOut => Check::new(
            Mark::Info,
            "hashi",
            format!(
                "v{current}, update check off ({})",
                crate::update::OPT_OUT_ENV
            ),
        ),
        UpdateState::Latest(Ok(v))
            if crate::update::is_newer(v, &crate::update::current_version()) =>
        {
            Check::new(
                Mark::Warn,
                "hashi",
                format!("v{current}, latest v{v}: update available, run hashi update"),
            )
        }
        UpdateState::Latest(Ok(v)) => Check::new(
            Mark::Ok,
            "hashi",
            format!("v{current}, latest v{v}: up to date"),
        ),
        UpdateState::Latest(Err(e)) => Check::new(
            Mark::Info,
            "hashi",
            format!(
                "v{current}, could not check for updates ({})",
                terminal_safe(&truncate(e, 80))
            ),
        ),
        UpdateState::Unknown => Check::new(Mark::Info, "hashi", format!("v{current}")),
    }
}

/// The closing advice, by how many agents were found.
#[must_use]
pub const fn advice(found: usize) -> &'static str {
    if found == 0 {
        "No coding agent found. Install one (for example Claude Code or Codex) and sign in."
    } else {
        "Agents that are not installed answer 503; install one any time and restart Hashi."
    }
}
