//! TUI model and update logic (the "Model" and "Update" of TEA).

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::TableState;

use super::forms::{
    AccountForm, AskForm, ConnectForm, EditTokenForm, ListenForm, ProjectForm, ReportForm,
};
use super::theme::{self, Rgb};
use crate::accounts::{self as acct, AccountMap};
use crate::agent::AgentKind;
use crate::api::AppState;
use crate::events::Event;
use crate::guard::ResourceSnapshot;
use crate::install::Helper;
use crate::server::PublicRoute;
use crate::store::{Invite, NewInvite, NewToken, Request, Revoke, Stats, TokenUsage};
use crate::tailscale::Mode;
use crate::token::{Limits, Role, SessionMode, Token, TokenStatus};
use crate::tunnel::StartError;
use crate::util::{now_ms, one_line, truncate};

/// Top-level views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// Live summary.
    Overview,
    /// Request history with details.
    Requests,
    /// Token management.
    Tokens,
    /// Event log.
    Logs,
    /// Everything the owner can do, with its CLI equivalent.
    Commands,
}

impl Tab {
    /// Display order.
    pub const ALL: [Self; 5] = [
        Self::Overview,
        Self::Requests,
        Self::Tokens,
        Self::Logs,
        Self::Commands,
    ];

    /// Tab label.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Requests => "Requests",
            Self::Tokens => "Tokens",
            Self::Logs => "Logs",
            Self::Commands => "Commands",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    fn shift(self, by: isize) -> Self {
        let n = Self::ALL.len().cast_signed();
        let i = (self.index().cast_signed() + by).rem_euclid(n);
        Self::ALL[i.cast_unsigned()]
    }
}

/// A row of the Commands view: something the owner can do from here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Start a Cloudflare quick tunnel.
    Cloudflare,
    /// Start Tailscale Serve.
    TailscaleServe,
    /// Start Tailscale Funnel.
    TailscaleFunnel,
    /// Stop whichever route is public.
    StopPublic,
    /// Create an invite link.
    NewInvite,
    /// Create a token.
    NewToken,
    /// See and decide requests for access, and open or close the link.
    AccessRequests,
    /// Send a prompt to an agent here or on a bridge.
    Ask,
    /// List and manage agent accounts.
    Accounts,
    /// List and manage projects.
    Projects,
    /// List and forget the bridges this machine connected to.
    Bridges,
    /// Redeem an invite link to someone else's Hashi.
    Connect,
    /// Move the server to another port or address.
    Listen,
    /// Copy the API base URL.
    CopyUrl,
    /// Install cloudflared.
    InstallCloudflared,
    /// Install tailscale.
    InstallTailscale,
    /// Install Crush.
    InstallCrush,
    /// Check for a newer release.
    CheckUpdate,
    /// Download and install the latest release.
    InstallUpdate,
    /// Show what `hashi doctor` checks.
    Doctor,
    /// Write a diagnostic report.
    Report,
    /// Show the audit log.
    Audit,
    /// Show every key binding.
    Help,
}

impl Action {
    /// Display order. [`Action::group`] starts a new heading where it changes.
    pub const ALL: [Self; 23] = [
        Self::Cloudflare,
        Self::TailscaleServe,
        Self::TailscaleFunnel,
        Self::StopPublic,
        Self::NewInvite,
        Self::NewToken,
        Self::AccessRequests,
        Self::Ask,
        Self::Accounts,
        Self::Projects,
        Self::Bridges,
        Self::Connect,
        Self::Listen,
        Self::CopyUrl,
        Self::InstallCloudflared,
        Self::InstallTailscale,
        Self::InstallCrush,
        Self::CheckUpdate,
        Self::InstallUpdate,
        Self::Doctor,
        Self::Report,
        Self::Audit,
        Self::Help,
    ];

    /// The heading the row sits under.
    #[must_use]
    pub const fn group(self) -> &'static str {
        match self {
            Self::Cloudflare | Self::TailscaleServe | Self::TailscaleFunnel | Self::StopPublic => {
                "Go public"
            }
            Self::NewInvite | Self::NewToken | Self::AccessRequests => "Share",
            Self::Ask => "Ask",
            Self::Accounts | Self::Projects | Self::Bridges | Self::Connect => "Manage",
            Self::Listen | Self::CopyUrl => "Server",
            Self::InstallCloudflared | Self::InstallTailscale | Self::InstallCrush => "Install",
            Self::CheckUpdate | Self::InstallUpdate | Self::Doctor | Self::Report | Self::Audit => {
                "Maintenance"
            }
            Self::Help => "Help",
        }
    }

    /// The key that runs it from anywhere, or `enter` from this view.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Cloudflare => "t",
            Self::NewInvite => "s",
            Self::NewToken => "n",
            Self::Ask => "a",
            Self::CopyUrl => "c",
            Self::Help => "?",
            _ => "enter",
        }
    }

    /// Short name.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Cloudflare => "Cloudflare quick tunnel",
            Self::TailscaleServe => "Tailscale serve",
            Self::TailscaleFunnel => "Tailscale funnel",
            Self::StopPublic => "Stop going public",
            Self::NewInvite => "New invite",
            Self::NewToken => "New token",
            Self::AccessRequests => "Access requests",
            Self::Ask => "Ask an agent",
            Self::Accounts => "Accounts",
            Self::Projects => "Projects",
            Self::Bridges => "Bridges I use",
            Self::Connect => "Connect to a bridge",
            Self::Listen => "Port and bind address",
            Self::CopyUrl => "Copy the API URL",
            Self::InstallCloudflared => "Install cloudflared",
            Self::InstallTailscale => "Install tailscale",
            Self::InstallCrush => "Install Crush",
            Self::CheckUpdate => "Check for updates",
            Self::InstallUpdate => "Install the update",
            Self::Doctor => "Doctor",
            Self::Report => "Diagnostic report",
            Self::Audit => "Audit log",
            Self::Help => "All key bindings",
        }
    }

    /// One line on what it does.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Cloudflare => "a random public HTTPS link, no account needed",
            Self::TailscaleServe => "your tailnet only, at <machine>.<tailnet>.ts.net",
            Self::TailscaleFunnel => "anyone on the internet, through Tailscale",
            Self::StopPublic => "turn off the active route; links go back to local",
            Self::NewInvite => "a one-time link that gives someone their own token",
            Self::NewToken => "an API token for a script or app",
            Self::AccessRequests => "people asking for access or for more, and the link",
            Self::Ask => "a prompt through the queue, or to a bridge you use",
            Self::Accounts => "add, sign in, disable and remove agent accounts",
            Self::Projects => "folders that tokens and invites can be bound to",
            Self::Bridges => "other people's Hashi that you connected to",
            Self::Connect => "paste an invite link someone sent you",
            Self::Listen => "move the server without stopping it",
            Self::CopyUrl => "the base URL clients should use",
            Self::InstallCloudflared => "Cloudflare's own install command for this machine",
            Self::InstallTailscale => "Tailscale's own install command for this machine",
            Self::InstallCrush => "Charm's own install command (Crush is opt-in)",
            Self::CheckUpdate => "ask GitHub for the latest release",
            Self::InstallUpdate => "download, verify and replace this binary",
            Self::Doctor => "agents, helpers, accounts, updates and the home",
            Self::Report => "a redacted file to attach to a bug report",
            Self::Audit => "recent sharing and access changes",
            Self::Help => "every key the dashboard knows",
        }
    }
}

/// Something the event loop runs with the terminal attached, the dashboard
/// stepping aside meanwhile: an install, an account sign-in or an update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingCommand {
    /// A helper's install plan.
    Install(crate::install::Plan),
    /// A login account's sign-in command.
    SignIn {
        /// The Hashi home.
        home: std::path::PathBuf,
        /// The agent.
        agent: AgentKind,
        /// The account.
        name: String,
        /// The agent's executable.
        bin: std::path::PathBuf,
    },
    /// `hashi update`: download, verify and install the latest release.
    Update {
        /// The Hashi home.
        home: std::path::PathBuf,
    },
}

impl PendingCommand {
    /// The CLI command it stands for, shown while it runs.
    #[must_use]
    pub fn title(&self) -> String {
        match self {
            Self::Install(plan) => format!("hashi install {}", plan.helper()),
            Self::SignIn { agent, name, .. } => format!("hashi account login {agent} {name}"),
            Self::Update { .. } => "hashi update".to_owned(),
        }
    }
}

/// Which agents a new token may use: every agent, or exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentChoice(pub Option<AgentKind>);

impl AgentChoice {
    /// Every agent Hashi supports.
    pub const EVERY: Self = Self(None);

    fn options() -> Vec<Self> {
        std::iter::once(Self::EVERY)
            .chain(AgentKind::ALL.into_iter().map(|a| Self(Some(a))))
            .collect()
    }

    fn next(self) -> Self {
        self.shift(1)
    }

