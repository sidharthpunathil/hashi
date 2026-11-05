//! SQLite persistence for tokens, requests and agent sessions.
//!
//! The connection lives behind a `std::sync::Mutex`. Every query here is a
//! short indexed lookup or write on a local file, so critical sections are
//! microseconds long and the lock is never held across an `.await`. Queries
//! that run often (usage checks, the dashboard list) never read prompt or
//! response bodies, so their cost does not grow with what users send.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde::Serialize;

mod accounts;
mod audit;
mod cancel;
mod invites;

pub use accounts::{AccountState, AccountStatus};
pub use audit::AuditEntry;
pub use invites::{INVITE_PREFIX, Invite, InviteState, NewInvite, Redeemed};

use crate::accounts::{self as acct, AccountMap};
use crate::agent::{AgentKind, Usage};
use crate::token::{self, GeneratedToken, Limits, Role, SessionMode, Token};
use crate::util::{month_start_utc, now_ms};

/// Storage errors.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Underlying SQLite failure.
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// The mutex was poisoned by a panic in another thread.
    #[error("database lock poisoned")]
    Poisoned,
    /// A stored value could not be decoded.
    #[error("corrupt record: {0}")]
    Corrupt(String),
}

type Result<T> = std::result::Result<T, StoreError>;

/// Lifecycle of a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RequestStatus {
    /// Waiting for a worker.
    Queued,
    /// An agent is working on it.
    Running,
    /// Finished with a response.
    Completed,
    /// Finished with an error.
    Failed,
}

impl RequestStatus {
    /// Stable lowercase identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    /// `true` once the request will not change again.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

impl FromStr for RequestStatus {
    type Err = StoreError;

    fn from_str(s: &str) -> Result<Self> {
        Ok(match s {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            other => return Err(StoreError::Corrupt(format!("status `{other}`"))),
        })
    }
}

/// A prompt and, eventually, its answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// Request id (UUID v4).
    pub id: String,
    /// Owning token.
    pub token_id: String,
    /// Owning token's name, denormalised for display.
    pub token_name: String,
    /// Target agent.
    pub agent: AgentKind,
    /// Model override.
    pub model: Option<String>,
    /// The prompt.
    pub message: String,
    /// Whether this request continues the token's conversation.
    pub stateful: bool,
    /// Current status.
    pub status: RequestStatus,
    /// Final answer, once completed.
    pub response: Option<String>,
    /// Failure reason, once failed.
    pub error: Option<String>,
    /// Token accounting, once completed.
    pub usage: Option<Usage>,
    /// Creation time, Unix ms.
    pub created_at: i64,
    /// Start time, Unix ms.
    pub started_at: Option<i64>,
    /// Finish time, Unix ms.
    pub completed_at: Option<i64>,
    /// The account that ran it (`default` for the owner's own login), once
    /// a worker has picked one. Shown to admins only.
    pub account: Option<String>,
    /// Accounts this request already ran into a limit on, so a failover
    /// never tries one twice.
    pub accounts_tried: Vec<String>,
}

impl Request {
    /// Time spent running, in ms, once finished.
    #[must_use]
    pub fn duration_ms(&self) -> Option<i64> {
        Some(self.completed_at? - self.started_at?)
    }
}

/// Input for [`Store::create_token`].
#[derive(Debug, Clone)]
pub struct NewToken {
    /// Human label.
    pub name: String,
    /// Permission level.
    pub role: Role,
    /// Conversation behaviour.
    pub session_mode: SessionMode,
    /// Agents the token may use.
    pub agents: Vec<AgentKind>,
    /// Lifetime in days, or `None` for no expiry.
    pub expires_in_days: Option<u32>,
    /// Who or what the token is for.
    pub note: Option<String>,
    /// Usage caps.
    pub limits: Limits,
    /// Accounts per agent, in order of preference. An agent without an
    /// entry uses its default account.
    pub accounts: AccountMap,
}

/// Upper bound on stored tokens, enforced by the API and by invite redemption.
pub const MAX_TOKENS: usize = 100;

/// Error stored on a queued request whose token expired before it started.
pub const EXPIRED_BEFORE_START: &str = "the token expired before this request started";

/// `maxPending` given to non-admin tokens created without any limits.
pub const DEFAULT_MAX_PENDING: u32 = 3;

impl NewToken {
    /// Limits for a token created without any: non-admin tokens may have
    /// [`DEFAULT_MAX_PENDING`] prompts in flight, admins are unlimited.
    #[must_use]
    pub fn default_limits(role: Role) -> Limits {
        match role {
            Role::Admin => Limits::default(),
            Role::User => Limits {
                max_pending: Some(DEFAULT_MAX_PENDING),
                ..Limits::default()
            },
        }
    }
}

/// Queue slots kept free for admin tokens: 10% of `queue_limit`, at least 5,
/// but never more than half the queue.
#[must_use]
pub const fn admin_reserve(queue_limit: u64) -> u64 {
    let tenth = queue_limit.div_ceil(10);
    let reserve = if tenth < 5 { 5 } else { tenth };
    let half = queue_limit / 2;
    if reserve < half { reserve } else { half }
}

/// A per-token cap that refused a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cap {
    /// `dailyRequests`.
    DailyRequests,
    /// `monthlyRequests`.
    MonthlyRequests,
    /// `dailyBudgetUsd`.
    DailyBudgetUsd,
    /// `monthlyBudgetUsd`.
    MonthlyBudgetUsd,
}

/// Result of [`Store::insert_request_checked`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Queued.
    Accepted,
    /// A request or budget cap for the current UTC day or month is used up.
    LimitReached(Cap),
    /// The token already has `pending` prompts in flight, at its cap of `max`.
    TokenBusy {
        /// Prompts queued or running for this token.
        pending: u64,
        /// The token's `maxPending`.
        max: u32,
    },
    /// The queue has no room for this token's role.
    QueueFull,
}

/// What one maintenance pass removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pruned {
    /// Finished requests.
    pub requests: usize,
    /// Audit records.
    pub audit: usize,
    /// Claimed or expired invites.
    pub invites: usize,
}

/// Audit records and spent invites are kept at least this many days.
pub const MIN_AUDIT_DAYS: u32 = 90;

/// Interruptions after which a request is failed instead of re-queued.
pub const MAX_INTERRUPTS: i64 = 3;

/// A database whose live data is at most this size is converted to
/// incremental auto-vacuum (which needs a full `VACUUM`) automatically.
const AUTO_VACUUM_CONVERT_BYTES: i64 = 128 * 1024 * 1024;

/// Characters of the prompt kept by [`Store::recent_requests_brief`].
pub const BRIEF_MESSAGE_CHARS: u32 = 200;

/// A token's use in the current UTC day and month.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    /// Prompts since the start of the UTC day.
    pub today_requests: u64,
    /// Prompts since the start of the UTC month.
    pub month_requests: u64,
    /// Reported spend since the start of the UTC day.
    pub today_cost_usd: f64,
    /// Reported spend since the start of the UTC month.
    pub month_cost_usd: f64,
    /// Prompts queued or running now.
    pub pending: u64,
}

/// Input for [`Store::insert_request`].
#[derive(Debug, Clone, Copy)]
pub struct NewRequest<'a> {
    /// Request id.
    pub id: &'a str,
    /// Owning token.
    pub token: &'a Token,
    /// Target agent.
    pub agent: AgentKind,
    /// Model override.
    pub model: Option<&'a str>,
    /// The prompt.
    pub message: &'a str,
}

/// Outcome of a revoke attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Revoke {
    /// The token with this id was deleted.
    Revoked(String),
    /// No token matched.
    NotFound,
    /// Refused: it is the last admin token.
    LastAdmin,
}

/// Aggregate counters for dashboards.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    /// Number of tokens.
    pub tokens: u64,
    /// All requests ever recorded (subject to retention).
    pub total: u64,
    /// Waiting requests.
    pub queued: u64,
    /// Running requests.
    pub running: u64,
    /// Successful requests.
    pub completed: u64,
    /// Failed requests.
    pub failed: u64,
}

/// Thread-safe handle to the database.
#[derive(Debug)]
pub struct Store {
    conn: Mutex<Connection>,
}

