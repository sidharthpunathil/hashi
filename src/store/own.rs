//! The owner's own use of the agent CLIs (see [`crate::stats`]): one row
//! per message of their interactive sessions, how far each session file
//! has been read, and the quota readings over time. Only numbers, a model
//! name and a project folder's name are kept; never what was said.

use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use super::{Result, Store};
use crate::agent::AgentKind;

/// How far a session file has been read, and what its earlier lines said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileState {
    /// Bytes read so far.
    pub offset: u64,
    /// The account UUID its `session-owner` line named.
    pub owner: Option<String>,
    /// Whether it ran without permission prompts.
    pub skip_perms: bool,
    /// Whether it was a headless (`-p`, SDK) session, which is left out.
    pub sdk: bool,
}

/// One message of the owner's own use.
#[derive(Debug, Clone, PartialEq)]
pub struct OwnRow {
    /// The API message id, so a message read twice counts once.
    pub msg_id: String,
    /// The agent.
    pub agent: AgentKind,
    /// The Hashi account it ran on (`default` for the owner's own login).
    pub account: String,
    /// The model.
    pub model: String,
    /// The project folder's name.
    pub project: String,
    /// The session id.
    pub session: String,
    /// When, Unix ms.
    pub at: i64,
    /// Input tokens not read from the cache.
    pub input: i64,
    /// Output tokens.
    pub output: i64,
    /// Tokens read from the cache.
    pub cache_read: i64,
    /// Tokens written to the cache.
    pub cache_write: i64,
    /// Estimated cost at list prices, USD.
    pub cost_usd: f64,
    /// Whether the session skipped permission prompts.
    pub skip_perms: bool,
}

/// Sums over some of the owner's own messages.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnTotals {
    /// What the rows are grouped by (a day, account, model or project).
    pub key: String,
    /// Messages.
    pub messages: i64,
    /// Sessions.
    pub sessions: i64,
    /// Input tokens not read from the cache.
    pub input: i64,
    /// Output tokens.
    pub output: i64,
    /// Tokens read from the cache.
    pub cache_read: i64,
    /// Tokens written to the cache.
    pub cache_write: i64,
    /// Estimated cost, USD.
    pub cost_usd: f64,
}

impl OwnTotals {
    /// Every token counted.
    #[must_use]
    pub const fn tokens(&self) -> i64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

/// What Hashi ran for others on one account.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedTotals {
    /// The account.
    pub account: String,
    /// Finished requests.
    pub requests: i64,
    /// Tokens in and out, as the runs reported them.
    pub tokens: i64,
    /// Cost, as the runs reported it, USD.
    pub cost_usd: f64,
}

/// One quota reading.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QuotaPoint {
    /// Percent used.
    pub used_percent: f64,
    /// When the window resets, Unix ms.
    pub resets_at: Option<i64>,
    /// When it was read, Unix ms.
    pub observed_at: i64,
}

/// What [`Store::own_totals`] groups by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupBy {
    /// UTC day, as `YYYY-MM-DD`.
    Day,
    /// Account.
    Account,
    /// Model.
    Model,
    /// Project folder.
    Project,
    /// Whether permission prompts were skipped (`skip` or `ask`).
    SkipPerms,
    /// Nothing: one row for everything.
    All,
}

pub(super) fn prune_quota_history(conn: &rusqlite::Connection, before: i64) -> Result<usize> {
    Ok(conn.execute("DELETE FROM quota_history WHERE observed_at < ?1", [before])?)
}