    fn shift(self, by: isize) -> Self {
        let options = Self::options();
        let i = options.iter().position(|a| *a == self).unwrap_or(0);
        options[cycle(i, options.len(), by)]
    }

    /// Label for the form.
    #[must_use]
    pub fn label(self) -> String {
        match self.0 {
            None => "all agents".to_owned(),
            Some(agent) => format!("{} only", agent.as_str()),
        }
    }

    fn agents(self) -> Vec<AgentKind> {
        match self.0 {
            None => AgentKind::ALL.to_vec(),
            Some(agent) => vec![agent],
        }
    }
}

/// A project picked in a form: none (the token's own workspace) or one of
/// the projects that existed when the form opened.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectChoice {
    names: Vec<String>,
    /// 0 is none, `i + 1` is `names[i]`.
    at: usize,
}

impl ProjectChoice {
    /// A choice among `names`, starting at none.
    #[must_use]
    pub fn new(names: Vec<String>) -> Self {
        Self { names, at: 0 }
    }

    /// A choice among `names`, starting at `selected` when it is one of them.
    #[must_use]
    pub fn with(names: Vec<String>, selected: Option<&str>) -> Self {
        let at = selected
            .and_then(|s| names.iter().position(|n| n == s))
            .map_or(0, |i| i + 1);
        Self { names, at }
    }

    pub(crate) fn shift(&mut self, by: isize) {
        self.at = cycle(self.at, self.names.len() + 1, by);
    }

    /// The picked project's name.
    #[must_use]
    pub fn selected(&self) -> Option<&str> {
        self.at
            .checked_sub(1)
            .and_then(|i| self.names.get(i))
            .map(String::as_str)
    }

    /// Label for the form.
    #[must_use]
    pub fn label(&self) -> String {
        match self.selected() {
            Some(name) => name.to_owned(),
            None if self.names.is_empty() => "none (add one with hashi project add)".to_owned(),
            None => "none (own workspace)".to_owned(),
        }
    }
}

/// State of the "new token" form.
#[derive(Debug, Clone)]
pub struct TokenForm {
    /// Name being typed.
    pub name: String,
    /// Focused field, `0..FIELDS`.
    pub field: usize,
    /// Selected role.
    pub role: Role,
    /// Selected session mode.
    pub mode: SessionMode,
    /// Selected agents.
    pub agents: AgentChoice,
    /// Accounts per agent, typed as `claude=work,personal codex=team`.
    /// Empty means every agent uses its default account.
    pub accounts: String,
    /// The project the token is bound to.
    pub project: ProjectChoice,
    /// Whether the token may see its agents' quota headroom, as
    /// `hashi token create --share-usage`.
    pub share_usage: bool,
    /// Validation message shown under the name field.
    pub error: Option<String>,
}

impl TokenForm {
    /// Number of fields.
    pub const FIELDS: usize = 7;
    /// Index of the accounts text field.
    pub const ACCOUNTS: usize = 4;
    /// Index of the project selector, the last field.
    pub const PROJECT: usize = 6;
    /// Index of the share usage toggle.
    pub const SHARE_USAGE: usize = 5;

    fn new(projects: Vec<String>) -> Self {
        Self {
            name: String::new(),
            field: 0,
            role: Role::User,
            mode: SessionMode::Stateless,
            agents: AgentChoice::EVERY,
            accounts: String::new(),
            project: ProjectChoice::new(projects),
            share_usage: false,
            error: None,
        }
    }

    fn toggle(&mut self) {
        match self.field {
            1 => {
                self.role = if self.role == Role::User {
                    Role::Admin
                } else {
                    Role::User
                }
            }
            2 => {
                self.mode = match self.mode {
                    SessionMode::Stateless => SessionMode::Stateful,
                    SessionMode::Stateful => SessionMode::Stateless,
                }
            }
            3 => self.agents = self.agents.next(),
            Self::PROJECT => self.project.shift(1),
            Self::SHARE_USAGE => self.share_usage = !self.share_usage,
            _ => {}
        }
    }
}

/// Daily prompt cap choices for an invite.
const DAILY_PROMPTS: [Option<u32>; 4] = [None, Some(25), Some(100), Some(500)];
/// Daily budget choices for an invite, in USD.
const DAILY_BUDGET: [Option<f64>; 3] = [None, Some(5.0), Some(20.0)];
/// Token lifetime choices for an invite, in days.
const LIFETIME_DAYS: [Option<u32>; 4] = [None, Some(1), Some(7), Some(30)];
/// How long an invite link stays valid, in hours.
const LINK_HOURS: [u32; 3] = [1, 24, 168];
/// Prompts an invited token may have in flight at once.
const INVITE_MAX_PENDING: u32 = 3;

/// State of the "share" (invite) form.
#[derive(Debug, Clone)]
pub struct ShareForm {
    /// Label being typed: who the invite is for.
    pub label: String,
    /// Focused field, `0..FIELDS`.
    pub field: usize,
    /// Selected agents.
    pub agents: AgentChoice,
    /// Selected session mode.
    pub mode: SessionMode,
    daily: usize,
    budget: usize,
    lifetime: usize,
    link: usize,
    /// Accounts per agent, typed as `claude=work,personal codex=team`.
    /// Empty means every agent uses its default account.
    pub accounts: String,
    /// The project the token is bound to.
    pub project: ProjectChoice,
    /// Whether the invited token may see its agents' quota headroom, as
    /// `hashi share --share-usage`.
    pub share_usage: bool,
    /// Validation message shown under the label field.
    pub error: Option<String>,
}

impl ShareForm {
    /// Number of fields.
    pub const FIELDS: usize = 10;
    /// Index of the accounts text field.
    pub const ACCOUNTS: usize = 7;
    /// Index of the project selector, the last field.
    pub const PROJECT: usize = 9;
    /// Index of the share usage toggle.
    pub const SHARE_USAGE: usize = 8;

    fn new(projects: Vec<String>) -> Self {
        Self {
            label: String::new(),
            field: 0,
            agents: AgentChoice::EVERY,
            mode: SessionMode::Stateless,
            daily: 0,
            budget: 0,
            lifetime: 0,
            link: 1,
            accounts: String::new(),
            project: ProjectChoice::new(projects),
            share_usage: false,
            error: None,
        }
    }

    fn step(&mut self, by: isize) {
        match self.field {
            1 => self.agents = self.agents.shift(by),
            2 => {
                self.mode = match self.mode {
                    SessionMode::Stateless => SessionMode::Stateful,
                    SessionMode::Stateful => SessionMode::Stateless,
                }
            }
            3 => self.daily = cycle(self.daily, DAILY_PROMPTS.len(), by),
            4 => self.budget = cycle(self.budget, DAILY_BUDGET.len(), by),
            5 => self.lifetime = cycle(self.lifetime, LIFETIME_DAYS.len(), by),
            6 => self.link = cycle(self.link, LINK_HOURS.len(), by),
            Self::PROJECT => self.project.shift(by),
            Self::SHARE_USAGE => self.share_usage = !self.share_usage,
            _ => {}
        }
    }

    /// Daily prompt cap, as shown in the form.
    #[must_use]
    pub fn daily_label(&self) -> String {
        DAILY_PROMPTS
            .get(self.daily)
            .copied()
            .flatten()
            .map_or_else(|| "none".to_owned(), |n| format!("{n} a day"))
    }

    /// Daily budget, as shown in the form.
    #[must_use]
    pub fn budget_label(&self) -> String {
        DAILY_BUDGET
            .get(self.budget)
            .copied()
            .flatten()
            .map_or_else(|| "none".to_owned(), |b| format!("${b:.0} a day"))
    }

    /// Token lifetime, as shown in the form.
    #[must_use]
    pub fn lifetime_label(&self) -> &'static str {
        match LIFETIME_DAYS.get(self.lifetime).copied().flatten() {
            None => "never expires",
            Some(1) => "1 day",
            Some(7) => "7 days",
            Some(_) => "30 days",
        }
    }

    /// Link validity, as shown in the form.
    #[must_use]
    pub fn link_label(&self) -> &'static str {
        link_long(self.link_hours())
    }

    fn link_hours(&self) -> u32 {
        LINK_HOURS.get(self.link).copied().unwrap_or(24)
    }

    fn to_new_invite(&self, label: String, accounts: AccountMap) -> NewInvite {
        NewInvite {
            label,
            session_mode: self.mode,
            agents: self.agents.agents(),
            token_expires_in_days: LIFETIME_DAYS.get(self.lifetime).copied().flatten(),
            limits: Limits {
                daily_requests: DAILY_PROMPTS.get(self.daily).copied().flatten(),
                daily_budget_usd: DAILY_BUDGET.get(self.budget).copied().flatten(),
                max_pending: Some(INVITE_MAX_PENDING),
                ..Limits::default()
            },
            link_hours: self.link_hours(),
            accounts,
            project: self.project.selected().map(str::to_owned),
        }
    }
}

/// Longest accounts text a form accepts.
const ACCOUNTS_CHARS: usize = 200;