/// Version 1: the 0.1.x schema. Statements are idempotent so databases created
/// before versioning (which report `user_version` 0) upgrade cleanly.
const SCHEMA_V1: &str = r"
CREATE TABLE IF NOT EXISTS tokens (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    hash         TEXT NOT NULL UNIQUE,
    prefix       TEXT NOT NULL,
    role         TEXT NOT NULL,
    session_mode TEXT NOT NULL,
    agents       TEXT NOT NULL,
    expires_at   INTEGER,
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER
);
CREATE TABLE IF NOT EXISTS requests (
    id            TEXT PRIMARY KEY,
    token_id      TEXT NOT NULL REFERENCES tokens(id) ON DELETE CASCADE,
    token_name    TEXT NOT NULL,
    agent         TEXT NOT NULL,
    model         TEXT,
    message       TEXT NOT NULL,
    stateful      INTEGER NOT NULL DEFAULT 0,
    status        TEXT NOT NULL,
    response      TEXT,
    error         TEXT,
    input_tokens  INTEGER,
    output_tokens INTEGER,
    cost_usd      REAL,
    created_at    INTEGER NOT NULL,
    started_at    INTEGER,
    completed_at  INTEGER
);
CREATE TABLE IF NOT EXISTS sessions (
    token_id         TEXT NOT NULL REFERENCES tokens(id) ON DELETE CASCADE,
    agent            TEXT NOT NULL,
    agent_session_id TEXT NOT NULL,
    updated_at       INTEGER NOT NULL,
    PRIMARY KEY (token_id, agent)
);
CREATE INDEX IF NOT EXISTS idx_requests_queue ON requests(agent, status, created_at);
CREATE INDEX IF NOT EXISTS idx_requests_created ON requests(created_at);
";

