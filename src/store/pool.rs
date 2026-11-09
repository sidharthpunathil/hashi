//! A pool: one join link, the people who joined it, the `pool` account
//! each offers per agent, and the tokens that use them all. What runs
//! where is in [`crate::pool`]; secrets are kept here only as hashes.

use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;

use super::{NewToken, Result, Store, StoreError, insert_token};
use crate::accounts::{self as acct, AccountMap};
use crate::agent::AgentKind;
use crate::token::{self, Limits, Role, SessionMode, Token};
use crate::util::now_ms;

/// Prefix of pool join codes.
pub const POOL_LINK_PREFIX: &str = "hashi_pool_";

/// Prefix of a member's worker secret.
pub const MEMBER_PREFIX: &str = "hashi_mem_";

/// Most people one pool link lets in.
pub const MAX_MEMBERS: u32 = 50;

/// Prompts a member's pool token may have in flight.
const MEMBER_MAX_PENDING: u32 = 3;

/// The pool's join link. Never contains the code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolLink {
    /// Stable identifier.
    pub id: String,
    /// When it was opened, Unix ms.
    pub created_at: i64,
    /// When it stops letting people in, Unix ms.
    pub expires_at: i64,
    /// When the host closed it, Unix ms.
    pub closed_at: Option<i64>,
    /// How many people may join through it.
    pub max_members: u32,
}

impl PoolLink {
    /// Whether people can still join through it at `now`.
    #[must_use]
    pub fn is_open(&self, now: i64) -> bool {
        self.closed_at.is_none() && self.expires_at > now
    }
}

/// Someone who joined the pool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolMember {
    /// Stable identifier.
    pub id: String,
    /// Their name, also the name of each account they offer.
    pub name: String,
    /// The token they use the pool with.
    pub token_id: Option<String>,
    /// The agents they offer.
    pub agents: Vec<AgentKind>,
    /// When they joined, Unix ms.
    pub joined_at: i64,
    /// When their worker last asked for work, Unix ms.
    pub last_seen: Option<i64>,
    /// When they said they leave, Unix ms.
    pub until: Option<i64>,
}

const MEMBER_COLUMNS: &str = "id, name, token_id, agents, joined_at, last_seen, until";

fn row_to_member(r: &Row<'_>) -> rusqlite::Result<PoolMember> {
    let agents: String = r.get(3)?;
    Ok(PoolMember {
        id: r.get(0)?,
        name: r.get(1)?,
        token_id: r.get(2)?,
        agents: token::decode_agents(&agents),
        joined_at: r.get(4)?,
        last_seen: r.get(5)?,
        until: r.get(6)?,
    })
}

fn row_to_link(r: &Row<'_>) -> rusqlite::Result<PoolLink> {
    Ok(PoolLink {
        id: r.get(0)?,
        created_at: r.get(1)?,
        expires_at: r.get(2)?,
        closed_at: r.get(3)?,
        max_members: r.get(4)?,
    })
}

const LINK_COLUMNS: &str = "id, created_at, expires_at, closed_at, max_members";

