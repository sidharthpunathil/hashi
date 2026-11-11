//! Usage records: each account's quota windows as last seen, the state of
//! its last read, and the conversations each token keeps on an account.
//! No credential ever reaches these tables.

use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use super::{Result, Store, StoreError};
use crate::agent::AgentKind;
use crate::usage::{Source, Summary, Window};
use crate::util::now_ms;

/// The last read of an account's live usage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PollStatus {
    /// When the last read was tried, Unix ms.
    pub attempted_at: i64,
    /// When a read last succeeded, Unix ms.
    pub ok_at: Option<i64>,
    /// The plan, when known.
    pub plan: Option<String>,
    /// Why the last read gave no figures, such as "unknown, refreshes on
    /// its next run".
    pub note: Option<String>,
}

/// What an account's runs reported today.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Spend {
    /// Reported cost, USD.
    pub cost_usd: f64,
    /// Tokens in and out.
    pub tokens: u64,
}

/// A stateful conversation a token keeps on an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    /// Agent.
    pub agent: AgentKind,
    /// Account it is on.
    pub account: String,
    /// Project, `default` outside any.
    pub project: String,
    /// Last turn, Unix ms.
    pub updated_at: i64,
}

const WINDOW_COLUMNS: &str =
    "window, label, used_percent, resets_at, window_mins, source, observed_at";

fn row_to_window(r: &rusqlite::Row<'_>) -> rusqlite::Result<Result<Window>> {
    let source: String = r.get(5)?;
    let build = || -> Result<Window> {
        Ok(Window {
            key: r.get(0)?,
            label: r.get(1)?,
            used_percent: r.get(2)?,
            resets_at: r.get(3)?,
            window_mins: r.get(4)?,
            source: source.parse().map_err(StoreError::Corrupt)?,
            observed_at: Some(r.get(6)?),
        })
    };
    Ok(build())
}

fn insert_window(
    conn: &rusqlite::Connection,
    agent: AgentKind,
    name: &str,
    w: &Window,
) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO usage (agent, account, window, label, used_percent, resets_at, window_mins,
             source, observed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(agent, account, window) DO UPDATE SET label = ?4, used_percent = ?5,
             resets_at = ?6, window_mins = ?7, source = ?8, observed_at = ?9",
    )?
    .execute(params![
        agent.as_str(),
        name,
        w.key,
        w.label,
        crate::usage::clamp_percent(w.used_percent),
        w.resets_at,
        w.window_mins,
        w.source.as_str(),
        w.observed_at.unwrap_or_else(now_ms),
    ])?;
    // The reading over time, for the forecast (see `crate::stats`): kept
    // when the figure moved, or at most every ten minutes when it did not.
    let at = w.observed_at.unwrap_or_else(now_ms);
    let pct = crate::usage::clamp_percent(w.used_percent);
    conn.prepare_cached(
        "INSERT INTO quota_history (agent, account, window, used_percent, resets_at, observed_at)
         SELECT ?1, ?2, ?3, ?4, ?5, ?6
         WHERE NOT EXISTS (
             SELECT 1 FROM quota_history
             WHERE agent = ?1 AND account = ?2 AND window = ?3
               AND used_percent = ?4 AND observed_at > ?6 - 600000)",
    )?
    .execute(params![agent.as_str(), name, w.key, pct, w.resets_at, at])?;
    Ok(())
}