/// Version 2: token policy, invites and the audit log.
const SCHEMA_V2: &str = r"
ALTER TABLE tokens ADD COLUMN paused INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tokens ADD COLUMN note TEXT;
ALTER TABLE tokens ADD COLUMN daily_requests INTEGER;
ALTER TABLE tokens ADD COLUMN monthly_requests INTEGER;
ALTER TABLE tokens ADD COLUMN daily_budget_usd REAL;
ALTER TABLE tokens ADD COLUMN monthly_budget_usd REAL;
ALTER TABLE tokens ADD COLUMN max_pending INTEGER;
CREATE INDEX IF NOT EXISTS idx_requests_token ON requests(token_id, created_at);
CREATE TABLE IF NOT EXISTS invites (
    id                    TEXT PRIMARY KEY,
    hash                  TEXT NOT NULL UNIQUE,
    label                 TEXT NOT NULL,
    session_mode          TEXT NOT NULL,
    agents                TEXT NOT NULL,
    token_expires_in_days INTEGER,
    daily_requests        INTEGER,
    monthly_requests      INTEGER,
    daily_budget_usd      REAL,
    monthly_budget_usd    REAL,
    max_pending           INTEGER,
    expires_at            INTEGER NOT NULL,
    created_at            INTEGER NOT NULL,
    claimed_at            INTEGER,
    token_id              TEXT REFERENCES tokens(id) ON DELETE SET NULL
);
CREATE TABLE IF NOT EXISTS audit (
    id     INTEGER PRIMARY KEY AUTOINCREMENT,
    at     INTEGER NOT NULL,
    actor  TEXT NOT NULL,
    action TEXT NOT NULL,
    target TEXT NOT NULL,
    detail TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_audit_at ON audit(at);
";

/// Version 3: usage checks served from an index alone, a status index for
/// the queue limit, and a crash counter per request. Incremental
/// auto-vacuum is switched on after the migration by [`Store::compact`].
const SCHEMA_V3: &str = r"
ALTER TABLE requests ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;
DROP INDEX IF EXISTS idx_requests_token;
CREATE INDEX IF NOT EXISTS idx_requests_usage ON requests(token_id, created_at, status, cost_usd);
CREATE INDEX IF NOT EXISTS idx_requests_status ON requests(status, agent);
";

/// Version 4: several accounts per agent. Account state, the accounts each
/// token and invite may use, the account that ran each request (and those
/// it already hit a limit on), and sessions kept per account. The sessions
/// table is rebuilt because its primary key gains the account.
const SCHEMA_V4: &str = r"
CREATE TABLE IF NOT EXISTS accounts (
    agent         TEXT NOT NULL,
    name          TEXT NOT NULL,
    kind          TEXT NOT NULL,
    disabled      INTEGER NOT NULL DEFAULT 0,
    limited_until INTEGER,
    last_used     INTEGER,
    created_at    INTEGER NOT NULL,
    PRIMARY KEY (agent, name)
);
ALTER TABLE tokens ADD COLUMN accounts TEXT NOT NULL DEFAULT '{}';
ALTER TABLE invites ADD COLUMN accounts TEXT NOT NULL DEFAULT '{}';
ALTER TABLE requests ADD COLUMN account TEXT;
ALTER TABLE requests ADD COLUMN accounts_tried TEXT NOT NULL DEFAULT '';
CREATE INDEX IF NOT EXISTS idx_requests_account ON requests(agent, account, started_at);
CREATE TABLE sessions_v4 (
    token_id         TEXT NOT NULL REFERENCES tokens(id) ON DELETE CASCADE,
    agent            TEXT NOT NULL,
    account          TEXT NOT NULL DEFAULT 'default',
    agent_session_id TEXT NOT NULL,
    updated_at       INTEGER NOT NULL,
    PRIMARY KEY (token_id, agent, account)
);
INSERT INTO sessions_v4 (token_id, agent, account, agent_session_id, updated_at)
    SELECT token_id, agent, 'default', agent_session_id, updated_at FROM sessions;
DROP TABLE sessions;
ALTER TABLE sessions_v4 RENAME TO sessions;
";

const MIGRATIONS: [&str; 4] = [SCHEMA_V1, SCHEMA_V2, SCHEMA_V3, SCHEMA_V4];

/// Usage of token `?1` since day start `?2` and month start `?3`. Every
/// column it touches is in `idx_requests_usage`.
const TOKEN_USAGE_SQL: &str = "SELECT
         COUNT(CASE WHEN created_at >= ?2 THEN 1 END),
         COUNT(CASE WHEN created_at >= ?3 THEN 1 END),
         COALESCE(SUM(CASE WHEN created_at >= ?2 THEN cost_usd END), 0.0),
         COALESCE(SUM(CASE WHEN created_at >= ?3 THEN cost_usd END), 0.0),
         COUNT(CASE WHEN status IN ('queued', 'running') THEN 1 END)
     FROM requests
     WHERE token_id = ?1 AND (created_at >= ?3 OR status IN ('queued', 'running'))";

const TOKEN_COLUMNS: &str = "id, name, prefix, role, session_mode, agents, expires_at, created_at, \
     last_used_at, paused, note, daily_requests, monthly_requests, daily_budget_usd, \
     monthly_budget_usd, max_pending, accounts";

const REQUEST_COLUMNS: &str = "id, token_id, token_name, agent, model, message, stateful, status, \
     response, error, input_tokens, output_tokens, cost_usd, created_at, started_at, completed_at, \
     account, accounts_tried";

impl Store {
    /// Opens (or creates) the database at `path`.
    ///
    /// # Errors
    ///
    /// Fails if the file cannot be opened or migrated.
    pub fn open(path: &Path) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    /// Opens a throwaway in-memory database, for tests.
    ///
    /// # Errors
    ///
    /// Fails if SQLite cannot allocate the database.
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "busy_timeout", 5_000)?;
        let before = migrate(&mut conn)?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        if before < 3 {
            // Best effort: another process may hold the database, and the
            // hourly maintenance pass tries again.
            if let Err(e) = store.compact() {
                tracing::warn!(error = %e, "could not enable incremental auto-vacuum yet");
            }
        }
        Ok(store)
    }

    /// The directory holding the database file, or `None` in memory.
    #[must_use]
    pub fn data_dir(&self) -> Option<PathBuf> {
        let conn = self.conn().ok()?;
        let path = conn.path().filter(|p| !p.is_empty())?;
        Path::new(path).parent().map(Path::to_path_buf)
    }

    fn conn(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| StoreError::Poisoned)
    }

    // ── tokens ─────────────────────────────────────────────────────────────

    /// Creates a token and returns its record plus the one-time raw secret.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn create_token(&self, new: &NewToken) -> Result<(Token, GeneratedToken)> {
        insert_token(&*self.conn()?, new)
    }

    /// Resolves a raw bearer secret to an unexpired token and records the use.
    /// Paused tokens are returned; callers decide what a paused token may do.
    ///
    /// # Errors
    ///
    /// Fails on database errors. Unknown or expired tokens yield `Ok(None)`.
    pub fn authenticate(&self, raw: &str) -> Result<Option<Token>> {
        let now = now_ms();
        Ok(self.authenticate_any(raw)?.filter(|t| !t.is_expired(now)))
    }

    /// Like [`Store::authenticate`], but also returns expired tokens, so they
    /// can still see why they stopped working.
    ///
    /// # Errors
    ///
    /// Fails on database errors. Unknown tokens yield `Ok(None)`.
    pub fn authenticate_any(&self, raw: &str) -> Result<Option<Token>> {
        let digest = token::hash(raw);
        let conn = self.conn()?;
        let sql = format!("SELECT {TOKEN_COLUMNS} FROM tokens WHERE hash = ?1");
        let Some(t) = conn
            .query_row(&sql, [&digest], row_to_token)
            .optional()?
            .transpose()?
        else {
            return Ok(None);
        };
        let now = now_ms();
        conn.execute(
            "UPDATE tokens SET last_used_at = ?1 WHERE id = ?2",
            params![now, t.id],
        )?;
        Ok(Some(Token {
            last_used_at: Some(now),
            ..t
        }))
    }

    /// Fetches a token by id.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn get_token(&self, id: &str) -> Result<Option<Token>> {
        let sql = format!("SELECT {TOKEN_COLUMNS} FROM tokens WHERE id = ?1");
        self.conn()?
            .query_row(&sql, [id], row_to_token)
            .optional()?
            .transpose()
    }

    /// Writes a token's editable fields: name, note, pause, expiry, limits
    /// and accounts.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn save_token_policy(&self, t: &Token) -> Result<()> {
        let l = t.limits;
        self.conn()?.execute(
            "UPDATE tokens SET name = ?1, note = ?2, paused = ?3, expires_at = ?4,
                 daily_requests = ?5, monthly_requests = ?6, daily_budget_usd = ?7,
                 monthly_budget_usd = ?8, max_pending = ?9, accounts = ?11
             WHERE id = ?10",
            params![
                t.name,
                t.note,
                t.paused,
                t.expires_at,
                l.daily_requests,
                l.monthly_requests,
                l.daily_budget_usd,
                l.monthly_budget_usd,
                l.max_pending,
                t.id,
                acct::encode(&t.accounts),
            ],
        )?;
        Ok(())
    }

    /// A token's use in the UTC day and month containing `now_ms`. Served
    /// entirely from `idx_requests_usage`, so it never reads request bodies.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn token_usage(&self, token_id: &str, now_ms: i64) -> Result<TokenUsage> {
        usage_in(&*self.conn()?, token_id, now_ms)
    }

    /// Lists all tokens, newest first.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn list_tokens(&self) -> Result<Vec<Token>> {
        let conn = self.conn()?;
        let sql = format!("SELECT {TOKEN_COLUMNS} FROM tokens ORDER BY created_at DESC");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], row_to_token)?;
        rows.map(|r| r.map_err(StoreError::from).and_then(|t| t))
            .collect()
    }

    /// Number of admin tokens.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn admin_count(&self) -> Result<u64> {
        let n: i64 = self.conn()?.query_row(
            "SELECT COUNT(*) FROM tokens WHERE role = 'admin'",
            [],
            |r| r.get(0),
        )?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    /// Deletes a token by id or unique display prefix. Refuses to remove the last admin.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn revoke_token(&self, id_or_prefix: &str) -> Result<Revoke> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let matches: Vec<(String, String)> = {
            let mut stmt =
                tx.prepare("SELECT id, role FROM tokens WHERE id = ?1 OR prefix = ?1 LIMIT 2")?;
            stmt.query_map([id_or_prefix], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        let [(id, role)] = matches.as_slice() else {
            return Ok(Revoke::NotFound);
        };
        if role == Role::Admin.as_str() {
            let admins: i64 = tx.query_row(
                "SELECT COUNT(*) FROM tokens WHERE role = 'admin'",
                [],
                |r| r.get(0),
            )?;
            if admins <= 1 {
                return Ok(Revoke::LastAdmin);
            }
        }
        tx.execute("DELETE FROM tokens WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(Revoke::Revoked(id.clone()))
    }

    // ── requests ───────────────────────────────────────────────────────────

    /// Enqueues a request without checking any cap.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn insert_request(&self, new: NewRequest<'_>) -> Result<()> {
        insert_request_row(&*self.conn()?, new)
    }

    /// Enqueues a request only if every cap allows it: the token's request
    /// and budget caps for the current UTC day and month, its `maxPending`,
    /// and the queue limit. Non-admin tokens may fill the queue up to
    /// `queue_limit - admin_reserve`; admins may use all of it. The usage
    /// read and the insert run in one IMMEDIATE transaction, so concurrent
    /// submits cannot overshoot any cap. Caps are checked in the order the
    /// variants of [`Cap`] are listed, then `maxPending`, then the queue.
    ///
    /// Budget caps compare the cost already reported by finished requests.
    /// Queued and running prompts have no cost yet and nothing is reserved
    /// for them, so a token can still go over a budget by what its
    /// in-flight prompts (bounded by `maxPending`) end up costing.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn insert_request_checked(
        &self,
        new: NewRequest<'_>,
        limits: &Limits,
        queue_limit: u64,
        admin_reserve: u64,
    ) -> Result<Admission> {
        let now = now_ms();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let usage = usage_in(&tx, &new.token.id, now)?;
        let over = |max: Option<u32>, used: u64| max.is_some_and(|m| used >= u64::from(m));
        let spent = |max: Option<f64>, used: f64| max.is_some_and(|m| used >= m);
        let cap = if over(limits.daily_requests, usage.today_requests) {
            Some(Cap::DailyRequests)
        } else if over(limits.monthly_requests, usage.month_requests) {
            Some(Cap::MonthlyRequests)
        } else if spent(limits.daily_budget_usd, usage.today_cost_usd) {
            Some(Cap::DailyBudgetUsd)
        } else if spent(limits.monthly_budget_usd, usage.month_cost_usd) {
            Some(Cap::MonthlyBudgetUsd)
        } else {
            None
        };
        if let Some(cap) = cap {
            return Ok(Admission::LimitReached(cap));
        }
        if let Some(max) = limits.max_pending
            && usage.pending >= u64::from(max)
        {
            return Ok(Admission::TokenBusy {
                pending: usage.pending,
                max,
            });
        }
        let total: i64 = tx.query_row(
            "SELECT COUNT(*) FROM requests WHERE status IN ('queued', 'running')",
            [],
            |r| r.get(0),
        )?;
        let cap = if new.token.is_admin() {
            queue_limit
        } else {
            queue_limit.saturating_sub(admin_reserve)
        };
        if u64::try_from(total).unwrap_or(0) >= cap {
            return Ok(Admission::QueueFull);
        }
        insert_request_row(&tx, new)?;
        tx.commit()?;
        Ok(Admission::Accepted)
    }

    /// Whether `agent` has a queued request a worker should look at: one
    /// whose token is not paused, or whose token has expired (so it can be
    /// failed). A paused token's prompts wait in the queue until it resumes.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn has_queued(&self, agent: AgentKind) -> Result<bool> {
        Ok(self.conn()?.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM requests r JOIN tokens t ON t.id = r.token_id
                 WHERE r.status = 'queued' AND r.agent = ?1
                   AND (t.paused = 0 OR (t.expires_at IS NOT NULL AND t.expires_at <= ?2)))",
            params![agent.as_str(), now_ms()],
            |r| r.get(0),
        )?)
    }

    /// Fails every queued request for `agent` whose token has expired, with
    /// [`EXPIRED_BEFORE_START`], and returns their ids. Such a prompt never
    /// runs, even if the expiry is extended later.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn fail_expired_queued(&self, agent: AgentKind) -> Result<Vec<String>> {
        let now = now_ms();
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "UPDATE requests SET status = 'failed', error = ?1, completed_at = ?2
             WHERE status = 'queued' AND agent = ?3 AND token_id IN
                 (SELECT id FROM tokens WHERE expires_at IS NOT NULL AND expires_at <= ?2)
             RETURNING id",
        )?;
        let rows = stmt.query_map(params![EXPIRED_BEFORE_START, now, agent.as_str()], |r| {
            r.get(0)
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Atomically moves the oldest queued request for `agent` to `running`.
    /// Requests whose token is paused or expired are skipped and stay
    /// queued; [`Store::fail_expired_queued`] ends the expired ones.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn claim_next(&self, agent: AgentKind) -> Result<Option<Request>> {
        let sql = format!(
            "UPDATE requests SET status = 'running', started_at = ?1
             WHERE id = (SELECT r.id FROM requests r JOIN tokens t ON t.id = r.token_id
                         WHERE r.status = 'queued' AND r.agent = ?2 AND t.paused = 0
                           AND (t.expires_at IS NULL OR t.expires_at > ?1)
                         ORDER BY r.created_at, r.rowid LIMIT 1)
             RETURNING {REQUEST_COLUMNS}"
        );
        self.conn()?
            .query_row(&sql, params![now_ms(), agent.as_str()], row_to_request)
            .optional()?
            .transpose()
    }

    /// Whether request `id` still exists and is running. Workers poll this,
    /// so a revoke from another process (which deletes the row) stops the
    /// run.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn is_running(&self, id: &str) -> Result<bool> {
        Ok(self.conn()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM requests WHERE id = ?1 AND status = 'running')",
            [id],
            |r| r.get(0),
        )?)
    }

    /// Requests of `token_id` that are running now.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn running_for_token(&self, token_id: &str) -> Result<u64> {
        let n: i64 = self.conn()?.query_row(
            "SELECT COUNT(*) FROM requests WHERE token_id = ?1 AND status = 'running'",
            [token_id],
            |r| r.get(0),
        )?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    /// Marks a request as completed.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn complete_request(&self, id: &str, response: &str, usage: Option<Usage>) -> Result<()> {
        let to_i64 = |n: u64| i64::try_from(n).unwrap_or(i64::MAX);
        self.conn()?.execute(
            "UPDATE requests SET status = 'completed', response = ?1, input_tokens = ?2,
                 output_tokens = ?3, cost_usd = ?4, completed_at = ?5 WHERE id = ?6",
            params![
                response,
                usage.map(|u| to_i64(u.input_tokens)),
                usage.map(|u| to_i64(u.output_tokens)),
                usage.and_then(|u| u.cost_usd),
                now_ms(),
                id,
            ],
        )?;
        Ok(())
    }

    /// Marks a request as failed.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn fail_request(&self, id: &str, error: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE requests SET status = 'failed', error = ?1, completed_at = ?2 WHERE id = ?3",
            params![error, now_ms(), id],
        )?;
        Ok(())
    }

    /// Fetches one request.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn get_request(&self, id: &str) -> Result<Option<Request>> {
        let sql = format!("SELECT {REQUEST_COLUMNS} FROM requests WHERE id = ?1");
        self.conn()?
            .query_row(&sql, [id], row_to_request)
            .optional()?
            .transpose()
    }

    /// Most recent requests, newest first, with full bodies. The dashboard
    /// list uses [`Store::recent_requests_brief`] instead.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn recent_requests(&self, limit: u32) -> Result<Vec<Request>> {
        let sql = format!(
            "SELECT {REQUEST_COLUMNS} FROM requests ORDER BY created_at DESC, rowid DESC LIMIT ?1"
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([limit], row_to_request)?;
        rows.map(|r| r.map_err(StoreError::from).and_then(|t| t))
            .collect()
    }

    /// Most recent requests, newest first, for a list view: the prompt and
    /// error are cut to [`BRIEF_MESSAGE_CHARS`] characters in SQL and the
    /// response is left out. Load one in full with [`Store::get_request`].
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn recent_requests_brief(&self, limit: u32) -> Result<Vec<Request>> {
        let sql = format!(
            "SELECT id, token_id, token_name, agent, model, substr(message, 1, {n}), stateful,
                 status, NULL, substr(error, 1, {n}), input_tokens, output_tokens, cost_usd,
                 created_at, started_at, completed_at, account, accounts_tried
             FROM requests ORDER BY created_at DESC, rowid DESC LIMIT ?1",
            n = BRIEF_MESSAGE_CHARS
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([limit], row_to_request)?;
        rows.map(|r| r.map_err(StoreError::from).and_then(|t| t))
            .collect()
    }

    /// Most recent failed requests, newest first.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn failed_requests(&self, limit: u32) -> Result<Vec<Request>> {
        let sql = format!(
            "SELECT {REQUEST_COLUMNS} FROM requests WHERE status = 'failed' \
             ORDER BY created_at DESC, rowid DESC LIMIT ?1"
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([limit], row_to_request)?;
        rows.map(|r| r.map_err(StoreError::from).and_then(|t| t))
            .collect()
    }

    /// The migration level the database is at.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .conn()?
            .pragma_query_value(None, "user_version", |r| r.get(0))?)
    }

    /// 1-based position of a queued request within its agent's queue.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn queue_position(&self, req: &Request) -> Result<u64> {
        let n: i64 = self.conn()?.query_row(
            "SELECT COUNT(*) FROM requests WHERE status = 'queued' AND agent = ?1 AND created_at <= ?2",
            params![req.agent.as_str(), req.created_at],
            |r| r.get(0),
        )?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    /// Requests not yet finished, across all agents.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn pending_count(&self) -> Result<u64> {
        let n: i64 = self.conn()?.query_row(
            "SELECT COUNT(*) FROM requests WHERE status IN ('queued', 'running')",
            [],
            |r| r.get(0),
        )?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    /// Re-queues requests that were running when the process last stopped,
    /// and counts the interruption. A request that has now interrupted the
    /// server [`MAX_INTERRUPTS`] times is failed instead, so a prompt that
    /// crashes Hashi cannot run again on every start. Returns how many
    /// requests were touched.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn recover_interrupted(&self) -> Result<usize> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let failed = tx.execute(
            "UPDATE requests SET status = 'failed', attempts = attempts + 1, completed_at = ?1,
                 error = ?2
             WHERE status = 'running' AND attempts + 1 >= ?3",
            params![
                now_ms(),
                format!("stopped after interrupting the server {MAX_INTERRUPTS} times"),
                MAX_INTERRUPTS
            ],
        )?;
        let requeued = tx.execute(
            "UPDATE requests SET status = 'queued', started_at = NULL, attempts = attempts + 1
             WHERE status = 'running'",
            [],
        )?;
        tx.commit()?;
        Ok(failed + requeued)
    }

    /// Puts a running request back in the queue without counting an
    /// interruption, for a clean shutdown.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn requeue(&self, id: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE requests SET status = 'queued', started_at = NULL
             WHERE id = ?1 AND status = 'running'",
            [id],
        )?;
        Ok(())
    }

    /// Deletes finished requests older than `days`.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn prune(&self, days: u32) -> Result<usize> {
        let now = now_ms();
        // Never prune this month's requests: monthly caps are counted from them.
        let cutoff = (now - i64::from(days) * 86_400_000).min(month_start_utc(now));
        Ok(self.conn()?.execute(
            "DELETE FROM requests WHERE status IN ('completed', 'failed') AND completed_at < ?1",
            [cutoff],
        )?)
    }

    /// One maintenance pass: finished requests past `retention_days` (never
    /// the current month), plus audit records and claimed or expired invites
    /// older than `retention_days` or [`MIN_AUDIT_DAYS`], whichever is longer.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn prune_all(&self, retention_days: u32) -> Result<Pruned> {
        let requests = self.prune(retention_days)?;
        let cutoff = now_ms() - i64::from(retention_days.max(MIN_AUDIT_DAYS)) * 86_400_000;
        let audit = self.prune_audit(cutoff)?;
        let invites = self.prune_invites(cutoff)?;
        Ok(Pruned {
            requests,
            audit,
            invites,
        })
    }

    /// Returns free pages to the file system. The first call on a database
    /// without incremental auto-vacuum converts it with a full `VACUUM`, but
    /// only while its live data is small (128 MiB); a larger database stays
    /// as it is until pruning brings it under that size. Returns `true` when
    /// incremental auto-vacuum is on afterwards.
    ///
    /// # Errors
    ///
    /// Fails on database errors, for example when another process holds a
    /// lock.
    pub fn compact(&self) -> Result<bool> {
        let conn = self.conn()?;
        let mode: i64 = conn.pragma_query_value(None, "auto_vacuum", |r| r.get(0))?;
        if mode == 2 {
            // Each step frees one page, so step until it is done.
            let mut stmt = conn.prepare("PRAGMA incremental_vacuum")?;
            let mut rows = stmt.query([])?;
            while rows.next()?.is_some() {}
            return Ok(true);
        }
        let pages: i64 = conn.pragma_query_value(None, "page_count", |r| r.get(0))?;
        let free: i64 = conn.pragma_query_value(None, "freelist_count", |r| r.get(0))?;
        let size: i64 = conn.pragma_query_value(None, "page_size", |r| r.get(0))?;
        if (pages - free).saturating_mul(size) > AUTO_VACUUM_CONVERT_BYTES {
            return Ok(false);
        }
        conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
        conn.execute_batch("VACUUM;")?;
        let mode: i64 = conn.pragma_query_value(None, "auto_vacuum", |r| r.get(0))?;
        Ok(mode == 2)
    }

    /// Moves a request and every audit record `days` into the past.
    #[cfg(test)]
    pub(crate) fn backdate_for_tests(&self, request_id: &str, days: i64) {
        let at = now_ms() - days * 86_400_000;
        let conn = self.conn().unwrap();
        conn.execute(
            "UPDATE requests SET created_at = ?1, started_at = ?1, completed_at = ?1 WHERE id = ?2",
            params![at, request_id],
        )
        .unwrap();
        conn.execute("UPDATE audit SET at = ?1", [at]).unwrap();
    }

    /// Aggregate counters.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn stats(&self) -> Result<Stats> {
        let conn = self.conn()?;
        let tokens: i64 = conn.query_row("SELECT COUNT(*) FROM tokens", [], |r| r.get(0))?;
        let mut stats = Stats {
            tokens: u64::try_from(tokens).unwrap_or(0),
            ..Stats::default()
        };
        let mut stmt = conn.prepare("SELECT status, COUNT(*) FROM requests GROUP BY status")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            let (status, n) = row?;
            let n = u64::try_from(n).unwrap_or(0);
            stats.total += n;
            match status.as_str() {
                "queued" => stats.queued = n,
                "running" => stats.running = n,
                "completed" => stats.completed = n,
                "failed" => stats.failed = n,
                _ => {}
            }
        }
        Ok(stats)
    }

    /// Counters for one agent.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn agent_stats(&self, agent: AgentKind) -> Result<Stats> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT status, COUNT(*) FROM requests WHERE agent = ?1 GROUP BY status")?;
        let rows = stmt.query_map([agent.as_str()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        let mut stats = Stats::default();
        for row in rows {
            let (status, n) = row?;
            let n = u64::try_from(n).unwrap_or(0);
            stats.total += n;
            match status.as_str() {
                "queued" => stats.queued = n,
                "running" => stats.running = n,
                "completed" => stats.completed = n,
                "failed" => stats.failed = n,
                _ => {}
            }
        }
        Ok(stats)
    }

    // ── sessions ───────────────────────────────────────────────────────────

    /// The agent session a stateful token should resume on its default
    /// account, if any.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn session(&self, token_id: &str, agent: AgentKind) -> Result<Option<String>> {
        self.account_session(token_id, agent, acct::DEFAULT)
    }

    /// The agent session a stateful token should resume on `account`. The
    /// conversation lives in the account's own directory, so each account
    /// has its own.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn account_session(
        &self,
        token_id: &str,
        agent: AgentKind,
        account: &str,
    ) -> Result<Option<String>> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT agent_session_id FROM sessions
                 WHERE token_id = ?1 AND agent = ?2 AND account = ?3",
                params![token_id, agent.as_str(), account],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Records the agent session for a token on its default account.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn set_session(&self, token_id: &str, agent: AgentKind, session: &str) -> Result<()> {
        self.set_account_session(token_id, agent, acct::DEFAULT, session)
    }

    /// Records the agent session for a token on `account`.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn set_account_session(
        &self,
        token_id: &str,
        agent: AgentKind,
        account: &str,
        session: &str,
    ) -> Result<()> {
        self.conn()?.execute(
            "INSERT INTO sessions (token_id, agent, account, agent_session_id, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(token_id, agent, account) DO UPDATE SET agent_session_id = ?4,
                 updated_at = ?5",
            params![token_id, agent.as_str(), account, session, now_ms()],
        )?;
        Ok(())
    }

    /// Forgets a token's sessions with `agent`, on every account, so the
    /// next prompt starts fresh.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn clear_session(&self, token_id: &str, agent: AgentKind) -> Result<()> {
        self.conn()?.execute(
            "DELETE FROM sessions WHERE token_id = ?1 AND agent = ?2",
            params![token_id, agent.as_str()],
        )?;
        Ok(())
    }
}