/// Parses a form's accounts text and checks the names exist.
fn form_accounts(store: &crate::store::Store, text: &str) -> Result<AccountMap, String> {
    let map = acct::parse_bindings(text)?;
    crate::api::check_accounts(store, map).map_err(|e| e.to_string())
}

const fn link_long(hours: u32) -> &'static str {
    match hours {
        1 => "1 hour",
        24 => "24 hours",
        _ => "7 days",
    }
}

/// Short form of a link validity: `1h`, `24h`, `7d`.
#[must_use]
pub fn link_short(hours: u32) -> String {
    if hours >= 48 && hours.is_multiple_of(24) {
        format!("{}d", hours / 24)
    } else {
        format!("{hours}h")
    }
}

/// Which table has focus in the Tokens view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokensFocus {
    /// The tokens table.
    Tokens,
    /// The pending invites table.
    Invites,
}

/// Modal overlays.
#[derive(Debug, Clone)]
pub enum Popup {
    /// Key binding reference.
    Help,
    /// A secret shown exactly once.
    Reveal {
        /// Heading.
        title: String,
        /// The secret.
        secret: String,
    },
    /// Token creation form.
    NewToken(TokenForm),
    /// Quit confirmation, shown while requests are still in flight.
    ConfirmQuit {
        /// Requests queued or running.
        pending: u64,
    },
    /// Revoke confirmation.
    ConfirmRevoke {
        /// Token id.
        id: String,
        /// Token name.
        name: String,
    },
    /// Invite creation form.
    Share(ShareForm),
    /// A freshly created invite link. The URL is built from the current base
    /// URL each time, so it turns public when a tunnel comes up.
    InviteReveal {
        /// Who the invite is for.
        label: String,
        /// The invite code.
        code: String,
        /// How long the link stays valid.
        link_hours: u32,
    },
    /// Request cancellation confirmation.
    ConfirmCancelRequest {
        /// Request id.
        id: String,
        /// Who sent it.
        token: String,
    },
    /// Invite cancellation confirmation.
    ConfirmCancelInvite {
        /// Invite id.
        id: String,
        /// Invite label.
        label: String,
    },
    /// A helper is missing: its install command, to run, copy or cancel.
    Install(crate::install::Plan),
    /// Another route is public; switching stops it first.
    ConfirmSwitch {
        /// The route that is public now.
        from: PublicRoute,
        /// The route asked for.
        to: PublicRoute,
    },
    /// Tailscale printed a URL the owner must open to approve something.
    Approval {
        /// The URL, as the CLI printed it.
        url: String,
    },
    /// Token edit form.
    EditToken(Box<EditTokenForm>),
    /// The prompt popup.
    Ask(Box<AskForm>),
    /// A bridge's answer, or the wait for it.
    RemoteAnswer {
        /// The bridge asked.
        remote: String,
        /// The answer, the error, or `None` while waiting.
        answer: Option<Result<String, String>>,
        /// Lines scrolled.
        scroll: u16,
    },
    /// The accounts list.
    Accounts {
        /// Selected row.
        selected: usize,
    },
    /// "Add account" form.
    AddAccount(Box<AccountForm>),
    /// Account removal confirmation.
    ConfirmRemoveAccount {
        /// The agent.
        agent: AgentKind,
        /// The account.
        name: String,
        /// Tokens that list it.
        tokens: usize,
    },
    /// The projects list.
    Projects {
        /// Selected row.
        selected: usize,
    },
    /// "Add project" or "edit project" form.
    Project(Box<ProjectForm>),
    /// Project removal confirmation.
    ConfirmRemoveProject {
        /// The project.
        name: String,
        /// Tokens bound to it.
        tokens: usize,
    },
    /// The bridges saved by `hashi connect`.
    Remotes {
        /// Selected row.
        selected: usize,
    },
    /// "Connect to a bridge" form.
    Connect(Box<ConnectForm>),
    /// Bridge removal confirmation.
    ConfirmRemoveRemote {
        /// The bridge's name.
        name: String,
    },
    /// The audit log.
    Audit {
        /// Rows scrolled.
        scroll: usize,
    },
    /// What `hashi doctor` checks.
    Doctor,
    /// The report popup.
    Report(Box<ReportForm>),
    /// A report was written here.
    ReportSaved {
        /// Where.
        path: String,
        /// Whether prompt excerpts are in it.
        include_prompts: bool,
    },
    /// The port and bind address form.
    Listen(Box<ListenForm>),
    /// Confirmation for a bind that exposes the API in plain text.
    ConfirmListen {
        /// The form, to go back to.
        form: Box<ListenForm>,
        /// The address asked for.
        addr: std::net::SocketAddr,
        /// The warning shown.
        warning: String,
    },
    /// Confirmation before the dashboard steps aside for `hashi update`.
    ConfirmUpdate,
    /// Requests for access and for more.
    AccessRequests {
        /// Selected row.
        selected: usize,
        /// Decided and expired requests too, as `hashi requests list --all`.
        all: bool,
    },
    /// The accept popup, prefilled with what was asked.
    AcceptRequest(Box<super::access::AcceptForm>),
    /// The decline popup, with an optional reason.
    DeclineRequest(Box<super::access::DeclineForm>),
    /// A freshly opened ask-access link. Like an invite, the URL is built
    /// from the current base URL each time, so it turns public with a
    /// tunnel. The code is shown only here.
    AccessLink {
        /// The link code.
        code: String,
        /// Hours until it closes by itself.
        hours: u32,
        /// The project new requests are for.
        project: Option<String>,
    },
}

/// Work finished in the background, picked up on the next tick.
#[derive(Debug, Clone)]
pub enum Done {
    /// `hashi connect`: the name saved under, or why it failed.
    Connected(Result<String, String>),
    /// `hashi ask --remote`: the bridge asked and its answer or error.
    Answer {
        /// The bridge.
        remote: String,
        /// The answer, or why there is none.
        result: Result<String, String>,
    },
    /// `hashi request status`: what checking each outgoing request found.
    RequestStatus(Result<Vec<crate::access_client::Checked>, String>),
}

/// Public tunnel state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelState {
    /// Not requested.
    Off,
    /// Waiting for cloudflared.
    Starting,
    /// Serving at this URL.
    Up(String),
    /// Failed with this reason.
    Down(String),
}

/// One line in the activity feed.
#[derive(Debug, Clone)]
pub struct FeedItem {
    /// When it happened (Unix ms).
    pub at: i64,
    /// Leading symbol.
    pub glyph: &'static str,
    /// Symbol colour.
    pub color: Rgb,
    /// Message.
    pub text: String,
}

/// The whole UI model.
#[derive(Debug)]
pub struct App {
    /// Server handles.
    pub state: AppState,
    /// Active tab.
    pub tab: Tab,
    /// Animation counter, bumped every tick.
    pub tick: u64,
    /// Global counters.
    pub stats: Stats,
    /// Counters per agent.
    pub agent_stats: HashMap<AgentKind, Stats>,
    /// Recent requests, newest first. Prompts and errors are cut short and
    /// responses left out; [`App::selected_request`] has the full row.
    pub recent: Vec<Request>,
    /// All tokens.
    pub tokens: Vec<Token>,
    /// Today's and this month's use, by token id. Read once per refresh
    /// while the Tokens view shows, the only place it appears.
    pub usage: HashMap<String, TokenUsage>,
    /// Invites not yet claimed or expired, newest first.
    pub invites: Vec<Invite>,
    /// Accounts added for each agent, besides its default one.
    pub accounts: HashMap<AgentKind, usize>,
    /// Memory, run slots and admission state, from the resource guard.
    pub resources: ResourceSnapshot,
    /// Activity feed, newest first.
    pub feed: VecDeque<FeedItem>,
    /// Selection in the requests table.
    pub requests_table: TableState,
    /// Selection in the tokens table.
    pub tokens_table: TableState,
    /// Selection in the pending invites table.
    pub invites_table: TableState,
    /// Which table in the Tokens view has focus.
    pub tokens_focus: TokensFocus,
    /// Scroll offset of the request detail pane.
    pub detail_scroll: u16,
    /// Active modal.
    pub popup: Option<Popup>,
    /// Transient message and when it was raised.
    pub toast: Option<(String, Instant)>,
    /// Tunnel status.
    pub tunnel: TunnelState,
    /// A newer release, such as `0.3.0`, when the update check found one.
    pub update: Option<String>,
    /// Set when the user asks to quit.
    pub should_quit: bool,
    /// Freeze spinners and other animation (`HASHI_REDUCED_MOTION=1`).
    pub reduced_motion: bool,
    /// The request detail pane, kept between frames.
    pub detail_cache: super::ui::DetailCache,
    pub(crate) last_refresh: Option<Instant>,
    /// The selected request in full, for the detail pane.
    pub(crate) detail: Option<Request>,
    /// Selected row of the Commands view, an index into [`Action::ALL`].
    pub commands_selected: usize,
    /// An install `enter` asked for, run by the event loop with the
    /// terminal attached.
    pub pending_command: Option<PendingCommand>,
    /// Where `cloudflared` is, if installed.
    pub cloudflared: Option<std::path::PathBuf>,
    /// Where `tailscale` is, if installed.
    pub tailscale_bin: Option<std::path::PathBuf>,
    /// Tailscale's `BackendState` from the last background probe.
    tailscale_state: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// The whole answer of the last `tailscale status --json`, for the doctor.
    pub(crate) tailscale_self: Shared<Option<Result<crate::tailscale::SelfStatus, String>>>,
    /// Where `crush` is, if installed.
    pub crush: Option<std::path::PathBuf>,
    /// Background work that has finished, waiting for the next tick.
    pub(crate) done: Shared<Vec<Done>>,
    /// Requests for access waiting for the owner: the Commands tab's badge.
    pub access_pending: usize,
    /// What the last `hashi request status` check found, for the Bridges
    /// popup.
    pub request_checks: Option<Vec<crate::access_client::Checked>>,
    /// Whether a request status check is running.
    pub request_checking: bool,
    /// What views read between refreshes; see [`super::cache`].
    pub(crate) cache: super::cache::FrameCache,
}