impl Store {
    /// How far `path` has been read.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn own_file_state(&self, path: &str) -> Result<Option<FileState>> {
        Ok(self
            .conn()?
            .prepare_cached(
                "SELECT offset, owner, skip_perms, sdk FROM own_usage_files WHERE path = ?1",
            )?
            .query_row([path], |r| {
                Ok(FileState {
                    offset: u64::try_from(r.get::<_, i64>(0)?).unwrap_or(0),
                    owner: r.get(1)?,
                    skip_perms: r.get(2)?,
                    sdk: r.get(3)?,
                })
            })
            .optional()?)
    }

    /// Saves the rows read from `path` and how far it was read, at once.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn save_own_rows(&self, path: &str, state: &FileState, rows: &[OwnRow]) -> Result<usize> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let mut added = 0;
        {
            let mut insert = tx.prepare_cached(
                "INSERT OR IGNORE INTO own_usage (msg_id, agent, account, model, project, session,
                     at, input, output, cache_read, cache_write, cost_usd, skip_perms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            )?;
            for r in rows {
                added += insert.execute(params![
                    r.msg_id,
                    r.agent.as_str(),
                    r.account,
                    r.model,
                    r.project,
                    r.session,
                    r.at,
                    r.input,
                    r.output,
                    r.cache_read,
                    r.cache_write,
                    r.cost_usd,
                    r.skip_perms,
                ])?;
            }
            // The owner's own sessions are use of the account too.
            let mut touch = tx.prepare_cached(
                "UPDATE accounts SET last_used = ?3
                 WHERE agent = ?1 AND name = ?2 AND COALESCE(last_used, 0) < ?3",
            )?;
            for r in rows {
                touch.execute(params![r.agent.as_str(), r.account, r.at])?;
            }
        }
        tx.execute(
            "INSERT INTO own_usage_files (path, offset, owner, skip_perms, sdk)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(path) DO UPDATE SET offset = ?2, owner = ?3, skip_perms = ?4, sdk = ?5",
            params![
                path,
                i64::try_from(state.offset).unwrap_or(i64::MAX),
                state.owner,
                state.skip_perms,
                state.sdk
            ],
        )?;
        tx.commit()?;
        Ok(added)
    }

    /// Deletes everything collected about the owner's own use.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn forget_own_usage(&self) -> Result<usize> {
        let conn = self.conn()?;
        let n = conn.execute("DELETE FROM own_usage", [])?;
        conn.execute("DELETE FROM own_usage_files", [])?;
        Ok(n)
    }

    /// The owner's own use since `since` (Unix ms), grouped by `by`, the
    /// largest cost first (days in order).
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn own_totals(&self, since: i64, by: GroupBy) -> Result<Vec<OwnTotals>> {
        let key = match by {
            GroupBy::Day => "strftime('%Y-%m-%d', at / 1000, 'unixepoch')",
            GroupBy::Account => "account",
            GroupBy::Model => "model",
            GroupBy::Project => "project",
            GroupBy::SkipPerms => "CASE skip_perms WHEN 1 THEN 'skip' ELSE 'ask' END",
            GroupBy::All => "'all'",
        };
        let order = if by == GroupBy::Day { "1" } else { "8 DESC" };
        let sql = format!(
            "SELECT {key}, COUNT(*), COUNT(DISTINCT session), SUM(input), SUM(output),
                 SUM(cache_read), SUM(cache_write), SUM(cost_usd)
             FROM own_usage WHERE at >= ?1 GROUP BY 1 ORDER BY {order}"
        );
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map([since], |r| {
                Ok(OwnTotals {
                    key: r.get(0)?,
                    messages: r.get(1)?,
                    sessions: r.get(2)?,
                    input: r.get(3)?,
                    output: r.get(4)?,
                    cache_read: r.get(5)?,
                    cache_write: r.get(6)?,
                    cost_usd: r.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The owner's own use of `account` of `agent` since `since`.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn own_account_totals(
        &self,
        agent: AgentKind,
        account: &str,
        since: i64,
    ) -> Result<OwnTotals> {
        Ok(self
            .conn()?
            .prepare_cached(
                "SELECT COUNT(*), COUNT(DISTINCT session), COALESCE(SUM(input), 0),
                     COALESCE(SUM(output), 0), COALESCE(SUM(cache_read), 0),
                     COALESCE(SUM(cache_write), 0), COALESCE(SUM(cost_usd), 0.0)
                 FROM own_usage WHERE agent = ?1 AND account = ?2 AND at >= ?3",
            )?
            .query_row(params![agent.as_str(), account, since], |r| {
                Ok(OwnTotals {
                    key: account.to_owned(),
                    messages: r.get(0)?,
                    sessions: r.get(1)?,
                    input: r.get(2)?,
                    output: r.get(3)?,
                    cache_read: r.get(4)?,
                    cache_write: r.get(5)?,
                    cost_usd: r.get(6)?,
                })
            })?)
    }

    /// Tokens used on each account since `since`, by agent name and account:
    /// what Hashi ran (input and output, as the runs reported them) plus the
    /// owner's own tracked use (every token counted, as `hashi stats` does).
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn account_tokens_since(
        &self,
        since: i64,
    ) -> Result<std::collections::HashMap<(String, String), i64>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT agent, account, SUM(n) FROM (
                 SELECT agent, COALESCE(account, 'default') AS account,
                     COALESCE(input_tokens, 0) + COALESCE(output_tokens, 0) AS n
                 FROM requests WHERE status = 'completed' AND created_at >= ?1
                 UNION ALL
                 SELECT agent, account, input + output + cache_read + cache_write
                 FROM own_usage WHERE at >= ?1)
             GROUP BY 1, 2",
        )?;
        let rows = stmt
            .query_map([since], |r| {
                Ok((
                    (r.get(0)?, r.get(1)?),
                    r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// What Hashi ran for tokens on each account of `agent` since `since`.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn shared_totals(&self, agent: AgentKind, since: i64) -> Result<Vec<SharedTotals>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT COALESCE(account, 'default'), COUNT(*),
                 COALESCE(SUM(COALESCE(input_tokens, 0) + COALESCE(output_tokens, 0)), 0),
                 COALESCE(SUM(cost_usd), 0.0)
             FROM requests
             WHERE agent = ?1 AND status = 'completed' AND created_at >= ?2
             GROUP BY 1 ORDER BY 4 DESC",
        )?;
        let rows = stmt
            .query_map(params![agent.as_str(), since], |r| {
                Ok(SharedTotals {
                    account: r.get(0)?,
                    requests: r.get(1)?,
                    tokens: r.get(2)?,
                    cost_usd: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The quota readings of one window since `since`, oldest first.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn quota_history(
        &self,
        agent: AgentKind,
        account: &str,
        window: &str,
        since: i64,
    ) -> Result<Vec<QuotaPoint>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT used_percent, resets_at, observed_at FROM quota_history
             WHERE agent = ?1 AND account = ?2 AND window = ?3 AND observed_at >= ?4
             ORDER BY observed_at",
        )?;
        let rows = stmt
            .query_map(params![agent.as_str(), account, window, since], |r| {
                Ok(QuotaPoint {
                    used_percent: r.get(0)?,
                    resets_at: r.get(1)?,
                    observed_at: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}