/// A token's use in the UTC day and month containing `now_ms`, read on an
/// existing connection or transaction.
fn usage_in(conn: &Connection, token_id: &str, now_ms: i64) -> Result<TokenUsage> {
    let day = crate::util::day_start_utc(now_ms);
    let month = month_start_utc(now_ms);
    let row = conn.query_row(TOKEN_USAGE_SQL, params![token_id, day, month], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, f64>(2)?,
            r.get::<_, f64>(3)?,
            r.get::<_, i64>(4)?,
        ))
    })?;
    let n = |v: i64| u64::try_from(v).unwrap_or(0);
    Ok(TokenUsage {
        today_requests: n(row.0),
        month_requests: n(row.1),
        today_cost_usd: row.2,
        month_cost_usd: row.3,
        pending: n(row.4),
    })
}

/// Applies pending migrations and returns the version the database was at.
fn migrate(conn: &mut Connection) -> Result<i64> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let done = usize::try_from(version).unwrap_or(0);
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(done) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(
            None,
            "user_version",
            i64::try_from(i + 1).unwrap_or(i64::MAX),
        )?;
        tx.commit()?;
    }
    Ok(version)
}

fn insert_request_row(conn: &Connection, new: NewRequest<'_>) -> Result<()> {
    conn.execute(
        "INSERT INTO requests (id, token_id, token_name, agent, model, message, stateful, status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'queued', ?8)",
        params![
            new.id,
            new.token.id,
            new.token.name,
            new.agent.as_str(),
            new.model,
            new.message,
            new.token.session_mode == SessionMode::Stateful,
            now_ms(),
        ],
    )?;
    Ok(())
}