/// The account map every pool token gets: each agent's pool accounts.
fn pool_map(conn: &rusqlite::Connection) -> Result<AccountMap> {
    let mut stmt = conn.prepare(
        "SELECT agent, name FROM accounts WHERE kind = 'pool' ORDER BY created_at, rowid",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut map = AccountMap::new();
    for row in rows {
        let (agent, name) = row?;
        if let Ok(agent) = agent.parse::<AgentKind>() {
            map.entry(agent).or_default().push(name);
        }
    }
    Ok(map)
}

fn sync_tokens(conn: &rusqlite::Connection) -> Result<()> {
    let map = pool_map(conn)?;
    conn.execute(
        "UPDATE tokens SET accounts = ?1 WHERE pool = 1",
        [acct::encode(&map)],
    )?;
    Ok(())
}

impl Store {
    /// Opens a new join link, closing any open one. Returns it with its
    /// one-time-shown code.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn open_pool_link(&self, hours: u32, max_members: u32) -> Result<(PoolLink, String)> {
        let bytes: [u8; 24] = rand::random();
        let code = format!("{POOL_LINK_PREFIX}{}", hex::encode(bytes));
        let now = now_ms();
        let link = PoolLink {
            id: uuid::Uuid::new_v4().to_string(),
            created_at: now,
            expires_at: now + i64::from(hours) * 3_600_000,
            closed_at: None,
            max_members: max_members.clamp(1, MAX_MEMBERS),
        };
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE pool_links SET closed_at = ?1 WHERE closed_at IS NULL",
            [now],
        )?;
        tx.execute(
            "INSERT INTO pool_links (id, hash, created_at, expires_at, max_members)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                link.id,
                token::hash(&code),
                link.created_at,
                link.expires_at,
                link.max_members
            ],
        )?;
        tx.commit()?;
        Ok((link, code))
    }

    /// The newest join link, open or not.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn pool_link(&self) -> Result<Option<PoolLink>> {
        let sql = format!("SELECT {LINK_COLUMNS} FROM pool_links ORDER BY created_at DESC LIMIT 1");
        Ok(self.conn()?.query_row(&sql, [], row_to_link).optional()?)
    }

    /// Closes the open join link. Returns `false` if none was open.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn close_pool_link(&self) -> Result<bool> {
        let n = self.conn()?.execute(
            "UPDATE pool_links SET closed_at = ?1 WHERE closed_at IS NULL",
            [now_ms()],
        )?;
        Ok(n > 0)
    }

    /// The link a join code belongs to, open or not.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn pool_link_by_code(&self, code: &str) -> Result<Option<PoolLink>> {
        let sql = format!("SELECT {LINK_COLUMNS} FROM pool_links WHERE hash = ?1");
        Ok(self
            .conn()?
            .query_row(&sql, [token::hash(code)], row_to_link)
            .optional()?)
    }

    /// Adds someone to the pool through `link`: their row, a `pool`
    /// account named `name` for each agent they offer, and the token they
    /// use the pool with. Every pool token then uses the new accounts.
    /// Returns the member, their worker secret and their token's secret.
    ///
    /// # Errors
    ///
    /// Fails with [`StoreError::LimitReached`] when the link is full or the
    /// name is taken by a member or an account, or on database errors.
    pub fn add_pool_member(
        &self,
        link: &PoolLink,
        name: &str,
        agents: &[AgentKind],
        until: Option<i64>,
    ) -> Result<(PoolMember, String, String)> {
        let now = now_ms();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let joined: i64 = tx.query_row(
            "SELECT COUNT(*) FROM pool_members WHERE joined_at >= ?1",
            [link.created_at],
            |r| r.get(0),
        )?;
        if joined >= i64::from(link.max_members) {
            return Err(StoreError::LimitReached("this pool is full"));
        }
        let taken: bool = tx
            .prepare("SELECT 1 FROM pool_members WHERE name = ?1")?
            .exists([name])?
            || tx
                .prepare("SELECT 1 FROM accounts WHERE name = ?1")?
                .exists([name])?;
        if taken {
            return Err(StoreError::LimitReached(
                "that name is taken in this pool; pick another",
            ));
        }
        let (tok, generated) = insert_token(
            &tx,
            &NewToken {
                name: format!("pool: {name}"),
                role: Role::User,
                session_mode: SessionMode::Stateless,
                agents: AgentKind::ALL.to_vec(),
                expires_in_days: None,
                note: Some("joined the pool".to_owned()),
                limits: Limits {
                    max_pending: Some(MEMBER_MAX_PENDING),
                    ..Limits::default()
                },
                accounts: AccountMap::new(),
                project: None,
            },
        )?;
        tx.execute(
            "UPDATE tokens SET pool = 1, share_usage = 1, expires_at = ?2 WHERE id = ?1",
            params![tok.id, until],
        )?;
        let bytes: [u8; 32] = rand::random();
        let secret = format!("{MEMBER_PREFIX}{}", hex::encode(bytes));
        let member = PoolMember {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_owned(),
            token_id: Some(tok.id),
            agents: agents.to_vec(),
            joined_at: now,
            last_seen: None,
            until,
        };
        tx.execute(
            "INSERT INTO pool_members (id, name, secret_hash, token_id, agents, joined_at, until)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                member.id,
                member.name,
                token::hash(&secret),
                member.token_id,
                token::encode_agents(&member.agents),
                member.joined_at,
                member.until,
            ],
        )?;
        for agent in agents {
            tx.execute(
                "INSERT INTO accounts (agent, name, kind, created_at, pool_member)
                 VALUES (?1, ?2, 'pool', ?3, ?4)",
                params![agent.as_str(), name, now, member.id],
            )?;
        }
        sync_tokens(&tx)?;
        tx.commit()?;
        Ok((member, secret, generated.raw))
    }

    /// The member a worker secret belongs to.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn pool_member_by_secret(&self, secret: &str) -> Result<Option<PoolMember>> {
        if !secret.starts_with(MEMBER_PREFIX) {
            return Ok(None);
        }
        let sql = format!("SELECT {MEMBER_COLUMNS} FROM pool_members WHERE secret_hash = ?1");
        Ok(self
            .conn()?
            .query_row(&sql, [token::hash(secret)], row_to_member)
            .optional()?)
    }

    /// Everyone in the pool, by when they joined.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn list_pool_members(&self) -> Result<Vec<PoolMember>> {
        let conn = self.conn()?;
        let sql = format!("SELECT {MEMBER_COLUMNS} FROM pool_members ORDER BY joined_at, rowid");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], row_to_member)?;
        rows.map(|r| r.map_err(StoreError::from)).collect()
    }

    /// Records that a member's worker asked for work.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn touch_pool_member(&self, id: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE pool_members SET last_seen = ?2 WHERE id = ?1",
            params![id, now_ms()],
        )?;
        Ok(())
    }

    /// Takes a member out of the pool: their accounts and token go, and
    /// every pool token stops using them. Returns the member, if there was
    /// one with that id or name.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn remove_pool_member(&self, id_or_name: &str) -> Result<Option<PoolMember>> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let sql = format!("SELECT {MEMBER_COLUMNS} FROM pool_members WHERE id = ?1 OR name = ?1");
        let Some(member) = tx.query_row(&sql, [id_or_name], row_to_member).optional()? else {
            return Ok(None);
        };
        tx.execute(
            "DELETE FROM accounts WHERE kind = 'pool' AND pool_member = ?1",
            [&member.id],
        )?;
        if let Some(t) = &member.token_id {
            tx.execute("DELETE FROM tokens WHERE id = ?1", [t])?;
        }
        tx.execute("DELETE FROM pool_members WHERE id = ?1", [&member.id])?;
        sync_tokens(&tx)?;
        tx.commit()?;
        Ok(Some(member))
    }

    /// Whether `token` uses the pool.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn token_in_pool(&self, token: &Token) -> Result<bool> {
        Ok(self
            .conn()?
            .prepare_cached("SELECT 1 FROM tokens WHERE id = ?1 AND pool = 1")?
            .exists([&token.id])?)
    }

    /// Makes token `id` use the pool, or stop using it (its account map
    /// goes back to every agent's default). Returns `false` if there is no
    /// such token.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn set_token_pool(&self, id: &str, pool: bool) -> Result<bool> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let map = if pool {
            pool_map(&tx)?
        } else {
            AccountMap::new()
        };
        let n = tx.execute(
            "UPDATE tokens SET pool = ?2, share_usage = ?2 OR share_usage, accounts = ?3
             WHERE id = ?1",
            params![id, pool, acct::encode(&map)],
        )?;
        tx.commit()?;
        Ok(n == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joining_adds_accounts_a_token_and_every_pool_token_follows() {
        let s = Store::open_in_memory().unwrap();
        let (link, code) = s.open_pool_link(24, 2).unwrap();
        assert!(code.starts_with(POOL_LINK_PREFIX));
        assert_eq!(s.pool_link_by_code(&code).unwrap().unwrap().id, link.id);
        let (sid, secret, raw) = s
            .add_pool_member(&link, "sid", &[AgentKind::Claude, AgentKind::Codex], None)
            .unwrap();
        assert_eq!(
            s.pool_member_by_secret(&secret).unwrap().unwrap().id,
            sid.id
        );
        let mine = s.authenticate(&raw).unwrap().unwrap();
        assert!(s.token_in_pool(&mine).unwrap());
        assert_eq!(acct::for_agent(&mine.accounts, AgentKind::Claude), ["sid"]);

        let (_, _, raw2) = s
            .add_pool_member(&link, "ana", &[AgentKind::Claude], None)
            .unwrap();
        let first = s.get_token(&mine.id).unwrap().unwrap();
        assert_eq!(
            acct::for_agent(&first.accounts, AgentKind::Claude),
            ["sid", "ana"]
        );
        // Full, and names are unique.
        assert!(
            s.add_pool_member(&link, "bo", &[AgentKind::Claude], None)
                .is_err()
        );
        let (link2, _) = s.open_pool_link(24, 5).unwrap();
        assert!(
            s.add_pool_member(&link2, "sid", &[AgentKind::Claude], None)
                .is_err()
        );

        // Leaving takes the accounts off everyone's tokens.
        s.remove_pool_member("sid").unwrap().unwrap();
        let ana = s.authenticate(&raw2).unwrap().unwrap();
        assert_eq!(acct::for_agent(&ana.accounts, AgentKind::Claude), ["ana"]);
        assert!(acct::for_agent(&ana.accounts, AgentKind::Codex) == ["default"]);
        assert!(s.authenticate(&raw).unwrap().is_none());
        assert!(s.get_account(AgentKind::Codex, "sid").unwrap().is_none());
    }
}
