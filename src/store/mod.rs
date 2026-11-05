//! SQLite persistence for tokens, requests and agent sessions.
//!
//! The connection lives behind a `std::sync::Mutex`. Every query here is a
//! short indexed lookup or write on a local file, so critical sections are
//! microseconds long and the lock is never held across an `.await`.

use std::path::Path;
use std::str::FromStr;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;

mod audit;
mod invites;

pub use audit::AuditEntry;
pub use invites::{Invite, InviteState, NewInvite, Redeemed};

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
}

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revoke {
    /// The token was deleted.
    Revoked,
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

const MIGRATIONS: [&str; 2] = [SCHEMA_V1, SCHEMA_V2];

const TOKEN_COLUMNS: &str = "id, name, prefix, role, session_mode, agents, expires_at, created_at, \
     last_used_at, paused, note, daily_requests, monthly_requests, daily_budget_usd, \
     monthly_budget_usd, max_pending";

const REQUEST_COLUMNS: &str = "id, token_id, token_name, agent, model, message, stateful, status, \
     response, error, input_tokens, output_tokens, cost_usd, created_at, started_at, completed_at";

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
        migrate(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
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

    /// Writes a token's editable fields: name, note, pause, expiry and limits.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn save_token_policy(&self, t: &Token) -> Result<()> {
        let l = t.limits;
        self.conn()?.execute(
            "UPDATE tokens SET name = ?1, note = ?2, paused = ?3, expires_at = ?4,
                 daily_requests = ?5, monthly_requests = ?6, daily_budget_usd = ?7,
                 monthly_budget_usd = ?8, max_pending = ?9
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
            ],
        )?;
        Ok(())
    }

    /// A token's use in the UTC day and month containing `now_ms`.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn token_usage(&self, token_id: &str, now_ms: i64) -> Result<TokenUsage> {
        let day = crate::util::day_start_utc(now_ms);
        let month = month_start_utc(now_ms);
        let row = self.conn()?.query_row(
            "SELECT
                 COUNT(CASE WHEN created_at >= ?2 THEN 1 END),
                 COUNT(CASE WHEN created_at >= ?3 THEN 1 END),
                 COALESCE(SUM(CASE WHEN created_at >= ?2 THEN cost_usd END), 0.0),
                 COALESCE(SUM(CASE WHEN created_at >= ?3 THEN cost_usd END), 0.0),
                 COUNT(CASE WHEN status IN ('queued', 'running') THEN 1 END)
             FROM requests
             WHERE token_id = ?1 AND (created_at >= ?3 OR status IN ('queued', 'running'))",
            params![token_id, day, month],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, f64>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            },
        )?;
        let n = |v: i64| u64::try_from(v).unwrap_or(0);
        Ok(TokenUsage {
            today_requests: n(row.0),
            month_requests: n(row.1),
            today_cost_usd: row.2,
            month_cost_usd: row.3,
            pending: n(row.4),
        })
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
        Ok(Revoke::Revoked)
    }

    // ── requests ───────────────────────────────────────────────────────────

    /// Enqueues a request.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn insert_request(&self, new: NewRequest<'_>) -> Result<()> {
        self.conn()?.execute(
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

    /// Atomically moves the oldest queued request for `agent` to `running`.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn claim_next(&self, agent: AgentKind) -> Result<Option<Request>> {
        let sql = format!(
            "UPDATE requests SET status = 'running', started_at = ?1
             WHERE id = (SELECT id FROM requests WHERE status = 'queued' AND agent = ?2
                         ORDER BY created_at, rowid LIMIT 1)
             RETURNING {REQUEST_COLUMNS}"
        );
        self.conn()?
            .query_row(&sql, params![now_ms(), agent.as_str()], row_to_request)
            .optional()?
            .transpose()
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

    /// Most recent requests, newest first.
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

    /// Re-queues requests that were running when the process last stopped.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn recover_interrupted(&self) -> Result<usize> {
        Ok(self.conn()?.execute(
            "UPDATE requests SET status = 'queued', started_at = NULL WHERE status = 'running'",
            [],
        )?)
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

    /// The agent session a stateful token should resume, if any.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn session(&self, token_id: &str, agent: AgentKind) -> Result<Option<String>> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT agent_session_id FROM sessions WHERE token_id = ?1 AND agent = ?2",
                params![token_id, agent.as_str()],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Records the agent session for a token.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn set_session(&self, token_id: &str, agent: AgentKind, session: &str) -> Result<()> {
        self.conn()?.execute(
            "INSERT INTO sessions (token_id, agent, agent_session_id, updated_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(token_id, agent) DO UPDATE SET agent_session_id = ?3, updated_at = ?4",
            params![token_id, agent.as_str(), session, now_ms()],
        )?;
        Ok(())
    }

    /// Forgets a token's session with `agent`, so the next prompt starts fresh.
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

fn migrate(conn: &mut Connection) -> Result<()> {
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
    };
    let l = token.limits;
    conn.execute(
        "INSERT INTO tokens (id, name, hash, prefix, role, session_mode, agents, expires_at,
             created_at, note, daily_requests, monthly_requests, daily_budget_usd,
             monthly_budget_usd, max_pending)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
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
        ],
    )?;
    Ok((token, secret))
}

fn row_to_token(r: &Row<'_>) -> rusqlite::Result<Result<Token>> {
    let role: String = r.get(3)?;
    let mode: String = r.get(4)?;
    let agents: String = r.get(5)?;
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
        assert_eq!(version, 2);
        // Opening again is a no-op.
        drop(s);
        assert!(Store::open(&path).is_ok());
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
        assert_eq!(s.revoke_token(&user.prefix).unwrap(), Revoke::Revoked);
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