/// Inserts a token inside an existing connection or transaction.
fn insert_token(conn: &Connection, new: &NewToken) -> Result<(Token, GeneratedToken)> {
    let secret = token::generate();
    let now = now_ms();
    let token = Token {
        id: uuid::Uuid::new_v4().to_string(),
        name: new.name.clone(),
        prefix: secret.prefix.clone(),
        role: new.role,
        session_mode: new.session_mode,
        agents: new.agents.clone(),
        expires_at: new.expires_in_days.map(|d| now + i64::from(d) * 86_400_000),
        created_at: now,
        last_used_at: None,
        paused: false,
        note: new.note.clone(),
        limits: new.limits,
        accounts: new.accounts.clone(),
    };
    let l = token.limits;
    conn.execute(
        "INSERT INTO tokens (id, name, hash, prefix, role, session_mode, agents, expires_at,
             created_at, note, daily_requests, monthly_requests, daily_budget_usd,
             monthly_budget_usd, max_pending, accounts)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            token.id,
            token.name,
            secret.hash,
            token.prefix,
            token.role.as_str(),
            token.session_mode.as_str(),
            token::encode_agents(&token.agents),
            token.expires_at,
            token.created_at,
            token.note,
            l.daily_requests,
            l.monthly_requests,
            l.daily_budget_usd,
            l.monthly_budget_usd,
            l.max_pending,
            acct::encode(&token.accounts),
        ],
    )?;
    Ok((token, secret))
}

