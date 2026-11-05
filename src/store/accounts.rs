//! Account state: which accounts exist, whether they are disabled or at a
//! limit, and which tokens and invites use them. The credentials themselves
//! never touch the database (see [`crate::accounts`]).

use std::collections::{BTreeSet, HashMap};

use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;

use super::{Result, Store, StoreError};
use crate::accounts::{self as acct, AccountKind, AccountMap};
use crate::agent::AgentKind;
use crate::util::{day_start_utc, now_ms};

/// One account's stored state. The default account has a row only once it
/// has been used from a token that lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountState {
    /// Agent it belongs to.
    pub agent: AgentKind,
    /// Account name.
    pub name: String,
    /// How it signs in.
    pub kind: AccountKind,
    /// Switched off by the owner.
    pub disabled: bool,
    /// At its limit until this time, Unix ms.
    pub limited_until: Option<i64>,
    /// Last run on it, Unix ms.
    pub last_used: Option<i64>,
    /// When it was added, Unix ms.
    pub created_at: i64,
    /// For a remote account: the saved remote (in `remotes.json`) it
    /// forwards to. Never its token.
    pub remote: Option<String>,
    /// For a remote account: the agent it asks for on that bridge.
    pub remote_agent: Option<AgentKind>,
    /// Why Hashi switched the account off on its own, for example because
    /// the remote bridge paused or revoked its token. Cleared on enable.
    pub disabled_reason: Option<String>,
}

/// Whether an account can take a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountStatus {
    /// Usable.
    Ready,
    /// At its limit until this time, Unix ms.
    Limited(i64),
    /// Switched off by the owner.
    Disabled,
}

impl AccountStatus {
    /// `ready`, `limited` or `disabled`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Limited(_) => "limited",
            Self::Disabled => "disabled",
        }
    }
}

impl AccountState {
    /// The account's status at `now`. Disabled wins over limited.
    #[must_use]
    pub fn status(&self, now: i64) -> AccountStatus {
        match self.limited_until {
            _ if self.disabled => AccountStatus::Disabled,
            Some(until) if until > now => AccountStatus::Limited(until),
            _ => AccountStatus::Ready,
        }
    }
}

/// What removing an account changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RemovedAccount {
    /// Whether the account existed.
    pub existed: bool,
    /// Tokens it was taken off.
    pub tokens: usize,
    /// Invites it was taken off.
    pub invites: usize,
    /// Projects it was taken off.
    pub projects: usize,
}

const ACCOUNT_COLUMNS: &str = "agent, name, kind, disabled, limited_until, last_used, created_at, \
     remote, remote_agent, disabled_reason";

fn row_to_account(r: &Row<'_>) -> rusqlite::Result<Result<AccountState>> {
    let agent: String = r.get(0)?;
    let kind: String = r.get(2)?;
    let remote_agent: Option<String> = r.get(8)?;
    let build = || -> Result<AccountState> {
        Ok(AccountState {
            agent: agent
                .parse()
                .map_err(|e: crate::agent::UnknownAgent| StoreError::Corrupt(e.to_string()))?,
            name: r.get(1)?,
            kind: kind.parse().map_err(StoreError::Corrupt)?,
            disabled: r.get(3)?,
            limited_until: r.get(4)?,
            last_used: r.get(5)?,
            created_at: r.get(6)?,
            remote: r.get(7)?,
            remote_agent: remote_agent.and_then(|a| a.parse().ok()),
            disabled_reason: r.get(9)?,
        })
    };
    Ok(build())
}

/// `map` with `name` taken off `agent`'s list, or `None` if it was not there.
fn without(mut map: AccountMap, agent: AgentKind, name: &str) -> Option<AccountMap> {
    let list = map.get_mut(&agent)?;
    let before = list.len();
    list.retain(|n| n != name);
    if list.len() == before {
        return None;
    }
    if list.is_empty() {
        map.remove(&agent);
    }
    Some(map)
}