/// State a background task fills in for the dashboard.
pub type Shared<T> = std::sync::Arc<std::sync::Mutex<T>>;

const FEED_CAP: usize = 500;
const RECENT_LIMIT: u32 = 300;

impl App {
    /// Creates the model. `bootstrap` is an admin secret to reveal on first run.
    #[must_use]
    pub fn new(state: AppState, bootstrap: Option<String>, tunnel: bool) -> Self {
        let mut app = Self {
            state,
            tab: Tab::Overview,
            tick: 0,
            stats: Stats::default(),
            agent_stats: HashMap::new(),
            recent: Vec::new(),
            tokens: Vec::new(),
            usage: HashMap::new(),
            invites: Vec::new(),
            accounts: HashMap::new(),
            resources: ResourceSnapshot::default(),
            feed: VecDeque::new(),
            requests_table: TableState::default().with_selected(Some(0)),
            tokens_table: TableState::default().with_selected(Some(0)),
            invites_table: TableState::default().with_selected(Some(0)),
            tokens_focus: TokensFocus::Tokens,
            detail_scroll: 0,
            popup: bootstrap.map(|secret| Popup::Reveal {
                title: "Your admin token".to_owned(),
                secret,
            }),
            toast: None,
            tunnel: if tunnel {
                TunnelState::Starting
            } else {
                TunnelState::Off
            },
            update: None,
            should_quit: false,
            reduced_motion: std::env::var("HASHI_REDUCED_MOTION").is_ok_and(|v| v != "0"),
            detail_cache: super::ui::DetailCache::default(),
            last_refresh: None,
            detail: None,
            commands_selected: 0,
            pending_command: None,
            cloudflared: None,
            tailscale_bin: None,
            tailscale_state: std::sync::Arc::default(),
            tailscale_self: std::sync::Arc::default(),
            crush: None,
            done: std::sync::Arc::default(),
            access_pending: 0,
            request_checks: None,
            request_checking: false,
            cache: super::cache::FrameCache::default(),
        };
        app.refresh();
        app
    }

    /// Reloads data from the store.
    pub fn refresh(&mut self) {
        let store = &self.state.store;
        let loaded = (|| {
            let stats = store.stats()?;
            // Only the agent cards read these, one per shown agent.
            let mut per = HashMap::new();
            for a in self.shown_agents() {
                per.insert(a, store.agent_stats(a)?);
            }
            let now = now_ms();
            let tokens = store.list_tokens()?;
            let invites: Vec<Invite> = store
                .list_invites()?
                .into_iter()
                .filter(|i| i.status(now) == "pending")
                .collect();
            Ok::<_, crate::store::StoreError>((
                stats,
                per,
                store.recent_requests_brief(RECENT_LIMIT)?,
                tokens,
                invites,
            ))
        })();
        // The tokens' use, account usage, projects and the rest are read
        // by the first frame that shows them.
        self.cache.clear();
        match loaded {
            Ok((stats, per, recent, tokens, invites)) => {
                self.stats = stats;
                self.agent_stats = per;
                self.recent = recent;
                self.tokens = tokens;
                self.invites = invites;
                clamp(&mut self.requests_table, self.recent.len());
                clamp(&mut self.tokens_table, self.tokens.len());
                clamp(&mut self.invites_table, self.invites.len());
                if self.invites.is_empty() {
                    self.tokens_focus = TokensFocus::Tokens;
                }
                self.load_detail();
            }
            Err(e) => self.notify(format!("refresh failed: {e}")),
        }
        if let Ok(pending) = crate::access::pending(&self.state.store) {
            self.access_pending = pending.len();
        }
        if let Ok(list) = self.state.store.list_accounts() {
            let mut counts = HashMap::new();
            for a in list.iter().filter(|a| a.kind != acct::AccountKind::Default) {
                *counts.entry(a.agent).or_insert(0) += 1;
            }
            self.accounts = counts;
        }
        self.resources = self.state.guard.snapshot();
        self.refresh_helpers();
        self.last_refresh = Some(Instant::now());
    }

    /// Whether something on screen changes by itself: a running request
    /// (spinner and elapsed time), the tunnel spinner, a toast waiting to
    /// expire or the cursor blink in a form. When this is false the
    /// dashboard only redraws on input, events and data refreshes.
    #[must_use]
    pub fn animating(&self) -> bool {
        self.stats.running > 0
            || self
                .recent
                .iter()
                .any(|r| r.status == crate::store::RequestStatus::Running)
            || matches!(self.tunnel, TunnelState::Starting)
            || self.toast.is_some()
            || self.request_checking
            || self.popup.as_ref().is_some_and(Popup::animates)
    }

