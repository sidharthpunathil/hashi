//! In-process event bus. The queue publishes, the TUI and long-poll waiters subscribe.

use tokio::sync::broadcast;

use crate::agent::AgentKind;
use crate::guard::HoldReason;
use crate::usage::UsageState;

/// Something worth telling observers about.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A request entered the queue.
    Queued {
        /// Request id.
        id: String,
        /// Target agent.
        agent: AgentKind,
        /// Submitting token's name.
        token: String,
    },
    /// A worker picked a request up.
    Started {
        /// Request id.
        id: String,
        /// Target agent.
        agent: AgentKind,
    },
    /// A request reached a terminal state.
    Finished {
        /// Request id.
        id: String,
        /// Target agent.
        agent: AgentKind,
        /// `true` when it completed successfully.
        ok: bool,
        /// Run time in milliseconds.
        duration_ms: u64,
        /// Failure reason, when `ok` is false.
        error: Option<String>,
    },
    /// Queued work is held back because the machine is short of a resource.
    /// Published at most once a minute.
    Held {
        /// Which resource is short.
        reason: HoldReason,
        /// Figures for the owner's dashboard and log.
        detail: String,
    },
    /// The public tunnel is up.
    TunnelReady(String),
    /// The public tunnel failed or closed.
    TunnelDown(String),
    /// Tailscale asks the owner to approve something (such as turning on
    /// Funnel for the tailnet) at this URL, exactly as its CLI printed it.
    ApprovalNeeded(String),
    /// A newer Hashi release is published.
    UpdateAvailable {
        /// The new version, such as `0.3.0`.
        version: String,
        /// Its release page.
        url: String,
    },
    /// An account's usage changed: its state, or its whole-percent headroom.
    /// Never published to token holders.
    AccountUsage {
        /// Agent.
        agent: AgentKind,
        /// Account name.
        account: String,
        /// The state now.
        state: UsageState,
        /// The state before, when one was published.
        previous: Option<UsageState>,
        /// Percent left, when known.
        headroom: Option<u8>,
        /// Until when it is limited, Unix ms.
        until: Option<i64>,
    },
    /// Free-form operational message.
    Log(String),
}

impl Event {
    /// For an [`Event::AccountUsage`] whose state changed in a way the owner
    /// should hear about (crossed the threshold, became limited, or
    /// recovered), the line to show.
    #[must_use]
    pub fn usage_alert(&self) -> Option<String> {
        let Self::AccountUsage {
            agent,
            account,
            state,
            previous,
            headroom,
            until,
        } = self
        else {
            return None;
        };
        let name = format!("{} account {account}", agent.display_name());
        let left = headroom
            .map(|h| format!(" ({h}% left)"))
            .unwrap_or_default();
        match (previous, state) {
            (Some(p), s) if p == s => None,
            (_, UsageState::Limited) => Some(match until.and_then(crate::util::rfc3339) {
                Some(t) => format!("{name} is at its limit until {t}"),
                None => format!("{name} is at its limit"),
            }),
            (_, UsageState::Warning) => Some(format!("{name} crossed its rotate threshold{left}")),
            (Some(UsageState::Limited | UsageState::Warning), UsageState::Ok) => {
                Some(format!("{name} recovered{left}"))
            }
            _ => None,
        }
    }
}

/// Cloneable publisher handle.
#[derive(Debug, Clone)]
pub struct Bus {
    tx: broadcast::Sender<Event>,
}

impl Bus {
    /// Creates a bus that buffers up to `capacity` events per slow subscriber.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Publishes an event. Having no subscribers is not an error.
    pub fn emit(&self, event: Event) {
        let _ = self.tx.send(event);
    }

    /// Publishes a log line.
    pub fn log(&self, msg: impl Into<String>) {
        self.emit(Event::Log(msg.into()));
    }

    /// Subscribes to future events.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

impl Default for Bus {
    fn default() -> Self {
        Self::new(1024)
    }
}