impl Store {
    /// Records a new account. Returns `false` if it already exists.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn add_account(&self, agent: AgentKind, name: &str, kind: AccountKind) -> Result<bool> {
        let n = self.conn()?.execute(
            "INSERT INTO accounts (agent, name, kind, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(agent, name) DO NOTHING",
            params![agent.as_str(), name, kind.as_str(), now_ms()],
        )?;
        Ok(n == 1)
    }

    /// Records a new remote account: `name` for `agent`, forwarding to the
    /// saved remote `remote` and its agent `remote_agent`. Returns `false`
    /// if it already exists.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn add_remote_account(
        &self,
        agent: AgentKind,
        name: &str,
        remote: &str,
        remote_agent: AgentKind,
    ) -> Result<bool> {
        let n = self.conn()?.execute(
            "INSERT INTO accounts (agent, name, kind, created_at, remote, remote_agent)
             VALUES (?1, ?2, 'remote', ?3, ?4, ?5)
             ON CONFLICT(agent, name) DO NOTHING",
            params![
                agent.as_str(),
                name,
                now_ms(),
                remote,
                remote_agent.as_str()
            ],
        )?;
        Ok(n == 1)
    }

    /// Switches an account off on Hashi's own account, with the reason the
    /// owner sees. Returns `false` if there is no such account.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn disable_account_because(
        &self,
        agent: AgentKind,
        name: &str,
        reason: &str,
    ) -> Result<bool> {
        let n = self.conn()?.execute(
            "UPDATE accounts SET disabled = 1, disabled_reason = ?3 WHERE agent = ?1 AND name = ?2",
            params![agent.as_str(), name, reason],
        )?;
        Ok(n == 1)
    }

    /// One account's state.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn get_account(&self, agent: AgentKind, name: &str) -> Result<Option<AccountState>> {
        let sql = format!("SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE agent = ?1 AND name = ?2");
        self.conn()?
            .prepare_cached(&sql)?
            .query_row(params![agent.as_str(), name], row_to_account)
            .optional()?
            .transpose()
    }

    /// Every account with a row, by agent and name.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn list_accounts(&self) -> Result<Vec<AccountState>> {
        let conn = self.conn()?;
        let sql = format!("SELECT {ACCOUNT_COLUMNS} FROM accounts ORDER BY agent, name");
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt.query_map([], row_to_account)?;
        rows.map(|r| r.map_err(StoreError::from).and_then(|a| a))
            .collect()
    }

    /// Disables or enables an account. Returns `false` if there is no such
    /// account.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn set_account_disabled(
        &self,
        agent: AgentKind,
        name: &str,
        disabled: bool,
    ) -> Result<bool> {
        let n = self.conn()?.execute(
            "UPDATE accounts SET disabled = ?3, disabled_reason = NULL
             WHERE agent = ?1 AND name = ?2",
            params![agent.as_str(), name, disabled],
        )?;
        Ok(n == 1)
    }

    /// Deletes an account's row and takes it off every token, invite and
    /// project that lists it, and forgets the conversations and usage kept on
    /// it, in one transaction.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn remove_account(&self, agent: AgentKind, name: &str) -> Result<RemovedAccount> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let existed = tx.execute(
            "DELETE FROM accounts WHERE agent = ?1 AND name = ?2",
            params![agent.as_str(), name],
        )? == 1;
        let mut out = RemovedAccount {
            existed,
            ..RemovedAccount::default()
        };
        for (table, key, count) in [
            ("tokens", "id", &mut out.tokens),
            ("invites", "id", &mut out.invites),
            ("projects", "name", &mut out.projects),
        ] {
            let rows: Vec<(String, String)> = {
                let mut stmt = tx.prepare(&format!("SELECT {key}, accounts FROM {table}"))?;
                stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?
            };
            for (id, map) in rows {
                if let Some(map) = without(acct::decode(&map), agent, name) {
                    tx.execute(
                        &format!("UPDATE {table} SET accounts = ?1 WHERE {key} = ?2"),
                        params![acct::encode(&map), id],
                    )?;
                    *count += 1;
                }
            }
        }
        for table in ["sessions", "usage", "usage_polls"] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE agent = ?1 AND account = ?2"),
                params![agent.as_str(), name],
            )?;
        }
        tx.commit()?;
        Ok(out)
    }

    /// Marks an account as at its limit until `until_ms` and records the use.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn mark_account_limited(&self, agent: AgentKind, name: &str, until_ms: i64) -> Result<()> {
        self.conn()?
            .prepare_cached(
                "INSERT INTO accounts (agent, name, kind, limited_until, last_used, created_at)
             VALUES (?1, ?2, 'default', ?3, ?4, ?4)
             ON CONFLICT(agent, name) DO UPDATE SET limited_until = ?3, last_used = ?4",
            )?
            .execute(params![agent.as_str(), name, until_ms, now_ms()])?;
        Ok(())
    }

    /// Records a run on an account. A run that succeeded also clears the
    /// account's limited mark.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn mark_account_used(&self, agent: AgentKind, name: &str, succeeded: bool) -> Result<()> {
        self.conn()?
            .prepare_cached(
                "INSERT INTO accounts (agent, name, kind, last_used, created_at)
             VALUES (?1, ?2, 'default', ?3, ?3)
             ON CONFLICT(agent, name) DO UPDATE SET last_used = ?3,
                 limited_until = CASE WHEN ?4 THEN NULL ELSE limited_until END",
            )?
            .execute(params![agent.as_str(), name, now_ms(), succeeded])?;
        Ok(())
    }

    /// Runs started on an account since the start of the UTC day containing
    /// `now`, and in all (subject to retention).
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn account_runs(&self, agent: AgentKind, name: &str, now: i64) -> Result<(u64, u64)> {
        let (today, total): (i64, i64) = self
            .conn()?
            .prepare_cached(
                "SELECT COUNT(CASE WHEN started_at >= ?3 THEN 1 END), COUNT(*)
             FROM requests WHERE agent = ?1 AND account = ?2",
            )?
            .query_row(params![agent.as_str(), name, day_start_utc(now)], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        Ok((
            u64::try_from(today).unwrap_or(0),
            u64::try_from(total).unwrap_or(0),
        ))
    }

    /// Tokens whose account list for `agent` includes `name`.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn tokens_using_account(&self, agent: AgentKind, name: &str) -> Result<usize> {
        Ok(self
            .list_tokens()?
            .iter()
            .filter(|t| {
                t.accounts
                    .get(&agent)
                    .is_some_and(|l| l.iter().any(|n| n == name))
            })
            .count())
    }

    /// How many tokens list each account, by agent and name: what
    /// [`Store::tokens_using_account`] answers for one account, for every
    /// account from one read of the tokens table. Accounts no token lists
    /// are absent.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn account_token_counts(&self) -> Result<HashMap<(AgentKind, String), usize>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare_cached("SELECT accounts FROM tokens")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut counts = HashMap::new();
        for row in rows {
            for (agent, names) in acct::decode(&row?) {
                // A token counts once per account, even if listed twice.
                let names: BTreeSet<String> = names.into_iter().collect();
                for name in names {
                    *counts.entry((agent, name)).or_insert(0) += 1;
                }
            }
        }
        Ok(counts)
    }

    /// The `agent:name` entries of `map` that are not accounts. `default`
    /// always exists.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn unknown_accounts(&self, map: &AccountMap) -> Result<Vec<String>> {
        let mut unknown = Vec::new();
        for (agent, names) in map {
            for name in names {
                let known = name == acct::DEFAULT
                    || self
                        .get_account(*agent, name)?
                        .is_some_and(|a| a.kind != AccountKind::Default);
                if !known {
                    unknown.push(format!("{agent}:{name}"));
                }
            }
        }
        Ok(unknown)
    }

    /// Records which account is running request `id`.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn set_request_account(&self, id: &str, account: &str) -> Result<()> {
        self.conn()?
            .prepare_cached("UPDATE requests SET account = ?2 WHERE id = ?1")?
            .execute(params![id, account])?;
        Ok(())
    }

    /// Puts a running request back in the queue after `account` hit its
    /// limit, so it runs on the next account. Like [`Store::requeue`] this
    /// does not count an interruption; the account is remembered so the
    /// request never tries it again. The request keeps its creation time,
    /// so it goes ahead of everything queued after it.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn requeue_after_limit(&self, id: &str, account: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE requests SET status = 'queued', started_at = NULL,
                 accounts_tried = CASE WHEN accounts_tried = '' THEN ?2
                                       ELSE accounts_tried || ',' || ?2 END
             WHERE id = ?1 AND status = 'running'",
            params![id, account],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{NewInvite, NewRequest, NewToken, RequestStatus};
    use crate::token::{Limits, Role, SessionMode};

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    fn token(s: &Store, accounts: AccountMap) -> crate::token::Token {
        s.create_token(&NewToken {
            name: "t".into(),
            role: Role::User,
            session_mode: SessionMode::Stateful,
            agents: AgentKind::ALL.to_vec(),
            expires_in_days: None,
            note: None,
            limits: Limits::default(),
            accounts,
            project: None,
        })
        .unwrap()
        .0
    }

    fn map(agent: AgentKind, names: &[&str]) -> AccountMap {
        AccountMap::from([(agent, names.iter().map(|n| (*n).to_owned()).collect())])
    }

    #[test]
    fn accounts_are_added_once_and_listed() {
        let s = store();
        assert!(
            s.add_account(AgentKind::Claude, "work", AccountKind::Login)
                .unwrap()
        );
        assert!(
            !s.add_account(AgentKind::Claude, "work", AccountKind::ApiKey)
                .unwrap()
        );
        s.add_account(AgentKind::Codex, "ci", AccountKind::ApiKey)
            .unwrap();
        let all = s.list_accounts().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(
            (all[0].agent, all[0].kind),
            (AgentKind::Claude, AccountKind::Login)
        );
        assert_eq!(all[0].status(now_ms()), AccountStatus::Ready);
    }

    #[test]
    fn disable_limit_and_success_change_the_status() {
        let s = store();
        s.add_account(AgentKind::Claude, "work", AccountKind::Login)
            .unwrap();
        let now = now_ms();
        s.mark_account_limited(AgentKind::Claude, "work", now + 60_000)
            .unwrap();
        let a = s.get_account(AgentKind::Claude, "work").unwrap().unwrap();
        assert_eq!(a.status(now), AccountStatus::Limited(now + 60_000));
        assert_eq!(a.kind, AccountKind::Login, "the kind is kept");
        s.mark_account_used(AgentKind::Claude, "work", false)
            .unwrap();
        assert!(matches!(
            s.get_account(AgentKind::Claude, "work")
                .unwrap()
                .unwrap()
                .status(now),
            AccountStatus::Limited(_)
        ));
        s.mark_account_used(AgentKind::Claude, "work", true)
            .unwrap();
        let a = s.get_account(AgentKind::Claude, "work").unwrap().unwrap();
        assert_eq!(
            (a.status(now), a.last_used.is_some()),
            (AccountStatus::Ready, true)
        );
        assert!(
            s.set_account_disabled(AgentKind::Claude, "work", true)
                .unwrap()
        );
        assert_eq!(
            s.get_account(AgentKind::Claude, "work")
                .unwrap()
                .unwrap()
                .status(now),
            AccountStatus::Disabled
        );
        assert!(
            !s.set_account_disabled(AgentKind::Claude, "nope", true)
                .unwrap()
        );
    }

    #[test]
    fn unknown_names_are_reported() {
        let s = store();
        s.add_account(AgentKind::Claude, "work", AccountKind::Login)
            .unwrap();
        let m = AccountMap::from([
            (
                AgentKind::Claude,
                vec!["work".to_owned(), "default".to_owned(), "x".to_owned()],
            ),
            (AgentKind::Codex, vec!["work".to_owned()]),
        ]);
        assert_eq!(s.unknown_accounts(&m).unwrap(), ["claude:x", "codex:work"]);
        // A default row made by a limit mark is not an added account.
        s.mark_account_limited(AgentKind::Gemini, "ghost", 1)
            .unwrap();
        assert_eq!(
            s.unknown_accounts(&map(AgentKind::Gemini, &["ghost"]))
                .unwrap(),
            ["gemini:ghost"]
        );
    }

    #[test]
    fn removing_takes_the_account_off_tokens_invites_and_sessions() {
        let s = store();
        s.add_account(AgentKind::Claude, "work", AccountKind::Login)
            .unwrap();
        s.add_account(AgentKind::Claude, "home", AccountKind::Login)
            .unwrap();
        let both = token(&s, map(AgentKind::Claude, &["work", "home"]));
        let only = token(&s, map(AgentKind::Claude, &["work"]));
        let other = token(&s, map(AgentKind::Claude, &["home"]));
        s.create_invite(&NewInvite {
            label: "a".into(),
            session_mode: SessionMode::Stateless,
            agents: vec![AgentKind::Claude],
            token_expires_in_days: None,
            limits: Limits::default(),
            link_hours: 24,
            accounts: map(AgentKind::Claude, &["work"]),
            project: None,
        })
        .unwrap();
        s.set_account_session(&both.id, AgentKind::Claude, "work", "s1")
            .unwrap();
        s.set_account_session(&both.id, AgentKind::Claude, "home", "s2")
            .unwrap();
        assert_eq!(
            s.tokens_using_account(AgentKind::Claude, "work").unwrap(),
            2
        );
        let counts = s.account_token_counts().unwrap();
        for (name, n) in [("work", 2), ("home", 2)] {
            assert_eq!(counts[&(AgentKind::Claude, name.to_owned())], n);
            assert_eq!(s.tokens_using_account(AgentKind::Claude, name).unwrap(), n);
        }
        assert_eq!(counts.len(), 2);

        let r = s.remove_account(AgentKind::Claude, "work").unwrap();
        assert_eq!(
            r,
            RemovedAccount {
                existed: true,
                tokens: 2,
                invites: 1,
                projects: 0,
            }
        );
        let get = |id: &str| s.get_token(id).unwrap().unwrap().accounts;
        assert_eq!(get(&both.id), map(AgentKind::Claude, &["home"]));
        assert_eq!(get(&only.id), AccountMap::new());
        assert_eq!(get(&other.id), map(AgentKind::Claude, &["home"]));
        assert!(s.list_invites().unwrap()[0].accounts.is_empty());
        assert!(
            s.account_session(&both.id, AgentKind::Claude, "work")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            s.account_session(&both.id, AgentKind::Claude, "home")
                .unwrap()
                .as_deref(),
            Some("s2")
        );
        assert!(!s.remove_account(AgentKind::Claude, "work").unwrap().existed);
    }

    #[test]
    fn sessions_are_kept_per_account() {
        let s = store();
        let t = token(&s, AccountMap::new());
        s.set_session(&t.id, AgentKind::Claude, "d").unwrap();
        s.set_account_session(&t.id, AgentKind::Claude, "work", "w")
            .unwrap();
        assert_eq!(
            s.session(&t.id, AgentKind::Claude).unwrap().as_deref(),
            Some("d")
        );
        assert_eq!(
            s.account_session(&t.id, AgentKind::Claude, "work")
                .unwrap()
                .as_deref(),
            Some("w")
        );
        s.clear_session(&t.id, AgentKind::Claude).unwrap();
        assert!(s.session(&t.id, AgentKind::Claude).unwrap().is_none());
        assert!(
            s.account_session(&t.id, AgentKind::Claude, "work")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn requeue_after_limit_remembers_accounts_without_counting_an_interruption() {
        let s = store();
        let t = token(&s, AccountMap::new());
        let id = uuid::Uuid::new_v4().to_string();
        s.insert_request(NewRequest {
            id: &id,
            token: &t,
            agent: AgentKind::Claude,
            model: None,
            message: "hi",
        })
        .unwrap();
        for name in ["work", "home"] {
            s.claim_next(AgentKind::Claude).unwrap().unwrap();
            s.set_request_account(&id, name).unwrap();
            s.requeue_after_limit(&id, name).unwrap();
        }
        let r = s.get_request(&id).unwrap().unwrap();
        assert_eq!(r.status, RequestStatus::Queued);
        assert_eq!(r.accounts_tried, ["work", "home"]);
        assert_eq!(r.account.as_deref(), Some("home"));
        let attempts: i64 = s
            .conn()
            .unwrap()
            .query_row("SELECT attempts FROM requests WHERE id = ?1", [&id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(attempts, 0);
        let (today, total) = s.account_runs(AgentKind::Claude, "home", now_ms()).unwrap();
        assert_eq!(
            (today, total),
            (0, 1),
            "a queued request has not started today"
        );
    }
}