impl Store {
    /// Replaces every window of an account with a poll's `windows`.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn replace_usage(&self, agent: AgentKind, name: &str, windows: &[Window]) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        // A poll that finished after the account was removed.
        if !super::accounts::account_known(&tx, agent, name)? {
            return Ok(());
        }
        tx.execute(
            "DELETE FROM usage WHERE agent = ?1 AND account = ?2",
            params![agent.as_str(), name],
        )?;
        for w in windows {
            insert_window(&tx, agent, name, w)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Adds or updates the given windows of an account, keeping the others.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn record_usage(&self, agent: AgentKind, name: &str, windows: &[Window]) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        // A run that finished after the account was removed.
        if !super::accounts::account_known(&tx, agent, name)? {
            return Ok(());
        }
        for w in windows {
            insert_window(&tx, agent, name, w)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Forgets the windows a failed run recorded (source `error`), after a
    /// run on the account succeeded.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn clear_error_usage(&self, agent: AgentKind, name: &str) -> Result<()> {
        self.conn()?.execute(
            "DELETE FROM usage WHERE agent = ?1 AND account = ?2 AND source = ?3",
            params![agent.as_str(), name, Source::Error.as_str()],
        )?;
        Ok(())
    }

    /// An account's windows, by key.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn usage_windows(&self, agent: AgentKind, name: &str) -> Result<Vec<Window>> {
        let conn = self.conn()?;
        let sql = format!(
            "SELECT {WINDOW_COLUMNS} FROM usage WHERE agent = ?1 AND account = ?2
             ORDER BY COALESCE(window_mins, 0), window"
        );
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![agent.as_str(), name], row_to_window)?;
        rows.map(|r| r.map_err(StoreError::from).and_then(|w| w))
            .collect()
    }

    /// Every account with usage windows or a read, as `(agent, name)`.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn usage_accounts(&self) -> Result<Vec<(AgentKind, String)>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT agent, account FROM usage UNION SELECT agent, account FROM usage_polls
             ORDER BY 1, 2",
        )?;
        let rows: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows
            .into_iter()
            .filter_map(|(a, n)| Some((a.parse().ok()?, n)))
            .collect())
    }

    /// Records a read of an account's usage. A successful read clears the
    /// note; `plan` is kept when the read gives none. Nothing is recorded
    /// for an account that no longer exists.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn set_poll_status(
        &self,
        agent: AgentKind,
        name: &str,
        ok: bool,
        plan: Option<&str>,
        note: Option<&str>,
    ) -> Result<()> {
        let now = now_ms();
        self.conn()?.execute(
            "INSERT INTO usage_polls (agent, account, attempted_at, ok_at, plan, note)
             SELECT ?1, ?2, ?3, CASE WHEN ?4 THEN ?3 END, ?5, ?6
             WHERE ?2 = 'default'
                 OR EXISTS (SELECT 1 FROM accounts WHERE agent = ?1 AND name = ?2)
             ON CONFLICT(agent, account) DO UPDATE SET attempted_at = ?3,
                 ok_at = CASE WHEN ?4 THEN ?3 ELSE ok_at END,
                 plan = COALESCE(?5, plan), note = ?6",
            params![agent.as_str(), name, now, ok, plan, note],
        )?;
        Ok(())
    }

    /// The last read of an account's usage.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn poll_status(&self, agent: AgentKind, name: &str) -> Result<Option<PollStatus>> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT attempted_at, ok_at, plan, note FROM usage_polls
                 WHERE agent = ?1 AND account = ?2",
                params![agent.as_str(), name],
                |r| {
                    Ok(PollStatus {
                        attempted_at: r.get(0)?,
                        ok_at: r.get(1)?,
                        plan: r.get(2)?,
                        note: r.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    /// Whether every enabled account in `names` has used `stop` percent of
    /// a window at `now`; then the soonest time one of them opens again.
    /// `None` when any of them is under the stop or not known.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn all_past_stop(
        &self,
        agent: AgentKind,
        names: &[String],
        stop: u8,
        now: i64,
    ) -> Result<Option<crate::usage::PastStop>> {
        let mut held: Option<crate::usage::PastStop> = None;
        for name in names {
            if name != crate::accounts::DEFAULT
                && self.get_account(agent, name)?.is_none_or(|a| a.disabled)
            {
                continue;
            }
            let Some(p) = self.past_stop(agent, name, stop, now)? else {
                return Ok(None);
            };
            held = Some(match held {
                None => p,
                Some(h) => crate::usage::PastStop {
                    used: h.used.min(p.used),
                    opens_at: match (h.opens_at, p.opens_at) {
                        (Some(a), Some(b)) => Some(a.min(b)),
                        (a, b) => a.or(b),
                    },
                },
            });
        }
        Ok(held)
    }

    /// Whether an account has used `stop` percent of any window at `now`
    /// (see [`crate::usage::past_stop`]).
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn past_stop(
        &self,
        agent: AgentKind,
        name: &str,
        stop: u8,
        now: i64,
    ) -> Result<Option<crate::usage::PastStop>> {
        let windows = self.usage_windows(agent, name)?;
        Ok(crate::usage::past_stop(&windows, stop, now))
    }

    /// An account's [`Summary`] at `now` under `rotate_at`: its windows and
    /// its limited mark. A disabled account is summarised like any other.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn account_summary(
        &self,
        agent: AgentKind,
        name: &str,
        rotate_at: u8,
        now: i64,
    ) -> Result<Summary> {
        let windows = self.usage_windows(agent, name)?;
        let limited = self.get_account(agent, name)?.and_then(|a| a.limited_until);
        Ok(crate::usage::summarize(&windows, limited, rotate_at, now))
    }

    /// Marks an account limited until `until_ms` without recording a use,
    /// for a poll that says the limit is reached. The time is capped at
    /// [`crate::agent::limit::MAX_RESET`] from now, and replaces the mark
    /// already there: a fresh poll knows better than an older reading, so a
    /// far-future time once seen cannot keep the account limited forever.
    /// An account other than `default` without a row is left alone.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn set_account_limited(&self, agent: AgentKind, name: &str, until_ms: i64) -> Result<()> {
        let now = now_ms();
        let until = super::accounts::cap_limit(until_ms, now);
        let conn = self.conn()?;
        let n = conn.execute(
            "UPDATE accounts SET limited_until = ?3 WHERE agent = ?1 AND name = ?2",
            params![agent.as_str(), name, until],
        )?;
        if n == 0 && name == crate::accounts::DEFAULT {
            conn.execute(
                "INSERT INTO accounts (agent, name, kind, limited_until, created_at)
                 VALUES (?1, ?2, 'default', ?3, ?4)
                 ON CONFLICT(agent, name) DO UPDATE SET limited_until = ?3",
                params![agent.as_str(), name, until, now],
            )?;
        }
        Ok(())
    }

    /// What runs on an account reported since the start of the UTC day
    /// containing `now`: the cost and the tokens, in and out.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn account_spend_today(&self, agent: AgentKind, name: &str, now: i64) -> Result<Spend> {
        let (cost, tokens): (f64, i64) = self
            .conn()?
            .prepare_cached(
                "SELECT COALESCE(SUM(cost_usd), 0.0),
                    COALESCE(SUM(COALESCE(input_tokens, 0) + COALESCE(output_tokens, 0)), 0)
             FROM requests WHERE agent = ?1 AND account = ?2 AND started_at >= ?3",
            )?
            .query_row(
                params![agent.as_str(), name, crate::util::day_start_utc(now)],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
        Ok(Spend {
            cost_usd: cost,
            tokens: u64::try_from(tokens).unwrap_or(0),
        })
    }

    /// Whether a run on the account is going on now.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn account_busy(&self, agent: AgentKind, name: &str) -> Result<bool> {
        let n: i64 = self
            .conn()?
            .prepare_cached(
                "SELECT COUNT(*) FROM requests
                 WHERE status = 'running' AND agent = ?1 AND account = ?2",
            )?
            .query_row(params![agent.as_str(), name], |r| r.get(0))?;
        Ok(n > 0)
    }

    /// The account of the token's most recent conversation with `agent` in
    /// `project`, among `accounts`.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn latest_conversation(
        &self,
        token_id: &str,
        agent: AgentKind,
        project: &str,
        accounts: &[String],
    ) -> Result<Option<String>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT account FROM sessions WHERE token_id = ?1 AND agent = ?2 AND project = ?3
             ORDER BY updated_at DESC",
        )?;
        let rows: Vec<String> = stmt
            .query_map(params![token_id, agent.as_str(), project], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows.into_iter().find(|a| accounts.contains(a)))
    }

    /// Forgets the token's conversation with `agent` on one account in one
    /// project, so the next message there starts fresh.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn clear_conversation(
        &self,
        token_id: &str,
        agent: AgentKind,
        account: &str,
        project: &str,
    ) -> Result<()> {
        self.conn()?.execute(
            "DELETE FROM sessions WHERE token_id = ?1 AND agent = ?2 AND account = ?3
                 AND project = ?4",
            params![token_id, agent.as_str(), account, project],
        )?;
        Ok(())
    }

    /// The conversations a token keeps, most recent first.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn conversations(&self, token_id: &str) -> Result<Vec<Conversation>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT agent, account, project, updated_at FROM sessions WHERE token_id = ?1
             ORDER BY updated_at DESC",
        )?;
        let rows: Vec<(String, String, String, i64)> = stmt
            .query_map([token_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows
            .into_iter()
            .filter_map(|(agent, account, project, updated_at)| {
                Some(Conversation {
                    agent: agent.parse().ok()?,
                    account,
                    project,
                    updated_at,
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::{UsageState, Window};

    fn w(key: &str, used: f64, resets_at: Option<i64>, source: Source) -> Window {
        Window::new(key, key, used, resets_at, Some(300), source)
    }

    /// A store with the Claude login account `work`.
    fn with_work() -> Store {
        let s = Store::open_in_memory().unwrap();
        s.add_account(
            AgentKind::Claude,
            "work",
            crate::accounts::AccountKind::Login,
        )
        .unwrap();
        s
    }

    #[test]
    fn polls_replace_and_runs_add() {
        let s = with_work();
        let now = now_ms();
        let later = Some(now + 3_600_000);
        s.replace_usage(
            AgentKind::Claude,
            "work",
            &[
                w("five_hour", 40.0, later, Source::Poll),
                w("seven_day", 10.0, later, Source::Poll),
            ],
        )
        .unwrap();
        s.record_usage(
            AgentKind::Claude,
            "work",
            &[w("five_hour", 55.0, later, Source::Run)],
        )
        .unwrap();
        let all = s.usage_windows(AgentKind::Claude, "work").unwrap();
        assert_eq!(all.len(), 2);
        let five = all.iter().find(|x| x.key == "five_hour").unwrap();
        assert!((five.used_percent - 55.0).abs() < f64::EPSILON);
        assert_eq!(five.source, Source::Run);
        s.replace_usage(
            AgentKind::Claude,
            "work",
            &[w("seven_day", 20.0, later, Source::Poll)],
        )
        .unwrap();
        assert_eq!(s.usage_windows(AgentKind::Claude, "work").unwrap().len(), 1);
        let sum = s
            .account_summary(AgentKind::Claude, "work", 90, now)
            .unwrap();
        assert_eq!(sum.headroom_pct(), Some(80));
        assert_eq!(sum.state, UsageState::Ok);
        assert_eq!(
            s.usage_accounts().unwrap(),
            [(AgentKind::Claude, "work".to_owned())]
        );
    }

    #[test]
    fn error_windows_go_when_a_run_succeeds() {
        let s = with_work();
        let now = now_ms();
        s.record_usage(
            AgentKind::Claude,
            "work",
            &[w("limit", 100.0, Some(now + 60_000), Source::Error)],
        )
        .unwrap();
        let sum = s
            .account_summary(AgentKind::Claude, "work", 90, now)
            .unwrap();
        assert_eq!(sum.state, UsageState::Limited);
        s.clear_error_usage(AgentKind::Claude, "work").unwrap();
        let sum = s
            .account_summary(AgentKind::Claude, "work", 90, now)
            .unwrap();
        assert_eq!(sum.state, UsageState::Unknown);
    }

    #[test]
    fn poll_status_keeps_the_plan_and_the_last_success() {
        let s = Store::open_in_memory().unwrap();
        s.set_poll_status(AgentKind::Codex, "default", true, Some("plus"), None)
            .unwrap();
        s.set_poll_status(AgentKind::Codex, "default", false, None, Some("unknown, x"))
            .unwrap();
        let p = s.poll_status(AgentKind::Codex, "default").unwrap().unwrap();
        assert_eq!(p.plan.as_deref(), Some("plus"));
        assert!(p.ok_at.is_some());
        assert_eq!(p.note.as_deref(), Some("unknown, x"));
        assert!(s.poll_status(AgentKind::Codex, "nope").unwrap().is_none());
    }

    #[test]
    fn a_poll_limit_mark_does_not_count_as_a_use() {
        let s = Store::open_in_memory().unwrap();
        s.add_account(
            AgentKind::Codex,
            "team",
            crate::accounts::AccountKind::Login,
        )
        .unwrap();
        let until = now_ms() + 60_000;
        s.set_account_limited(AgentKind::Codex, "team", until)
            .unwrap();
        let a = s.get_account(AgentKind::Codex, "team").unwrap().unwrap();
        assert_eq!((a.limited_until, a.last_used), (Some(until), None));
        // A fresh poll replaces the mark, earlier or not.
        s.set_account_limited(AgentKind::Codex, "team", until - 30_000)
            .unwrap();
        let a = s.get_account(AgentKind::Codex, "team").unwrap().unwrap();
        assert_eq!(a.limited_until, Some(until - 30_000));
    }
}
