//! One-time invite links. An invite is a token template plus a hashed code;
//! redeeming it burns the code and creates the token in one transaction.

use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;

use super::{NewToken, Result, Store, StoreError, insert_token};
use crate::accounts::{self as acct, AccountMap};
use crate::agent::AgentKind;
use crate::token::{self, GeneratedToken, Limits, Role, SessionMode, Token};
use crate::util::now_ms;

/// Prefix of invite codes.
pub const INVITE_PREFIX: &str = "hashi_inv_";

/// Input for [`Store::create_invite`].
#[derive(Debug, Clone)]
pub struct NewInvite {
    /// Who the invite is for. Becomes the token's name.
    pub label: String,
    /// Conversation behaviour of the token.
    pub session_mode: SessionMode,
    /// Agents the token may use.
    pub agents: Vec<AgentKind>,
    /// Lifetime of the created token, from the moment it is claimed.
    pub token_expires_in_days: Option<u32>,
    /// Usage caps of the created token.
    pub limits: Limits,
    /// How long the link stays valid, in hours.
    pub link_hours: u32,
    /// Accounts per agent the created token may use.
    pub accounts: AccountMap,
    /// The project the created token is bound to.
    pub project: Option<String>,
}

/// A stored invite. Never contains the code.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Invite {
    /// Stable identifier.
    pub id: String,
    /// Who the invite is for.
    pub label: String,
    /// Conversation behaviour of the token.
    pub session_mode: SessionMode,
    /// Agents the token may use.
    pub agents: Vec<AgentKind>,
    /// Lifetime of the created token, in days.
    pub token_expires_in_days: Option<u32>,
    /// Usage caps of the created token.
    pub limits: Limits,
    /// When the link stops working, Unix ms.
    pub expires_at: i64,
    /// Creation time, Unix ms.
    pub created_at: i64,
    /// When it was claimed, Unix ms.
    pub claimed_at: Option<i64>,
    /// The token it created, once claimed.
    pub token_id: Option<String>,
    /// Accounts per agent the created token may use. Admins only: the claim
    /// page never shows them.
    pub accounts: AccountMap,
    /// The project the created token is bound to. The claim page shows
    /// only this name.
    pub project: Option<String>,
}

impl Invite {
    /// `pending`, `claimed` or `expired`.
    #[must_use]
    pub fn status(&self, now_ms: i64) -> &'static str {
        if self.claimed_at.is_some() {
            "claimed"
        } else if self.expires_at <= now_ms {
            "expired"
        } else {
            "pending"
        }
    }
}

/// What a code currently points at.
#[derive(Debug, Clone, PartialEq)]
pub enum InviteState {
    /// Valid and unclaimed.
    Pending(Box<Invite>),
    /// Already used.
    Claimed,
    /// Past its link expiry.
    Expired,
    /// No such code.
    NotFound,
}

/// Outcome of [`Store::redeem_invite`].
#[derive(Debug)]
pub enum Redeemed {
    /// The token that was created, and its one-time secret.
    Token(Box<Token>, GeneratedToken),
    /// Already used.
    Claimed,
    /// Past its link expiry.
    Expired,
    /// No such code.
    NotFound,
    /// The server already holds [`super::MAX_TOKENS`] tokens. The code is
    /// left unused.
    TokenLimit,
}

const INVITE_COLUMNS: &str = "id, label, session_mode, agents, token_expires_in_days, \
     daily_requests, monthly_requests, daily_budget_usd, monthly_budget_usd, max_pending, \
     expires_at, created_at, claimed_at, token_id, accounts, project";