    /// Advances animations and refreshes data periodically. Returns whether
    /// the screen needs a redraw.
    pub fn on_tick(&mut self) -> bool {
        let animating = self.animating();
        if animating && !self.reduced_motion {
            self.tick = self.tick.wrapping_add(1);
        }
        // Always pick up finished work: a spinner waiting for it animates,
        // so it must not short-circuit the check.
        let done = self.take_done();
        let mut redraw = animating || done;
        if self
            .last_refresh
            .is_none_or(|t| t.elapsed() >= Duration::from_millis(750))
        {
            self.refresh();
            redraw = true;
        }
        if self
            .toast
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(4))
        {
            self.toast = None;
            redraw = true;
        }
        redraw
    }

    /// Folds a bus event into the model.
    pub fn on_event(&mut self, event: Event) {
        let item = match event {
            Event::Queued { id, agent, token } => FeedItem {
                at: now_ms(),
                glyph: "◌",
                color: theme::MIST,
                text: format!(
                    "{} queued for {} by {token}",
                    short(&id),
                    agent.display_name()
                ),
            },
            Event::Started { id, agent } => FeedItem {
                at: now_ms(),
                glyph: theme::agent_glyph(agent),
                color: theme::agent_color(agent),
                text: format!("{} picked up {}", agent.display_name(), short(&id)),
            },
            Event::Finished {
                id,
                agent,
                ok: true,
                duration_ms,
                ..
            } => FeedItem {
                at: now_ms(),
                glyph: "✓",
                color: theme::MINT,
                text: format!(
                    "{} answered {} in {}",
                    agent.display_name(),
                    short(&id),
                    crate::util::human_duration(Duration::from_millis(duration_ms))
                ),
            },
            Event::Finished {
                id,
                agent,
                ok: false,
                error,
                ..
            } => FeedItem {
                at: now_ms(),
                glyph: "✗",
                color: theme::ROSE,
                text: format!(
                    "{} failed {}: {}",
                    agent.display_name(),
                    short(&id),
                    truncate(&one_line(error.as_deref().unwrap_or("unknown error")), 120)
                ),
            },
            Event::Held { reason, detail } => FeedItem {
                at: now_ms(),
                glyph: "!",
                color: theme::AMBER,
                text: format!("held: {reason}, queued work waits ({detail})"),
            },
            Event::TunnelReady(url) => {
                self.tunnel = TunnelState::Up(url.clone());
                FeedItem {
                    at: now_ms(),
                    glyph: "⌁",
                    color: theme::VIOLET,
                    text: format!("tunnel live at {url}"),
                }
            }
            Event::TunnelDown(reason) => {
                // A deliberate stop, or a switch already under way, is not a
                // failure: the runtime knows which it is.
                self.tunnel = self
                    .runtime_route_state()
                    .unwrap_or_else(|| TunnelState::Down(reason.clone()));
                FeedItem {
                    at: now_ms(),
                    glyph: "!",
                    color: theme::AMBER,
                    text: format!("tunnel: {reason}"),
                }
            }
            Event::ApprovalNeeded(url) => {
                if self.popup.is_none() {
                    self.popup = Some(Popup::Approval { url: url.clone() });
                } else {
                    self.notify("Tailscale needs your approval: see Logs");
                }
                FeedItem {
                    at: now_ms(),
                    glyph: "!",
                    color: theme::AMBER,
                    text: format!("approve in your browser: {url}"),
                }
            }
            Event::UpdateAvailable { version, .. } => {
                let text = format!("update v{version} is available: run hashi update");
                self.update = Some(version);
                FeedItem {
                    at: now_ms(),
                    glyph: "↑",
                    color: theme::ORCHID,
                    text,
                }
            }
            ev @ Event::AccountUsage { .. } => {
                let Some((item, toast)) = super::usage::feed(&ev) else {
                    return;
                };
                if let Some(toast) = toast {
                    self.notify(toast);
                }
                item
            }
            Event::AccessRequest { kind, name, .. } => {
                // The text is made safe to draw; the refresh below updates
                // the Commands tab's badge.
                let text = super::access::toast_text(kind, &name);
                self.notify(format!("{text}: see Commands, Access requests"));
                FeedItem {
                    at: now_ms(),
                    glyph: "?",
                    color: theme::ORCHID,
                    text,
                }
            }
            Event::Log(text) => FeedItem {
                at: now_ms(),
                glyph: "·",
                color: theme::MIST,
                text,
            },
        };
        self.feed.push_front(item);
        self.feed.truncate(FEED_CAP);
        // Finishing work changes counters; show it immediately.
        self.last_refresh = None;
    }

    /// Handles a key press.
    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        // The key may add or remove an account, a project or a bridge.
        self.cache.clear_after_key();
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        if let Some(popup) = self.popup.take() {
            self.popup = self.on_popup_key(popup, key);
            return;
        }
        match key.code {
            KeyCode::Char('q') => {
                // Quitting stops the bridge, so ask first when work would be lost.
                let pending = self.stats.running + self.stats.queued;
                if pending > 0 {
                    self.popup = Some(Popup::ConfirmQuit { pending });
                } else {
                    self.should_quit = true;
                }
            }
            KeyCode::Char('?') => self.popup = Some(Popup::Help),
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => self.set_tab(self.tab.shift(1)),
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                self.set_tab(self.tab.shift(-1))
            }
            KeyCode::Char(c @ '1'..='5') => {
                let i = usize::from(u8::try_from(c).unwrap_or(b'1') - b'1');
                self.set_tab(Tab::ALL[i]);
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::PageDown | KeyCode::Char('J') => {
                self.detail_scroll = self.detail_scroll.saturating_add(5)
            }
            KeyCode::PageUp | KeyCode::Char('K') => {
                self.detail_scroll = self.detail_scroll.saturating_sub(5)
            }
            KeyCode::Char('c') => {
                let url = self.state.runtime.base_url();
                self.copy(&url, "URL copied");
            }
            KeyCode::Char('r') => {
                self.refresh();
                self.notify("refreshed");
            }
            KeyCode::Char('t') => self.go_public(),
            KeyCode::Char('a') => self.open_ask(None),
            KeyCode::Char('e')
                if self.tab == Tab::Tokens && self.tokens_focus == TokensFocus::Tokens =>
            {
                self.open_edit_token();
            }
            KeyCode::Enter if self.tab == Tab::Commands => self.run_action(self.selected_action()),
            KeyCode::Char('n') if matches!(self.tab, Tab::Tokens | Tab::Commands) => {
                self.popup = Some(Popup::NewToken(TokenForm::new(self.project_names())));
            }
            KeyCode::Char('s') if matches!(self.tab, Tab::Tokens | Tab::Commands) => {
                self.popup = Some(Popup::Share(ShareForm::new(self.project_names())));
            }
            KeyCode::Char('i') if self.tab == Tab::Tokens => {
                if self.invites.is_empty() {
                    self.notify("no pending invites");
                } else {
                    self.tokens_focus = match self.tokens_focus {
                        TokensFocus::Tokens => TokensFocus::Invites,
                        TokensFocus::Invites => TokensFocus::Tokens,
                    };
                }
            }
            KeyCode::Char('x')
                if self.tab == Tab::Tokens && self.tokens_focus == TokensFocus::Invites =>
            {
                if let Some(inv) = self.selected_invite() {
                    self.popup = Some(Popup::ConfirmCancelInvite {
                        id: inv.id.clone(),
                        label: inv.label.clone(),
                    });
                }
            }
            KeyCode::Char('x') if self.tab == Tab::Requests => match self.selected_request() {
                Some(r) if !r.status.is_terminal() => {
                    self.popup = Some(Popup::ConfirmCancelRequest {
                        id: r.id.clone(),
                        token: r.token_name.clone(),
                    });
                }
                Some(_) => self.notify("only queued or running requests can be cancelled"),
                None => {}
            },
            KeyCode::Char('p')
                if self.tab == Tab::Tokens && self.tokens_focus == TokensFocus::Tokens =>
            {
                self.toggle_pause();
            }
            KeyCode::Char('d')
                if self.tab == Tab::Tokens && self.tokens_focus == TokensFocus::Tokens =>
            {
                if let Some(t) = self.selected_token() {
                    self.popup = Some(Popup::ConfirmRevoke {
                        id: t.id.clone(),
                        name: t.name.clone(),
                    });
                }
            }
            _ => {}
        }
    }

    fn on_popup_key(&mut self, popup: Popup, key: KeyEvent) -> Option<Popup> {
        match popup {
            Popup::Help => None,
            popup @ (Popup::EditToken(_)
            | Popup::Ask(_)
            | Popup::RemoteAnswer { .. }
            | Popup::Accounts { .. }
            | Popup::AddAccount(_)
            | Popup::ConfirmRemoveAccount { .. }
            | Popup::Projects { .. }
            | Popup::Project(_)
            | Popup::ConfirmRemoveProject { .. }
            | Popup::Remotes { .. }
            | Popup::Connect(_)
            | Popup::ConfirmRemoveRemote { .. }
            | Popup::Audit { .. }
            | Popup::Doctor
            | Popup::Report(_)
            | Popup::ReportSaved { .. }
            | Popup::Listen(_)
            | Popup::ConfirmListen { .. }
            | Popup::ConfirmUpdate
            | Popup::AccessRequests { .. }
            | Popup::AcceptRequest(_)
            | Popup::DeclineRequest(_)
            | Popup::AccessLink { .. }) => self.on_manage_key(popup, key),
            Popup::Reveal { title, secret } => match key.code {
                KeyCode::Char('c') => {
                    self.copy(&secret, "token copied to clipboard");
                    Some(Popup::Reveal { title, secret })
                }
                KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') => None,
                _ => Some(Popup::Reveal { title, secret }),
            },
            Popup::ConfirmQuit { pending } => match key.code {
                KeyCode::Char('y' | 'Y') => {
                    self.should_quit = true;
                    None
                }
                KeyCode::Char('n' | 'N') | KeyCode::Esc => None,
                _ => Some(Popup::ConfirmQuit { pending }),
            },
            Popup::ConfirmRevoke { id, name } => match key.code {
                KeyCode::Char('y' | 'Y') => {
                    self.revoke(&id, &name);
                    None
                }
                KeyCode::Char('n' | 'N') | KeyCode::Esc => None,
                _ => Some(Popup::ConfirmRevoke { id, name }),
            },
            Popup::ConfirmCancelRequest { id, token } => match key.code {
                KeyCode::Char('y' | 'Y') => {
                    self.cancel_request(id);
                    None
                }
                KeyCode::Char('n' | 'N') | KeyCode::Esc => None,
                _ => Some(Popup::ConfirmCancelRequest { id, token }),
            },
            Popup::ConfirmCancelInvite { id, label } => match key.code {
                KeyCode::Char('y' | 'Y') => {
                    self.cancel_invite(&id, &label);
                    None
                }
                KeyCode::Char('n' | 'N') | KeyCode::Esc => None,
                _ => Some(Popup::ConfirmCancelInvite { id, label }),
            },
            Popup::InviteReveal {
                label,
                code,
                link_hours,
            } => {
                match key.code {
                    KeyCode::Char('c') => {
                        let url = self.invite_url(&code);
                        self.copy(&url, "invite link copied to clipboard");
                    }
                    // No popups from here: they would replace the link.
                    KeyCode::Char('t') => self.start_route(PublicRoute::Cloudflare, false),
                    KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') => return None,
                    _ => {}
                }
                Some(Popup::InviteReveal {
                    label,
                    code,
                    link_hours,
                })
            }
            Popup::Install(plan) => match key.code {
                KeyCode::Enter if matches!(plan, crate::install::Plan::Run { .. }) => {
                    self.pending_command = Some(PendingCommand::Install(plan));
                    None
                }
                KeyCode::Enter | KeyCode::Char('c') => {
                    self.copy(&plan.text(), "copied to clipboard");
                    Some(Popup::Install(plan))
                }
                KeyCode::Esc | KeyCode::Char('q') => None,
                _ => Some(Popup::Install(plan)),
            },
            Popup::ConfirmSwitch { from, to } => match key.code {
                KeyCode::Char('y' | 'Y') => {
                    match crate::server::switch_public(&self.state.runtime, &self.state.bus, to) {
                        Ok(()) => {
                            self.tunnel = TunnelState::Starting;
                            self.notify(format!("switching to {}", to.label()));
                        }
                        Err(e) => self.notify(e.to_string()),
                    }
                    None
                }
                KeyCode::Char('n' | 'N') | KeyCode::Esc => None,
                _ => Some(Popup::ConfirmSwitch { from, to }),
            },
            Popup::Approval { url } => match key.code {
                KeyCode::Char('c') => {
                    self.copy(&url, "approval link copied");
                    Some(Popup::Approval { url })
                }
                KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') => None,
                _ => Some(Popup::Approval { url }),
            },
            Popup::Share(mut form) => {
                match key.code {
                    KeyCode::Esc => return None,
                    KeyCode::Enter => return self.create_invite(form),
                    KeyCode::Down | KeyCode::Tab => {
                        form.field = (form.field + 1) % ShareForm::FIELDS
                    }
                    KeyCode::Up | KeyCode::BackTab => {
                        form.field = (form.field + ShareForm::FIELDS - 1) % ShareForm::FIELDS;
                    }
                    KeyCode::Backspace if form.field == 0 => {
                        form.label.pop();
                        form.error = None;
                    }
                    KeyCode::Char(c) if form.field == 0 => {
                        if form.label.chars().count() < 100 && !c.is_control() {
                            form.label.push(c);
                            form.error = None;
                        }
                    }
                    KeyCode::Backspace if form.field == ShareForm::ACCOUNTS => {
                        form.accounts.pop();
                        form.error = None;
                    }
                    KeyCode::Char(c) if form.field == ShareForm::ACCOUNTS => {
                        if form.accounts.chars().count() < ACCOUNTS_CHARS && !c.is_control() {
                            form.accounts.push(c);
                            form.error = None;
                        }
                    }
                    KeyCode::Left => form.step(-1),
                    KeyCode::Right | KeyCode::Char(' ') => form.step(1),
                    _ => {}
                }
                Some(Popup::Share(form))
            }
            Popup::NewToken(mut form) => {
                match key.code {
                    KeyCode::Esc => return None,
                    KeyCode::Enter => return self.create_token(form),
                    KeyCode::Down | KeyCode::Tab => {
                        form.field = (form.field + 1) % TokenForm::FIELDS
                    }
                    KeyCode::Up | KeyCode::BackTab => {
                        form.field = (form.field + TokenForm::FIELDS - 1) % TokenForm::FIELDS;
                    }
                    KeyCode::Backspace if form.field == 0 => {
                        form.name.pop();
                        form.error = None;
                    }
                    KeyCode::Char(c) if form.field == 0 => {
                        if form.name.chars().count() < 100 && !c.is_control() {
                            form.name.push(c);
                            form.error = None;
                        }
                    }
                    KeyCode::Backspace if form.field == TokenForm::ACCOUNTS => {
                        form.accounts.pop();
                        form.error = None;
                    }
                    KeyCode::Char(c) if form.field == TokenForm::ACCOUNTS => {
                        if form.accounts.chars().count() < ACCOUNTS_CHARS && !c.is_control() {
                            form.accounts.push(c);
                            form.error = None;
                        }
                    }
                    KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') => form.toggle(),
                    _ => {}
                }
                Some(Popup::NewToken(form))
            }
        }
    }

    fn create_token(&mut self, mut form: TokenForm) -> Option<Popup> {
        let name = form.name.trim().to_owned();
        if name.is_empty() {
            form.field = 0;
            form.error = Some("Name is required, for example ci-bot".to_owned());
            return Some(Popup::NewToken(form));
        }
        let accounts = match form_accounts(&self.state.store, &form.accounts) {
            Ok(map) => map,
            Err(e) => {
                form.field = TokenForm::ACCOUNTS;
                form.error = Some(e);
                return Some(Popup::NewToken(form));
            }
        };
        let new = NewToken {
            name,
            role: form.role,
            session_mode: form.mode,
            agents: form.agents.agents(),
            expires_in_days: None,
            note: None,
            limits: NewToken::default_limits(form.role),
            accounts,
            project: form.project.selected().map(str::to_owned),
        };
        if let Err(e) = crate::api::check_binding(
            &self.state.store,
            new.project.as_deref(),
            &new.agents,
            &new.accounts,
        ) {
            form.field = TokenForm::PROJECT;
            form.error = Some(e.to_string());
            return Some(Popup::NewToken(form));
        }
        match self.state.store.create_token(&new) {
            Ok((token, secret)) => {
                if form.share_usage
                    && let Err(e) = self.state.store.set_token_share_usage(&token.id, true)
                {
                    self.notify(format!("created, but usage is not shared: {e}"));
                }
                self.state
                    .bus
                    .log(format!("token created: {} ({})", token.name, token.prefix));
                self.refresh();
                Some(Popup::Reveal {
                    title: format!("Token for {}", token.name),
                    secret: secret.raw,
                })
            }
            Err(e) => {
                self.notify(format!("could not create token: {e}"));
                None
            }
        }
    }

    fn create_invite(&mut self, mut form: ShareForm) -> Option<Popup> {
        let label = form.label.trim().to_owned();
        if label.is_empty() {
            form.field = 0;
            form.error = Some("Label is required, for example alice".to_owned());
            return Some(Popup::Share(form));
        }
        let accounts = match form_accounts(&self.state.store, &form.accounts) {
            Ok(map) => map,
            Err(e) => {
                form.field = ShareForm::ACCOUNTS;
                form.error = Some(e);
                return Some(Popup::Share(form));
            }
        };
        let new = form.to_new_invite(label, accounts);
        if let Err(e) = crate::api::check_binding(
            &self.state.store,
            new.project.as_deref(),
            &new.agents,
            &new.accounts,
        ) {
            form.field = ShareForm::PROJECT;
            form.error = Some(e.to_string());
            return Some(Popup::Share(form));
        }
        match self.state.store.create_invite(&new) {
            Ok((invite, code)) => {
                if form.share_usage
                    && let Err(e) = self.state.store.set_invite_share_usage(&invite.id, true)
                {
                    self.notify(format!("created, but usage is not shared: {e}"));
                }
                self.audit("invite.create", &invite.label, link_long(new.link_hours));
                self.state
                    .bus
                    .log(format!("invite created: {}", invite.label));
                self.refresh();
                Some(Popup::InviteReveal {
                    label: invite.label,
                    code,
                    link_hours: new.link_hours,
                })
            }
            Err(e) => {
                self.notify(format!("could not create invite: {e}"));
                None
            }
        }
    }

    /// The names of the projects, for the project selector of the forms.
    pub(crate) fn project_names(&self) -> Vec<String> {
        self.state
            .store
            .list_projects()
            .map(|l| l.into_iter().map(|p| p.name).collect())
            .unwrap_or_default()
    }

    fn cancel_invite(&mut self, id: &str, label: &str) {
        match self.state.store.cancel_invite(id) {
            Ok(true) => {
                self.audit("invite.cancel", label, "");
                self.state.bus.log(format!("invite cancelled: {label}"));
                self.notify(format!("cancelled invite for {label}"));
            }
            Ok(false) => self.notify("invite already claimed or expired"),
            Err(e) => self.notify(format!("cancel failed: {e}")),
        }
        self.refresh();
    }

    /// Cancels a request through [`crate::queue::cancel`]. Stopping a run
    /// waits for its process tree to go, so it runs as a task and reports
    /// through the event feed.
    fn cancel_request(&mut self, id: String) {
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            self.notify("cannot cancel: the server is not running");
            return;
        };
        let state = self.state.clone();
        rt.spawn(async move {
            use crate::queue::{CancelBy, CancelOutcome, cancel};
            match cancel(&state, &id, CancelBy::Other("dashboard")).await {
                Ok(CancelOutcome::Cancelled(_)) => {}
                Ok(CancelOutcome::Finished(_)) => state.bus.log("request already finished"),
                Ok(CancelOutcome::NotFound) => state.bus.log("request no longer exists"),
                Err(e) => state.bus.log(format!("cancel failed: {e}")),
            }
        });
        self.notify("cancelling request");
    }

    fn toggle_pause(&mut self) {
        let Some(mut tk) = self.selected_token().cloned() else {
            return;
        };
        let now = now_ms();
        if !tk.paused && tk.is_admin() && tk.status(now) == TokenStatus::Active {
            let others = self
                .tokens
                .iter()
                .filter(|o| o.id != tk.id && o.is_admin() && o.status(now) == TokenStatus::Active)
                .count();
            if others == 0 {
                self.notify("cannot pause the last active admin token");
                return;
            }
        }
        tk.paused = !tk.paused;
        let detail = if tk.paused { "paused" } else { "resumed" };
        match self.state.store.save_token_policy(&tk) {
            Ok(()) => {
                if !tk.paused {
                    self.state.dispatcher.wake_all();
                }
                self.audit("token.update", &tk.name, detail);
                self.state.bus.log(format!("token {detail}: {}", tk.name));
                self.notify(format!("{detail} {}", tk.name));
            }
            Err(e) => self.notify(format!("could not update {}: {e}", tk.name)),
        }
        self.refresh();
    }

    pub(crate) fn audit(&mut self, action: &str, target: &str, detail: &str) {
        if let Err(e) = self.state.store.audit("dashboard", action, target, detail) {
            self.notify(format!("audit log failed: {e}"));
        }
    }

    pub(crate) fn selected_token(&self) -> Option<&Token> {
        self.tokens_table
            .selected()
            .and_then(|i| self.tokens.get(i))
    }

    /// The invite highlighted in the pending invites table.
    #[must_use]
    pub fn selected_invite(&self) -> Option<&Invite> {
        self.invites_table
            .selected()
            .and_then(|i| self.invites.get(i))
    }

    fn revoke(&mut self, id: &str, name: &str) {
        match self.state.store.revoke_token(id) {
            Ok(Revoke::Revoked(token_id)) => {
                self.state.bus.log(format!("token revoked: {name}"));
                self.notify(format!("revoked {name}"));
                // Stop its runs first, then delete the workspace once they
                // are gone. The dashboard must not block, so the wait runs
                // as a task.
                let stopped = self.state.dispatcher.cancel_token_runs(&token_id);
                let home = self.state.store.data_dir();
                let bus = self.state.bus.clone();
                let name = name.to_owned();
                let cleanup = move || {
                    if let Some(home) = home
                        && let Err(e) = crate::server::remove_workspace(&home, &token_id)
                    {
                        bus.log(format!("could not delete the workspace of {name}: {e}"));
                    }
                };
                match tokio::runtime::Handle::try_current() {
                    Ok(rt) => {
                        rt.spawn(async move {
                            stopped.await;
                            let _ = tokio::task::spawn_blocking(cleanup).await;
                        });
                    }
                    // No runtime means no workers either, so nothing runs.
                    Err(_) => cleanup(),
                }
            }
            Ok(Revoke::LastAdmin) => self.notify("cannot revoke the last admin token"),
            Ok(Revoke::NotFound) => self.notify("token already gone"),
            Err(e) => self.notify(format!("revoke failed: {e}")),
        }
        self.refresh();
    }

    pub(crate) fn set_tab(&mut self, tab: Tab) {
        if tab == Tab::Commands && self.tab != Tab::Commands {
            self.probe_tailscale();
        }
        self.tab = tab;
        self.detail_scroll = 0;
    }

    fn move_selection(&mut self, by: isize) {
        if self.tab == Tab::Commands {
            self.commands_selected = self
                .commands_selected
                .saturating_add_signed(by)
                .min(Action::ALL.len() - 1);
            return;
        }
        let (table, len) = match self.tab {
            Tab::Requests => (&mut self.requests_table, self.recent.len()),
            Tab::Tokens => match self.tokens_focus {
                TokensFocus::Tokens => (&mut self.tokens_table, self.tokens.len()),
                TokensFocus::Invites => (&mut self.invites_table, self.invites.len()),
            },
            _ => return,
        };
        if len == 0 {
            return;
        }
        let cur = table.selected().unwrap_or(0).cast_signed();
        let next = (cur + by).clamp(0, len.cast_signed() - 1);
        table.select(Some(next.cast_unsigned()));
        self.detail_scroll = 0;
        if self.tab == Tab::Requests {
            self.load_detail();
        }
    }

    /// Loads the selected request with its full prompt and response.
    pub(crate) fn load_detail(&mut self) {
        let id = self
            .requests_table
            .selected()
            .and_then(|i| self.recent.get(i))
            .map(|r| r.id.clone());
        self.detail = id.and_then(|id| self.state.store.get_request(&id).ok().flatten());
    }

    /// The claim link for an invite code, against the current base URL.
    #[must_use]
    pub fn invite_url(&self, code: &str) -> String {
        format!("{}/claim/{code}", self.state.runtime.base_url())
    }

    /// Whether links made now only work on this machine.
    #[must_use]
    pub fn local_only(&self) -> bool {
        self.state.runtime.public_url().is_none()
    }

    /// Starts a quick tunnel on the running server (key `t`).
    pub fn go_public(&mut self) {
        self.start_route(PublicRoute::Cloudflare, true);
    }

    /// Starts `to`. With `popups`, a missing helper opens the install popup
    /// and another active route asks to switch; without (from inside another
    /// popup), both become a toast.
    pub fn start_route(&mut self, to: PublicRoute, popups: bool) {
        let rt = &self.state.runtime;
        let started = match to {
            PublicRoute::Cloudflare => crate::server::go_public(rt, &self.state.bus),
            PublicRoute::Tailscale(mode) => crate::server::go_tailscale(rt, &self.state.bus, mode),
        };
        match started {
            Ok(()) => {
                self.tunnel = TunnelState::Starting;
                self.notify(format!("starting {}", to.label()));
            }
            Err(StartError::OtherRoute(_)) if popups => {
                if let Some(from) = crate::server::active_route(rt) {
                    self.popup = Some(Popup::ConfirmSwitch { from, to });
                }
            }
            Err(StartError::NotInstalled) if popups => self.open_install(Helper::Cloudflared),
            Err(StartError::TailscaleNotInstalled) if popups => {
                self.open_install(Helper::Tailscale);
            }
            Err(StartError::NotInstalled) => self.notify(format!(
                "cloudflared is not installed: {}",
                crate::tunnel::install_hint()
            )),
            Err(e) => self.notify(e.to_string()),
        }
    }

    /// Stops whichever route is public.
    pub fn stop_public(&mut self) {
        match crate::server::stop_public(&self.state.runtime) {
            Some(route) => self.notify(format!("stopping {}", route.label())),
            None => self.notify("nothing is public"),
        }
    }

    pub(crate) fn open_install(&mut self, helper: Helper) {
        self.popup = Some(Popup::Install(crate::install::plan_here(helper)));
    }

    /// The executable for `helper`, as the server would find it. Refreshed
    /// with the rest of the data.
    #[must_use]
    pub fn helper_path(&self, helper: Helper) -> Option<&std::path::Path> {
        match helper {
            Helper::Cloudflared => self.cloudflared.as_deref(),
            Helper::Tailscale => self.tailscale_bin.as_deref(),
            // An agent, not a helper of the dashboard.
            Helper::Crush => self.crush.as_deref(),
        }
    }

    fn refresh_helpers(&mut self) {
        let rt = &self.state.runtime;
        let cf_custom = rt.tunnel.settings().and_then(|s| s.bin.clone());
        let ts_custom = rt.tailscale.settings().and_then(|s| s.bin.clone());
        self.cloudflared = crate::install::locate(Helper::Cloudflared, cf_custom.as_deref());
        self.tailscale_bin = crate::install::locate(Helper::Tailscale, ts_custom.as_deref());
        self.crush = crate::install::locate(Helper::Crush, None);
    }

    /// Asks `tailscale status --json` in the background whether this device
    /// is signed in, for the Commands view. Needs a Tokio runtime and an
    /// installed `tailscale`.
    pub(crate) fn probe_tailscale(&mut self) {
        let Some(bin) = self.tailscale_bin.clone() else {
            return;
        };
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let slot = std::sync::Arc::clone(&self.tailscale_state);
        let whole = std::sync::Arc::clone(&self.tailscale_self);
        rt.spawn(async move {
            let status = crate::tailscale::self_status(&bin)
                .await
                .map_err(|e| e.to_string());
            let state = match &status {
                Ok(me) => me.backend_state.clone(),
                Err(e) => format!("unknown ({e})"),
            };
            if let Ok(mut s) = slot.lock() {
                *s = Some(state);
            }
            if let Ok(mut s) = whole.lock() {
                *s = Some(status);
            }
        });
    }

    /// Tailscale's `BackendState` from the last probe, such as `Running`.
    #[must_use]
    pub fn tailscale_backend(&self) -> Option<String> {
        self.tailscale_state.lock().ok().and_then(|s| s.clone())
    }

    /// Runs a Commands view row.
    pub fn run_action(&mut self, action: Action) {
        match action {
            Action::Cloudflare => self.go_public(),
            Action::TailscaleServe => self.start_route(PublicRoute::Tailscale(Mode::Serve), true),
            Action::TailscaleFunnel => {
                self.start_route(PublicRoute::Tailscale(Mode::Funnel), true);
            }
            Action::StopPublic => self.stop_public(),
            Action::NewInvite => {
                self.popup = Some(Popup::Share(ShareForm::new(self.project_names())));
            }
            Action::NewToken => {
                self.popup = Some(Popup::NewToken(TokenForm::new(self.project_names())));
            }
            Action::InstallCloudflared | Action::InstallTailscale | Action::InstallCrush => {
                let helper = match action {
                    Action::InstallCloudflared => Helper::Cloudflared,
                    Action::InstallTailscale => Helper::Tailscale,
                    _ => Helper::Crush,
                };
                match self.helper_path(helper) {
                    Some(path) => {
                        let msg = format!("{helper} is already installed at {}", path.display());
                        self.notify(msg);
                    }
                    None => self.open_install(helper),
                }
            }
            Action::CheckUpdate => self.check_update(),
            Action::CopyUrl => {
                let url = self.state.runtime.base_url();
                self.copy(&url, "URL copied");
            }
            Action::Help => self.popup = Some(Popup::Help),
            Action::Ask
            | Action::AccessRequests
            | Action::Accounts
            | Action::Projects
            | Action::Bridges
            | Action::Connect
            | Action::Listen
            | Action::InstallUpdate
            | Action::Doctor
            | Action::Report
            | Action::Audit => self.run_manage_action(action),
        }
    }

    pub(crate) fn check_update(&mut self) {
        if let Some(v) = &self.update {
            let msg = format!("v{v} is available: run hashi update in a terminal");
            self.notify(msg);
            return;
        }
        if crate::update::opted_out() {
            self.notify(format!(
                "the update check is off ({})",
                crate::update::OPT_OUT_ENV
            ));
            return;
        }
        // Only a running server has a home and a network to ask; the test
        // dashboard has neither.
        let home = self.state.store.data_dir();
        let (true, Some(home)) = (self.state.runtime.tunnel.settings().is_some(), home) else {
            self.notify("run hashi update --check in a terminal");
            return;
        };
        let bus = self.state.bus.clone();
        // A plain thread, as for the startup check: the request must never
        // hold up shutdown.
        let spawned = std::thread::Builder::new()
            .name("hashi-update-check".to_owned())
            .spawn(move || {
                let found = crate::update::check(
                    &home,
                    crate::update::LATEST_URL,
                    crate::util::now_ms(),
                    true,
                );
                match found {
                    Ok(r)
                        if crate::update::is_newer(
                            &r.version,
                            &crate::update::current_version(),
                        ) =>
                    {
                        bus.emit(Event::UpdateAvailable {
                            version: r.version.to_string(),
                            url: r.url,
                        });
                    }
                    Ok(r) => bus.log(format!("hashi is up to date (latest v{})", r.version)),
                    Err(e) => bus.log(format!("could not check for updates: {e:#}")),
                }
            });
        if spawned.is_err() {
            self.notify("could not start the update check");
            return;
        }
        self.notify("checking for updates");
    }

    /// The row selected in the Commands view.
    #[must_use]
    pub fn selected_action(&self) -> Action {
        Action::ALL[self.commands_selected.min(Action::ALL.len() - 1)]
    }

    /// Takes the install plan `enter` asked to run, for the event loop to
    /// run with the terminal attached.
    pub fn take_pending_command(&mut self) -> Option<PendingCommand> {
        self.pending_command.take()
    }

    /// Called by the event loop when a command run with the terminal
    /// attached has finished. `result` is a line to show, or the error.
    pub fn command_finished(&mut self, done: &PendingCommand, result: Result<String, String>) {
        self.cache.clear();
        self.refresh_helpers();
        match done {
            PendingCommand::Install(plan) => {
                let helper = plan.helper();
                match result {
                    Ok(_) if self.helper_path(helper).is_some() => {
                        self.notify(format!("{helper} is installed"));
                        self.state.bus.log(format!("{helper} installed"));
                    }
                    Ok(_) => self.notify(format!(
                        "{helper} is still not found; open a new terminal or restart hashi"
                    )),
                    Err(e) => self.notify(format!("install failed: {e}")),
                }
                if helper == Helper::Tailscale {
                    self.probe_tailscale();
                }
            }
            PendingCommand::SignIn { agent, name, .. } => {
                match result {
                    Ok(state) => {
                        self.state
                            .bus
                            .log(format!("{agent}:{name} signed in: {state}"));
                        self.notify(format!("{agent}:{name} signed in: {state}"));
                    }
                    Err(e) => self.notify(format!("sign-in failed: {e}")),
                }
                self.popup = Some(Popup::Accounts { selected: 0 });
            }
            PendingCommand::Update { .. } => match result {
                Ok(msg) => {
                    self.state.bus.log(msg.clone());
                    self.notify(msg);
                }
                Err(e) => self.notify(format!("update failed: {e}")),
            },
        }
        self.last_refresh = None;
    }

    /// The tunnel state the runtime reports, when it settles the question:
    /// a route starting or up, or (on a real server) both routes off.
    fn runtime_route_state(&self) -> Option<TunnelState> {
        use crate::tunnel::Status;
        let rt = &self.state.runtime;
        let statuses = [rt.tailscale.status(), rt.tunnel.status()];
        for s in &statuses {
            match s {
                Status::Up(url) => return Some(TunnelState::Up(url.clone())),
                Status::Starting => return Some(TunnelState::Starting),
                _ => {}
            }
        }
        let real = rt.tunnel.settings().is_some() || rt.tailscale.settings().is_some();
        (real && statuses.iter().all(|s| *s == Status::Off)).then_some(TunnelState::Off)
    }

    /// Which route the header's public URL belongs to, when it is Tailscale.
    #[must_use]
    pub fn tailscale_mode_up(&self) -> Option<Mode> {
        let ts = &self.state.runtime.tailscale;
        matches!(ts.status(), crate::tunnel::Status::Up(_))
            .then(|| ts.mode())
            .flatten()
    }

    /// Shows a transient message in the footer.
    pub fn notify(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    pub(crate) fn copy(&mut self, text: &str, done: &str) {
        match super::clipboard::copy(text) {
            Ok(()) => self.notify(done),
            Err(e) => self.notify(format!("copy failed: {e}")),
        }
    }

    /// The request highlighted in the requests table, in full when loaded.
    #[must_use]
    pub fn selected_request(&self) -> Option<&Request> {
        let brief = self
            .requests_table
            .selected()
            .and_then(|i| self.recent.get(i))?;
        match &self.detail {
            Some(full) if full.id == brief.id => Some(full),
            _ => Some(brief),
        }
    }

    /// How long the oldest running request for `agent` has been running, in ms.
    #[must_use]
    pub fn running_for(&self, agent: AgentKind) -> Option<u64> {
        let now = now_ms();
        self.recent
            .iter()
            .filter(|r| r.agent == agent && r.status == crate::store::RequestStatus::Running)
            .filter_map(|r| r.started_at)
            .min()
            .and_then(|s| u64::try_from(now - s).ok())
    }

    /// Agents to show on the overview: the installed ones, or Claude Code and
    /// Codex when none are, so a fresh install still shows what to expect.
    #[must_use]
    pub fn shown_agents(&self) -> Vec<AgentKind> {
        let installed: Vec<AgentKind> = AgentKind::ALL
            .into_iter()
            .filter(|a| {
                self.state
                    .runtime
                    .agents
                    .iter()
                    .any(|i| i.agent == *a && i.available)
            })
            .collect();
        if installed.is_empty() {
            vec![AgentKind::Claude, AgentKind::Codex]
        } else {
            installed
        }
    }

    /// Recent completed durations for `agent`, oldest first, for sparklines.
    #[must_use]
    pub fn latencies(&self, agent: AgentKind, n: usize) -> Vec<u64> {
        let mut v: Vec<u64> = self
            .recent
            .iter()
            .filter(|r| r.agent == agent)
            .filter_map(Request::duration_ms)
            .filter_map(|d| u64::try_from(d).ok())
            .take(n)
            .collect();
        v.reverse();
        v
    }

    /// Finished requests per `bucket` over the last `buckets` windows, oldest first.
    #[must_use]
    pub fn throughput(&self, bucket: Duration, buckets: usize) -> Vec<u64> {
        let mut out = vec![0_u64; buckets];
        let now = now_ms();
        let width = i64::try_from(bucket.as_millis()).unwrap_or(1).max(1);
        for done in self.recent.iter().filter_map(|r| r.completed_at) {
            let age = (now - done) / width;
            if let Ok(age) = usize::try_from(age)
                && age < buckets
            {
                out[buckets - 1 - age] += 1;
            }
        }
        out
    }
}

fn clamp(table: &mut TableState, len: usize) {
    match (table.selected(), len) {
        (_, 0) => table.select(None),
        (None, _) => table.select(Some(0)),
        (Some(i), n) if i >= n => table.select(Some(n - 1)),
        _ => {}
    }
}

fn cycle(i: usize, len: usize, by: isize) -> usize {
    let n = len.max(1).cast_signed();
    (i.cast_signed() + by).rem_euclid(n).cast_unsigned()
}

fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}