fn row_to_token(r: &Row<'_>) -> rusqlite::Result<Result<Token>> {
    let role: String = r.get(3)?;
    let mode: String = r.get(4)?;
    let agents: String = r.get(5)?;
    let accounts: String = r.get(16)?;
    let build = || -> Result<Token> {
        Ok(Token {
            id: r.get(0)?,
            name: r.get(1)?,
            prefix: r.get(2)?,
            role: role.parse().map_err(StoreError::Corrupt)?,
            session_mode: mode.parse().map_err(StoreError::Corrupt)?,
            agents: token::decode_agents(&agents),
            expires_at: r.get(6)?,
            created_at: r.get(7)?,
            last_used_at: r.get(8)?,
            paused: r.get(9)?,
            note: r.get(10)?,
            limits: Limits {
                daily_requests: r.get(11)?,
                monthly_requests: r.get(12)?,
                daily_budget_usd: r.get(13)?,
                monthly_budget_usd: r.get(14)?,
                max_pending: r.get(15)?,
            },
            accounts: acct::decode(&accounts),
        })
    };
    Ok(build())
}

fn row_to_request(r: &Row<'_>) -> rusqlite::Result<Result<Request>> {
    let agent: String = r.get(3)?;
    let status: String = r.get(7)?;
    let input: Option<i64> = r.get(10)?;
    let output: Option<i64> = r.get(11)?;
    let cost: Option<f64> = r.get(12)?;
    let tried: String = r.get(17)?;
    let build = || -> Result<Request> {
        let to_u64 = |n: i64| u64::try_from(n).unwrap_or(0);
        Ok(Request {
            id: r.get(0)?,
            token_id: r.get(1)?,
            token_name: r.get(2)?,
            agent: agent
                .parse()
                .map_err(|e: crate::agent::UnknownAgent| StoreError::Corrupt(e.to_string()))?,
            model: r.get(4)?,
            message: r.get(5)?,
            stateful: r.get(6)?,
            status: status.parse()?,
            response: r.get(8)?,
            error: r.get(9)?,
            usage: match (input, output) {
                (None, None) => None,
                (i, o) => Some(Usage {
                    input_tokens: i.map_or(0, to_u64),
                    output_tokens: o.map_or(0, to_u64),
                    cost_usd: cost,
                }),
            },
            created_at: r.get(13)?,
            started_at: r.get(14)?,
            completed_at: r.get(15)?,
            account: r.get(16)?,
            accounts_tried: tried
                .split(',')
                .filter(|n| !n.is_empty())
                .map(str::to_owned)
                .collect(),
        })
    };
    Ok(build())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    fn new_token(role: Role) -> NewToken {
        NewToken {
            name: "test".into(),
            role,
            session_mode: SessionMode::Stateless,
            agents: AgentKind::ALL.to_vec(),
            expires_in_days: None,
            note: None,
            limits: Limits::default(),
            accounts: AccountMap::new(),
        }
    }

    fn enqueue(s: &Store, t: &Token, agent: AgentKind) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        s.insert_request(NewRequest {
            id: &id,
            token: t,
            agent,
            model: None,
            message: "hi",
        })
        .unwrap();
        id
    }

    #[test]
    fn migrates_v1_database_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.db");
        {
            // A 0.1.x database: v1 schema, user_version never set.
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.execute(
                "INSERT INTO tokens (id, name, hash, prefix, role, session_mode, agents, created_at)
                 VALUES ('t1', 'old', ?1, 'hashi_old', 'admin', 'stateless', 'claude,codex', 1)",
                [token::hash("hashi_old_secret")],
            )
            .unwrap();
        }
        let s = Store::open(&path).unwrap();
        let t = s.authenticate("hashi_old_secret").unwrap().unwrap();
        assert_eq!(
            (t.name.as_str(), t.paused, t.limits),
            ("old", false, Limits::default())
        );
        let version: i64 = s
            .conn()
            .unwrap()
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, 4);
        // Opening again is a no-op.
        drop(s);
        assert!(Store::open(&path).is_ok());
    }

    fn plan(s: &Store, sql: &str) -> String {
        let conn = s.conn().unwrap();
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let rows = stmt
            .query_map(params!["t", 0_i64, 0_i64], |r| r.get::<_, String>(3))
            .unwrap();
        rows.map(|r| r.unwrap()).collect::<Vec<_>>().join("; ")
    }

    #[test]
    fn token_usage_reads_only_the_covering_index() {
        let s = store();
        let p = plan(&s, TOKEN_USAGE_SQL);
        assert!(
            p.contains("USING COVERING INDEX idx_requests_usage"),
            "unexpected plan: {p}"
        );
    }

    #[test]
    fn migrates_v2_database_to_v3_with_attempts_and_auto_vacuum() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v2.db");
        {
            let mut conn = Connection::open(&path).unwrap();
            for sql in [SCHEMA_V1, SCHEMA_V2] {
                let tx = conn.transaction().unwrap();
                tx.execute_batch(sql).unwrap();
                tx.commit().unwrap();
            }
            conn.pragma_update(None, "user_version", 2).unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.schema_version().unwrap(), 4);
        let conn = s.conn().unwrap();
        let mode: i64 = conn
            .pragma_query_value(None, "auto_vacuum", |r| r.get(0))
            .unwrap();
        assert_eq!(mode, 2, "incremental auto-vacuum");
        let attempts: i64 = conn
            .query_row("SELECT COUNT(attempts) FROM requests", [], |r| r.get(0))
            .unwrap();
        assert_eq!(attempts, 0);
    }

    #[test]
    fn migrates_v3_database_to_v4_keeping_sessions_on_the_default_account() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v3.db");
        {
            let mut conn = Connection::open(&path).unwrap();
            for sql in [SCHEMA_V1, SCHEMA_V2, SCHEMA_V3] {
                let tx = conn.transaction().unwrap();
                tx.execute_batch(sql).unwrap();
                tx.commit().unwrap();
            }
            conn.pragma_update(None, "user_version", 3).unwrap();
            conn.execute(
                "INSERT INTO tokens (id, name, hash, prefix, role, session_mode, agents, created_at)
                 VALUES ('t1', 'old', ?1, 'hashi_old', 'user', 'stateful', 'claude', 1)",
                [token::hash("hashi_old_secret")],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO sessions (token_id, agent, agent_session_id, updated_at)
                 VALUES ('t1', 'claude', 'sess-1', 1)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO requests (id, token_id, token_name, agent, message, status, created_at)
                 VALUES ('r1', 't1', 'old', 'claude', 'hi', 'completed', 1)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO invites (id, hash, label, session_mode, agents, expires_at, created_at)
                 VALUES ('i1', 'h', 'bob', 'stateless', 'claude', 9999999999999, 1)",
                [],
            )
            .unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.schema_version().unwrap(), 4);
        let t = s.get_token("t1").unwrap().unwrap();
        assert!(t.accounts.is_empty());
        assert_eq!(
            s.session("t1", AgentKind::Claude).unwrap().as_deref(),
            Some("sess-1")
        );
        assert_eq!(
            s.account_session("t1", AgentKind::Claude, "default")
                .unwrap()
                .as_deref(),
            Some("sess-1")
        );
        let r = s.get_request("r1").unwrap().unwrap();
        assert_eq!((r.account, r.accounts_tried.len()), (None, 0));
        assert!(s.list_invites().unwrap()[0].accounts.is_empty());
        assert!(s.list_accounts().unwrap().is_empty());
        // Deleting the token still cascades to its sessions.
        s.conn()
            .unwrap()
            .execute("DELETE FROM tokens WHERE id = 't1'", [])
            .unwrap();
        assert!(s.session("t1", AgentKind::Claude).unwrap().is_none());
    }

    #[test]
    fn brief_list_cuts_prompts_and_drops_responses() {
        let s = store();
        let (t, _) = s.create_token(&new_token(Role::User)).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let long = "é".repeat(5_000);
        s.insert_request(NewRequest {
            id: &id,
            token: &t,
            agent: AgentKind::Claude,
            model: None,
            message: &long,
        })
        .unwrap();
        s.claim_next(AgentKind::Claude).unwrap();
        s.complete_request(&id, &"r".repeat(50_000), None).unwrap();
        let brief = s.recent_requests_brief(10).unwrap();
        assert_eq!(brief[0].message.chars().count(), 200);
        assert_eq!(brief[0].response, None);
        assert_eq!(brief[0].status, RequestStatus::Completed);
        let full = s.get_request(&id).unwrap().unwrap();
        assert_eq!(
            (full.message.len(), full.response.unwrap().len()),
            (long.len(), 50_000)
        );
    }

    fn checked(s: &Store, t: &Token, max: Option<u32>, limit: u64) -> Admission {
        let limits = Limits {
            max_pending: max,
            ..Limits::default()
        };
        checked_with(s, t, &limits, limit)
    }

    fn checked_with(s: &Store, t: &Token, limits: &Limits, limit: u64) -> Admission {
        let id = uuid::Uuid::new_v4().to_string();
        s.insert_request_checked(
            NewRequest {
                id: &id,
                token: t,
                agent: AgentKind::Claude,
                model: None,
                message: "hi",
            },
            limits,
            limit,
            admin_reserve(limit),
        )
        .unwrap()
    }

    #[test]
    fn checked_insert_enforces_max_pending() {
        let s = store();
        let (t, _) = s.create_token(&new_token(Role::User)).unwrap();
        for _ in 0..2 {
            assert_eq!(checked(&s, &t, Some(2), 100), Admission::Accepted);
        }
        assert_eq!(
            checked(&s, &t, Some(2), 100),
            Admission::TokenBusy { pending: 2, max: 2 }
        );
    }

    #[test]
    fn admins_keep_a_reserved_share_of_the_queue() {
        assert_eq!(
            (
                admin_reserve(100),
                admin_reserve(20),
                admin_reserve(4),
                admin_reserve(1)
            ),
            (10, 5, 2, 0)
        );
        let s = store();
        let (user, _) = s.create_token(&new_token(Role::User)).unwrap();
        let (admin, _) = s.create_token(&new_token(Role::Admin)).unwrap();
        // 20 slots, 5 reserved: users stop at 15, the admin can use the rest.
        for _ in 0..15 {
            assert_eq!(checked(&s, &user, None, 20), Admission::Accepted);
        }
        assert_eq!(checked(&s, &user, None, 20), Admission::QueueFull);
        for _ in 0..5 {
            assert_eq!(checked(&s, &admin, None, 20), Admission::Accepted);
        }
        assert_eq!(checked(&s, &admin, None, 20), Admission::QueueFull);
    }

    #[test]
    fn default_limits_cap_users_only() {
        assert_eq!(
            NewToken::default_limits(Role::User).max_pending,
            Some(DEFAULT_MAX_PENDING)
        );
        assert_eq!(NewToken::default_limits(Role::Admin), Limits::default());
    }

    #[test]
    fn third_interruption_fails_the_request() {
        let s = store();
        let (t, _) = s.create_token(&new_token(Role::User)).unwrap();
        let id = enqueue(&s, &t, AgentKind::Claude);
        for _ in 0..2 {
            s.claim_next(AgentKind::Claude).unwrap().unwrap();
            assert_eq!(s.recover_interrupted().unwrap(), 1);
            assert_eq!(
                s.get_request(&id).unwrap().unwrap().status,
                RequestStatus::Queued
            );
        }
        s.claim_next(AgentKind::Claude).unwrap().unwrap();
        assert_eq!(s.recover_interrupted().unwrap(), 1);
        let r = s.get_request(&id).unwrap().unwrap();
        assert_eq!(r.status, RequestStatus::Failed);
        assert_eq!(
            r.error.as_deref(),
            Some("stopped after interrupting the server 3 times")
        );
        assert!(s.claim_next(AgentKind::Claude).unwrap().is_none());
    }

    #[test]
    fn clean_requeue_does_not_count_an_interruption() {
        let s = store();
        let (t, _) = s.create_token(&new_token(Role::User)).unwrap();
        let id = enqueue(&s, &t, AgentKind::Claude);
        for _ in 0..5 {
            s.claim_next(AgentKind::Claude).unwrap().unwrap();
            s.requeue(&id).unwrap();
        }
        let r = s.get_request(&id).unwrap().unwrap();
        assert_eq!((r.status, r.started_at), (RequestStatus::Queued, None));
    }

    #[test]
    fn compact_returns_pruned_space() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("hashi.db")).unwrap();
        let (t, _) = s.create_token(&new_token(Role::User)).unwrap();
        let mut ids = Vec::new();
        for _ in 0..50 {
            let id = uuid::Uuid::new_v4().to_string();
            s.insert_request(NewRequest {
                id: &id,
                token: &t,
                agent: AgentKind::Claude,
                model: None,
                message: &"m".repeat(30_000),
            })
            .unwrap();
            ids.push(id);
        }
        let pages = |s: &Store| -> i64 {
            s.conn()
                .unwrap()
                .pragma_query_value(None, "page_count", |r| r.get(0))
                .unwrap()
        };
        let before = pages(&s);
        s.conn()
            .unwrap()
            .execute("DELETE FROM requests", [])
            .unwrap();
        assert!(s.compact().unwrap());
        assert!(pages(&s) < before / 4, "{} -> {}", before, pages(&s));
    }

    #[test]
    fn data_dir_is_the_database_folder() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("hashi.db")).unwrap();
        assert_eq!(
            s.data_dir().unwrap().canonicalize().unwrap(),
            dir.path().canonicalize().unwrap()
        );
        assert!(store().data_dir().is_none());
    }

    #[test]
    fn paused_token_still_authenticates() {
        let s = store();
        let (mut t, secret) = s.create_token(&new_token(Role::User)).unwrap();
        t.paused = true;
        s.save_token_policy(&t).unwrap();
        assert!(s.authenticate(&secret.raw).unwrap().unwrap().paused);
    }

    #[test]
    fn save_token_policy_roundtrips_limits() {
        let s = store();
        let (mut t, _) = s.create_token(&new_token(Role::User)).unwrap();
        t.note = Some("for the CI bot".into());
        t.limits = Limits {
            daily_requests: Some(10),
            monthly_requests: Some(100),
            daily_budget_usd: Some(1.5),
            monthly_budget_usd: Some(20.0),
            max_pending: Some(2),
        };
        s.save_token_policy(&t).unwrap();
        let back = s.get_token(&t.id).unwrap().unwrap();
        assert_eq!((back.note, back.limits), (t.note, t.limits));
    }

    #[test]
    fn token_usage_counts_today_and_pending() {
        let s = store();
        let (t, _) = s.create_token(&new_token(Role::User)).unwrap();
        let done = enqueue(&s, &t, AgentKind::Claude);
        s.claim_next(AgentKind::Claude).unwrap();
        let usage = Usage {
            input_tokens: 1,
            output_tokens: 1,
            cost_usd: Some(0.25),
        };
        s.complete_request(&done, "ok", Some(usage)).unwrap();
        enqueue(&s, &t, AgentKind::Claude);
        let u = s.token_usage(&t.id, now_ms()).unwrap();
        assert_eq!((u.today_requests, u.month_requests, u.pending), (2, 2, 1));
        assert!((u.today_cost_usd - 0.25).abs() < 1e-9);
    }

    #[test]
    fn prune_keeps_the_current_month() {
        let s = store();
        let (t, _) = s.create_token(&new_token(Role::User)).unwrap();
        let id = enqueue(&s, &t, AgentKind::Claude);
        s.claim_next(AgentKind::Claude).unwrap();
        s.complete_request(&id, "ok", None).unwrap();
        // Zero-day retention would delete everything, but monthly caps need it.
        assert_eq!(s.prune(0).unwrap(), 0);
    }

    #[test]
    fn authenticate_accepts_raw_secret_only() {
        let s = store();
        let (t, secret) = s.create_token(&new_token(Role::User)).unwrap();
        assert_eq!(
            s.authenticate(&secret.raw).unwrap().map(|x| x.id),
            Some(t.id)
        );
        assert!(s.authenticate(&secret.hash).unwrap().is_none());
    }

    #[test]
    fn authenticate_rejects_expired_tokens() {
        let s = store();
        let (t, secret) = s.create_token(&new_token(Role::User)).unwrap();
        s.conn()
            .unwrap()
            .execute("UPDATE tokens SET expires_at = 1 WHERE id = ?1", [&t.id])
            .unwrap();
        assert!(s.authenticate(&secret.raw).unwrap().is_none());
    }

    #[test]
    fn revoke_refuses_last_admin() {
        let s = store();
        let (admin, _) = s.create_token(&new_token(Role::Admin)).unwrap();
        assert_eq!(s.revoke_token(&admin.id).unwrap(), Revoke::LastAdmin);
    }

    #[test]
    fn revoke_by_prefix_deletes_token() {
        let s = store();
        let (user, _) = s.create_token(&new_token(Role::User)).unwrap();
        assert_eq!(
            s.revoke_token(&user.prefix).unwrap(),
            Revoke::Revoked(user.id.clone())
        );
        assert_eq!(s.revoke_token(&user.id).unwrap(), Revoke::NotFound);
    }

    #[test]
    fn claim_next_is_fifo_per_agent() {
        let s = store();
        let (t, _) = s.create_token(&new_token(Role::User)).unwrap();
        let first = enqueue(&s, &t, AgentKind::Claude);
        let _codex = enqueue(&s, &t, AgentKind::Codex);
        let _second = enqueue(&s, &t, AgentKind::Claude);
        let claimed = s.claim_next(AgentKind::Claude).unwrap().unwrap();
        assert_eq!(claimed.id, first);
        assert_eq!(claimed.status, RequestStatus::Running);
    }

    #[test]
    fn claim_next_skips_paused_and_expired_tokens() {
        let s = store();
        let (mut paused, _) = s.create_token(&new_token(Role::User)).unwrap();
        let (mut expired, _) = s.create_token(&new_token(Role::User)).unwrap();
        let (active, _) = s.create_token(&new_token(Role::User)).unwrap();
        let held = enqueue(&s, &paused, AgentKind::Claude);
        let stale = enqueue(&s, &expired, AgentKind::Claude);
        let fresh = enqueue(&s, &active, AgentKind::Claude);
        paused.paused = true;
        s.save_token_policy(&paused).unwrap();
        expired.expires_at = Some(1);
        s.save_token_policy(&expired).unwrap();

        assert_eq!(s.claim_next(AgentKind::Claude).unwrap().unwrap().id, fresh);
        assert!(s.claim_next(AgentKind::Claude).unwrap().is_none());
        // The expired token's prompt is still worth a look, to fail it.
        assert!(s.has_queued(AgentKind::Claude).unwrap());
        assert_eq!(
            s.fail_expired_queued(AgentKind::Claude).unwrap(),
            std::slice::from_ref(&stale)
        );
        let failed = s.get_request(&stale).unwrap().unwrap();
        assert_eq!(
            (failed.status, failed.error.as_deref()),
            (RequestStatus::Failed, Some(EXPIRED_BEFORE_START))
        );
        // Only the paused prompt is left, and it waits.
        assert!(!s.has_queued(AgentKind::Claude).unwrap());
        assert_eq!(
            s.get_request(&held).unwrap().unwrap().status,
            RequestStatus::Queued
        );
        paused.paused = false;
        s.save_token_policy(&paused).unwrap();
        assert_eq!(s.claim_next(AgentKind::Claude).unwrap().unwrap().id, held);
    }

    #[test]
    fn checked_insert_enforces_request_caps_in_order() {
        let s = store();
        let (t, _) = s.create_token(&new_token(Role::User)).unwrap();
        let daily = Limits {
            daily_requests: Some(1),
            monthly_requests: Some(1),
            ..Limits::default()
        };
        assert_eq!(checked_with(&s, &t, &daily, 100), Admission::Accepted);
        assert_eq!(
            checked_with(&s, &t, &daily, 100),
            Admission::LimitReached(Cap::DailyRequests)
        );
        let monthly = Limits {
            monthly_requests: Some(1),
            ..Limits::default()
        };
        assert_eq!(
            checked_with(&s, &t, &monthly, 100),
            Admission::LimitReached(Cap::MonthlyRequests)
        );
    }

    #[test]
    fn checked_insert_enforces_budget_caps() {
        let s = store();
        let (t, _) = s.create_token(&new_token(Role::User)).unwrap();
        let id = enqueue(&s, &t, AgentKind::Claude);
        s.claim_next(AgentKind::Claude).unwrap();
        s.complete_request(
            &id,
            "ok",
            Some(Usage {
                input_tokens: 1,
                output_tokens: 1,
                cost_usd: Some(0.5),
            }),
        )
        .unwrap();
        let day = Limits {
            daily_budget_usd: Some(0.5),
            ..Limits::default()
        };
        assert_eq!(
            checked_with(&s, &t, &day, 100),
            Admission::LimitReached(Cap::DailyBudgetUsd)
        );
        let month = Limits {
            monthly_budget_usd: Some(0.25),
            ..Limits::default()
        };
        assert_eq!(
            checked_with(&s, &t, &month, 100),
            Admission::LimitReached(Cap::MonthlyBudgetUsd)
        );
        let roomy = Limits {
            monthly_budget_usd: Some(1.0),
            ..Limits::default()
        };
        assert_eq!(checked_with(&s, &t, &roomy, 100), Admission::Accepted);
    }

    #[test]
    fn is_running_tracks_the_row() {
        let s = store();
        let (t, _) = s.create_token(&new_token(Role::User)).unwrap();
        let id = enqueue(&s, &t, AgentKind::Claude);
        assert!(!s.is_running(&id).unwrap());
        s.claim_next(AgentKind::Claude).unwrap();
        assert!(s.is_running(&id).unwrap());
        assert_eq!(s.running_for_token(&t.id).unwrap(), 1);
        s.revoke_token(&t.id).unwrap();
        assert!(!s.is_running(&id).unwrap());
        assert_eq!(s.running_for_token(&t.id).unwrap(), 0);
    }

    #[test]
    fn complete_request_records_response_and_usage() {
        let s = store();
        let (t, _) = s.create_token(&new_token(Role::User)).unwrap();
        let id = enqueue(&s, &t, AgentKind::Codex);
        s.claim_next(AgentKind::Codex).unwrap();
        let usage = Usage {
            input_tokens: 10,
            output_tokens: 3,
            cost_usd: Some(0.5),
        };
        s.complete_request(&id, "done", Some(usage)).unwrap();
        let r = s.get_request(&id).unwrap().unwrap();
        assert_eq!(
            (r.status, r.response.as_deref(), r.usage),
            (RequestStatus::Completed, Some("done"), Some(usage))
        );
    }

    #[test]
    fn recover_interrupted_requeues_running() {
        let s = store();
        let (t, _) = s.create_token(&new_token(Role::User)).unwrap();
        let id = enqueue(&s, &t, AgentKind::Claude);
        s.claim_next(AgentKind::Claude).unwrap();
        assert_eq!(s.recover_interrupted().unwrap(), 1);
        assert_eq!(
            s.get_request(&id).unwrap().unwrap().status,
            RequestStatus::Queued
        );
    }

    #[test]
    fn sessions_upsert_and_clear() {
        let s = store();
        let (t, _) = s.create_token(&new_token(Role::User)).unwrap();
        s.set_session(&t.id, AgentKind::Claude, "a").unwrap();
        s.set_session(&t.id, AgentKind::Claude, "b").unwrap();
        assert_eq!(
            s.session(&t.id, AgentKind::Claude).unwrap().as_deref(),
            Some("b")
        );
        s.clear_session(&t.id, AgentKind::Claude).unwrap();
        assert!(s.session(&t.id, AgentKind::Claude).unwrap().is_none());
    }

    #[test]
    fn stats_count_by_status() {
        let s = store();
        let (t, _) = s.create_token(&new_token(Role::User)).unwrap();
        enqueue(&s, &t, AgentKind::Claude);
        let id = enqueue(&s, &t, AgentKind::Codex);
        s.claim_next(AgentKind::Codex).unwrap();
        s.fail_request(&id, "boom").unwrap();
        let st = s.stats().unwrap();
        assert_eq!((st.total, st.queued, st.failed, st.tokens), (2, 1, 1, 1));
    }
}