impl Store {
    /// Creates an invite and returns it with its one-time code.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn create_invite(&self, new: &NewInvite) -> Result<(Invite, String)> {
        let bytes: [u8; 32] = rand::random();
        let code = format!("{INVITE_PREFIX}{}", hex::encode(bytes));
        let now = now_ms();
        let invite = Invite {
            id: uuid::Uuid::new_v4().to_string(),
            label: new.label.clone(),
            session_mode: new.session_mode,
            agents: new.agents.clone(),
            token_expires_in_days: new.token_expires_in_days,
            limits: new.limits,
            expires_at: now + i64::from(new.link_hours) * 3_600_000,
            created_at: now,
            claimed_at: None,
            token_id: None,
            accounts: new.accounts.clone(),
            project: new.project.clone(),
        };
        let l = invite.limits;
        self.conn()?.execute(
            "INSERT INTO invites (id, hash, label, session_mode, agents, token_expires_in_days,
                 daily_requests, monthly_requests, daily_budget_usd, monthly_budget_usd,
                 max_pending, expires_at, created_at, accounts, project)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                invite.id,
                token::hash(&code),
                invite.label,
                invite.session_mode.as_str(),
                token::encode_agents(&invite.agents),
                invite.token_expires_in_days,
                l.daily_requests,
                l.monthly_requests,
                l.daily_budget_usd,
                l.monthly_budget_usd,
                l.max_pending,
                invite.expires_at,
                invite.created_at,
                acct::encode(&invite.accounts),
                invite.project,
            ],
        )?;
        Ok((invite, code))
    }

    /// All invites, newest first.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn list_invites(&self) -> Result<Vec<Invite>> {
        let conn = self.conn()?;
        let sql = format!("SELECT {INVITE_COLUMNS} FROM invites ORDER BY created_at DESC");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], row_to_invite)?;
        rows.map(|r| r.map_err(StoreError::from).and_then(|i| i))
            .collect()
    }

    /// Deletes a pending invite. Claimed invites are kept as history.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn cancel_invite(&self, id: &str) -> Result<bool> {
        let n = self.conn()?.execute(
            "DELETE FROM invites WHERE id = ?1 AND claimed_at IS NULL",
            [id],
        )?;
        Ok(n > 0)
    }

    /// Deletes invites claimed, or expired unclaimed, before `before_ms`.
    /// Pending invites are never removed.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn prune_invites(&self, before_ms: i64) -> Result<usize> {
        Ok(self.conn()?.execute(
            "DELETE FROM invites
             WHERE (claimed_at IS NOT NULL AND claimed_at < ?1)
                OR (claimed_at IS NULL AND expires_at < ?1)",
            [before_ms],
        )?)
    }

    /// Looks up a code without using it.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn preview_invite(&self, code: &str) -> Result<InviteState> {
        let sql = format!("SELECT {INVITE_COLUMNS} FROM invites WHERE hash = ?1");
        let found = self
            .conn()?
            .query_row(&sql, [token::hash(code)], row_to_invite)
            .optional()?
            .transpose()?;
        Ok(classify(found, now_ms()))
    }

    /// Burns a code and creates its token, atomically. Works once.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn redeem_invite(&self, code: &str) -> Result<Redeemed> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let sql = format!("SELECT {INVITE_COLUMNS} FROM invites WHERE hash = ?1");
        let found = tx
            .query_row(&sql, [token::hash(code)], row_to_invite)
            .optional()?
            .transpose()?;
        let now = now_ms();
        let invite = match classify(found, now) {
            InviteState::Pending(i) => *i,
            InviteState::Claimed => return Ok(Redeemed::Claimed),
            InviteState::Expired => return Ok(Redeemed::Expired),
            InviteState::NotFound => return Ok(Redeemed::NotFound),
        };
        let tokens: i64 = tx.query_row("SELECT COUNT(*) FROM tokens", [], |r| r.get(0))?;
        if usize::try_from(tokens).unwrap_or(usize::MAX) >= super::MAX_TOKENS {
            return Ok(Redeemed::TokenLimit);
        }
        let (token, secret) = insert_token(
            &tx,
            &NewToken {
                name: invite.label.clone(),
                role: Role::User,
                session_mode: invite.session_mode,
                agents: invite.agents.clone(),
                expires_in_days: invite.token_expires_in_days,
                note: Some(format!(
                    "invited on {}",
                    crate::util::rfc3339(invite.created_at).unwrap_or_default()
                )),
                limits: invite.limits,
                accounts: invite.accounts.clone(),
                project: invite.project.clone(),
            },
        )?;
        let changed = tx.execute(
            "UPDATE invites SET claimed_at = ?1, token_id = ?2 WHERE id = ?3 AND claimed_at IS NULL",
            params![now, token.id, invite.id],
        )?;
        if changed != 1 {
            // Dropping the transaction rolls the new token back.
            return Ok(Redeemed::Claimed);
        }
        tx.commit()?;
        Ok(Redeemed::Token(Box::new(token), secret))
    }
}

fn classify(found: Option<Invite>, now: i64) -> InviteState {
    match found {
        None => InviteState::NotFound,
        Some(i) if i.claimed_at.is_some() => InviteState::Claimed,
        Some(i) if i.expires_at <= now => InviteState::Expired,
        Some(i) => InviteState::Pending(Box::new(i)),
    }
}

fn row_to_invite(r: &Row<'_>) -> rusqlite::Result<Result<Invite>> {
    let mode: String = r.get(2)?;
    let agents: String = r.get(3)?;
    let accounts: String = r.get(14)?;
    let build = || -> Result<Invite> {
        Ok(Invite {
            id: r.get(0)?,
            label: r.get(1)?,
            session_mode: mode.parse().map_err(StoreError::Corrupt)?,
            agents: token::decode_agents(&agents),
            token_expires_in_days: r.get(4)?,
            limits: Limits {
                daily_requests: r.get(5)?,
                monthly_requests: r.get(6)?,
                daily_budget_usd: r.get(7)?,
                monthly_budget_usd: r.get(8)?,
                max_pending: r.get(9)?,
            },
            expires_at: r.get(10)?,
            created_at: r.get(11)?,
            claimed_at: r.get(12)?,
            token_id: r.get(13)?,
            accounts: acct::decode(&accounts),
            project: r.get(15)?,
        })
    };
    Ok(build())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_invite(link_hours: u32) -> NewInvite {
        NewInvite {
            label: "alice".into(),
            session_mode: SessionMode::Stateful,
            agents: vec![AgentKind::Claude],
            token_expires_in_days: Some(7),
            limits: Limits {
                daily_requests: Some(25),
                max_pending: Some(3),
                ..Limits::default()
            },
            link_hours,
            accounts: AccountMap::new(),
            project: None,
        }
    }

    #[test]
    fn redeem_refuses_once_the_token_cap_is_reached() {
        let s = Store::open_in_memory().unwrap();
        let (_, code) = s.create_invite(&new_invite(24)).unwrap();
        for i in 0..super::super::MAX_TOKENS {
            s.create_token(&NewToken {
                name: format!("t{i}"),
                role: Role::User,
                session_mode: SessionMode::Stateless,
                agents: vec![AgentKind::Claude],
                expires_in_days: None,
                note: None,
                limits: Limits::default(),
                accounts: AccountMap::new(),
                project: None,
            })
            .unwrap();
        }
        assert!(matches!(
            s.redeem_invite(&code).unwrap(),
            Redeemed::TokenLimit
        ));
        assert!(matches!(
            s.preview_invite(&code).unwrap(),
            InviteState::Pending(_)
        ));
    }

    #[test]
    fn prune_invites_keeps_pending_and_recent() {
        let s = Store::open_in_memory().unwrap();
        let (old_claimed, code) = s.create_invite(&new_invite(24)).unwrap();
        assert!(matches!(
            s.redeem_invite(&code).unwrap(),
            Redeemed::Token(..)
        ));
        let (old_expired, _) = s.create_invite(&new_invite(1)).unwrap();
        let (pending, _) = s.create_invite(&new_invite(24)).unwrap();
        let (recent_claimed, code) = s.create_invite(&new_invite(24)).unwrap();
        assert!(matches!(
            s.redeem_invite(&code).unwrap(),
            Redeemed::Token(..)
        ));
        {
            let conn = s.conn().unwrap();
            conn.execute(
                "UPDATE invites SET claimed_at = 5 WHERE id = ?1",
                [&old_claimed.id],
            )
            .unwrap();
            conn.execute(
                "UPDATE invites SET expires_at = 5 WHERE id = ?1",
                [&old_expired.id],
            )
            .unwrap();
            // A pending invite is kept even when its row is old.
            conn.execute(
                "UPDATE invites SET created_at = 5 WHERE id = ?1",
                [&pending.id],
            )
            .unwrap();
        }
        assert_eq!(s.prune_invites(1_000).unwrap(), 2);
        let left: Vec<String> = s
            .list_invites()
            .unwrap()
            .into_iter()
            .map(|i| i.id)
            .collect();
        assert_eq!(left.len(), 2);
        assert!(left.contains(&pending.id) && left.contains(&recent_claimed.id));
    }

    #[test]
    fn preview_does_not_consume_the_code() {
        let s = Store::open_in_memory().unwrap();
        let (_, code) = s.create_invite(&new_invite(24)).unwrap();
        assert!(matches!(
            s.preview_invite(&code).unwrap(),
            InviteState::Pending(_)
        ));
        assert!(matches!(
            s.preview_invite(&code).unwrap(),
            InviteState::Pending(_)
        ));
    }

    #[test]
    fn redeem_is_single_use_and_copies_the_template() {
        let s = Store::open_in_memory().unwrap();
        let (invite, code) = s.create_invite(&new_invite(24)).unwrap();
        let Redeemed::Token(token, secret) = s.redeem_invite(&code).unwrap() else {
            panic!("first redeem should create a token");
        };
        assert_eq!(token.role, Role::User);
        assert_eq!(token.limits.daily_requests, Some(25));
        assert_eq!(token.agents, vec![AgentKind::Claude]);
        assert!(s.authenticate(&secret.raw).unwrap().is_some());
        assert!(matches!(s.redeem_invite(&code).unwrap(), Redeemed::Claimed));
        let stored = s
            .list_invites()
            .unwrap()
            .into_iter()
            .find(|i| i.id == invite.id)
            .unwrap();
        assert_eq!(stored.token_id.as_deref(), Some(token.id.as_str()));
        assert_eq!(s.list_tokens().unwrap().len(), 1);
    }

    #[test]
    fn expired_invite_cannot_be_redeemed() {
        let s = Store::open_in_memory().unwrap();
        let (_, code) = s.create_invite(&new_invite(0)).unwrap();
        assert!(matches!(s.redeem_invite(&code).unwrap(), Redeemed::Expired));
        assert!(s.list_tokens().unwrap().is_empty());
    }

    #[test]
    fn unknown_code_is_not_found() {
        let s = Store::open_in_memory().unwrap();
        assert!(matches!(
            s.redeem_invite("hashi_inv_nope").unwrap(),
            Redeemed::NotFound
        ));
    }

    #[test]
    fn cancel_only_pending_invites() {
        let s = Store::open_in_memory().unwrap();
        let (pending, _) = s.create_invite(&new_invite(24)).unwrap();
        let (claimed, code) = s.create_invite(&new_invite(24)).unwrap();
        s.redeem_invite(&code).unwrap();
        assert!(s.cancel_invite(&pending.id).unwrap());
        assert!(!s.cancel_invite(&claimed.id).unwrap());
    }
}
