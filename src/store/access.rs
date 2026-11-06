//! Access requests: the public ask-access link, requests from new people,
//! and requests for more from existing tokens.
//!
//! Neither secret is stored. The link code and the pickup secret are kept
//! only as SHA-256 hashes, and the invite an accepted request creates gets a
//! code derived from the pickup secret, of which only the hash is kept too.
//! Only the friend holding the pickup secret can ever learn the claim URL.

use rusqlite::{OptionalExtension, Row, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use super::{Result, Store, StoreError};
use crate::agent::AgentKind;
use crate::token;
use crate::util::now_ms;

/// Prefix of ask-access link codes (128 bits of randomness follow).
pub const LINK_PREFIX: &str = "hashi_ask_";
/// Prefix of pickup secrets (256 bits of randomness follow).
pub const PICKUP_PREFIX: &str = "hashi_pick_";
/// Pending requests one link may hold.
pub const MAX_PENDING_PER_LINK: usize = 50;
/// Pending new-person requests the server holds in total.
pub const MAX_PENDING_TOTAL: usize = 200;
/// Pending "more" requests from token holders the server holds in total.
/// Counted apart from new-person requests, so anonymous requesters filling
/// [`MAX_PENDING_TOTAL`] never stop a token holder from asking for more.
pub const MAX_PENDING_MORE: usize = 200;
/// Open requests one token may hold.
pub const MAX_OPEN_PER_TOKEN: usize = 3;
/// A token may open one new request per this many milliseconds.
pub const MORE_EVERY_MS: i64 = 3_600_000;
/// A request left undecided this many days expires.
pub const REQUEST_DAYS: i64 = 7;

/// The invite code an accepted request's pickup secret unlocks. It is
/// derived, so the database never holds it.
#[must_use]
pub fn claim_code_for(pickup: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"hashi-access-claim\0");
    h.update(pickup.as_bytes());
    format!("{}{}", super::INVITE_PREFIX, hex::encode(h.finalize()))
}

/// The public link that lets people ask for access.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessLink {
    /// Stable identifier.
    pub id: String,
    /// The project new requests are for, by name.
    pub project: Option<String>,
    /// The owner's note shown on the page.
    pub note: Option<String>,
    /// Creation time, Unix ms.
    pub created_at: i64,
    /// When it closes by itself, Unix ms.
    pub expires_at: i64,
    /// When it was closed, Unix ms.
    pub closed_at: Option<i64>,
    /// Who opened it.
    pub created_by: String,
}

impl AccessLink {
    /// `open`, `closed` or `expired`.
    #[must_use]
    pub fn status(&self, now_ms: i64) -> &'static str {
        if self.closed_at.is_some() {
            "closed"
        } else if self.expires_at <= now_ms {
            "expired"
        } else {
            "open"
        }
    }
}

/// Who is asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccessKind {
    /// A new person, through the public link.
    New,
    /// An existing token, asking for more.
    More,
}

impl AccessKind {
    /// Stored and shown form.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::More => "more",
        }
    }
}

/// What a token asked for. Every field is optional.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MoreAsk {
    /// Prompts per UTC day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_requests: Option<u32>,
    /// Prompts per UTC month.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monthly_requests: Option<u32>,
    /// Spend per UTC day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_budget_usd: Option<f64>,
    /// Spend per UTC month.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monthly_budget_usd: Option<f64>,
    /// A new lifetime, counted from when it is accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_days: Option<u32>,
}

impl MoreAsk {
    /// Whether nothing is asked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// A stored request. Never contains a secret.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessRequest {
    /// Stable identifier.
    pub id: String,
    /// New person or more.
    pub kind: AccessKind,
    /// The link it came through (new people).
    pub link_id: Option<String>,
    /// The token asking (more).
    pub token_id: Option<String>,
    /// The requester's name, or the token's.
    pub name: String,
    /// Free text from the requester.
    pub message: String,
    /// Agents wanted (new) or added (more).
    pub agents: Vec<AgentKind>,
    /// The link's project (new) or the token's (more).
    pub project: Option<String>,
    /// Higher limits or a later expiry (more).
    pub asked: MoreAsk,
    /// `pending`, `accepted` or `declined`, as stored. See [`Self::state`].
    pub status: String,
    /// The owner's reason for declining.
    pub reason: Option<String>,
    /// The invite created on acceptance (new).
    pub invite_id: Option<String>,
    /// Creation time, Unix ms.
    pub created_at: i64,
    /// When it expires if nobody decides, Unix ms.
    pub expires_at: i64,
    /// When it was decided, Unix ms.
    pub decided_at: Option<i64>,
    /// Who decided it.
    pub decided_by: Option<String>,
}

impl AccessRequest {
    /// `pending`, `accepted`, `declined` or `expired`.
    #[must_use]
    pub fn state(&self, now_ms: i64) -> &'static str {
        match self.status.as_str() {
            "accepted" => "accepted",
            "declined" => "declined",
            _ if self.expires_at <= now_ms => "expired",
            _ => "pending",
        }
    }

    /// Whether the owner can still decide it.
    #[must_use]
    pub fn is_pending(&self, now_ms: i64) -> bool {
        self.state(now_ms) == "pending"
    }
}

/// Input for [`Store::insert_access_request`]. Fields must already be
/// checked and cleaned.
#[derive(Debug, Clone)]
pub struct NewAccessRequest {
    /// New person or more.
    pub kind: AccessKind,
    /// The link (new).
    pub link_id: Option<String>,
    /// The token (more).
    pub token_id: Option<String>,
    /// Name.
    pub name: String,
    /// Message.
    pub message: String,
    /// Agents.
    pub agents: Vec<AgentKind>,
    /// Project name.
    pub project: Option<String>,
    /// Asked limits (more).
    pub asked: MoreAsk,
}

/// Outcome of [`Store::insert_access_request`].
#[derive(Debug)]
pub enum AccessAdmission {
    /// Stored. A new person's request comes with its one-time pickup secret.
    Created(Box<AccessRequest>, Option<String>),
    /// The link already holds [`MAX_PENDING_PER_LINK`] pending requests.
    LinkFull,
    /// The server already holds [`MAX_PENDING_TOTAL`] pending new-person
    /// requests, or [`MAX_PENDING_MORE`] pending "more" requests.
    TotalFull,
    /// The token already has [`MAX_OPEN_PER_TOKEN`] open requests.
    TokenBusy,
    /// The token opened one less than [`MORE_EVERY_MS`] ago; the next one
    /// can be sent after this long.
    TooSoon(std::time::Duration),
}

/// What a pickup secret currently points at.
#[derive(Debug, Clone, PartialEq)]
pub struct Pickup {
    /// The request.
    pub request: AccessRequest,
    /// The claim code, derived from the secret, when the request was
    /// accepted.
    pub claim_code: Option<String>,
}

const LINK_COLUMNS: &str = "id, project, note, created_at, expires_at, closed_at, created_by";
const REQUEST_COLUMNS: &str = "id, kind, link_id, token_id, name, message, agents, project, \
     asked, status, reason, invite_id, created_at, expires_at, decided_at, decided_by";

/// Shown text is cleaned again on the way out, so a row written by an
/// older or buggy build still prints safely.
fn shown(s: String, keep_lines: bool) -> String {
    crate::access::clean_text(&s, keep_lines)
}

fn row_to_link(r: &Row<'_>) -> rusqlite::Result<AccessLink> {
    Ok(AccessLink {
        id: r.get(0)?,
        project: r.get(1)?,
        note: r.get::<_, Option<String>>(2)?.map(|n| shown(n, false)),
        created_at: r.get(3)?,
        expires_at: r.get(4)?,
        closed_at: r.get(5)?,
        created_by: r.get(6)?,
    })
}

fn row_to_request(r: &Row<'_>) -> rusqlite::Result<Result<AccessRequest>> {
    let kind: String = r.get(1)?;
    let agents: String = r.get(6)?;
    let asked: String = r.get(8)?;
    let build = || -> Result<AccessRequest> {
        Ok(AccessRequest {
            id: r.get(0)?,
            kind: match kind.as_str() {
                "new" => AccessKind::New,
                "more" => AccessKind::More,
                other => return Err(StoreError::Corrupt(format!("access kind {other}"))),
            },
            link_id: r.get(2)?,
            token_id: r.get(3)?,
            name: shown(r.get(4)?, false),
            message: shown(r.get(5)?, true),
            agents: token::decode_agents(&agents),
            project: r.get(7)?,
            asked: serde_json::from_str(&asked).unwrap_or_default(),
            status: r.get(9)?,
            reason: r.get::<_, Option<String>>(10)?.map(|s| shown(s, true)),
            invite_id: r.get(11)?,
            created_at: r.get(12)?,
            expires_at: r.get(13)?,
            decided_at: r.get(14)?,
            decided_by: r.get(15)?,
        })
    };
    Ok(build())
}

fn count(conn: &rusqlite::Connection, sql: &str, p: impl rusqlite::Params) -> Result<usize> {
    let n: i64 = conn.query_row(sql, p, |r| r.get(0))?;
    Ok(usize::try_from(n).unwrap_or(usize::MAX))
}

impl Store {
    /// Opens a new ask-access link for `hours`, closing any link that is
    /// still open, and returns it with its one-time code.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn open_access_link(
        &self,
        project: Option<&str>,
        note: Option<&str>,
        hours: u32,
        actor: &str,
    ) -> Result<(AccessLink, String)> {
        let bytes: [u8; 16] = rand::random();
        let code = format!("{LINK_PREFIX}{}", hex::encode(bytes));
        let now = now_ms();
        let link = AccessLink {
            id: uuid::Uuid::new_v4().to_string(),
            project: project.map(str::to_owned),
            note: note.map(str::to_owned),
            created_at: now,
            expires_at: now + i64::from(hours) * 3_600_000,
            closed_at: None,
            created_by: actor.to_owned(),
        };
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE access_links SET closed_at = ?1 WHERE closed_at IS NULL",
            [now],
        )?;
        tx.execute(
            "INSERT INTO access_links (id, hash, project, note, created_at, expires_at, created_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                link.id,
                token::hash(&code),
                link.project,
                link.note,
                link.created_at,
                link.expires_at,
                link.created_by,
            ],
        )?;
        tx.commit()?;
        Ok((link, code))
    }

    /// Closes every open link. Returns how many were open and unexpired.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn close_access_links(&self) -> Result<usize> {
        let now = now_ms();
        let conn = self.conn()?;
        let live = count(
            &conn,
            "SELECT COUNT(*) FROM access_links WHERE closed_at IS NULL AND expires_at > ?1",
            [now],
        )?;
        conn.execute(
            "UPDATE access_links SET closed_at = ?1 WHERE closed_at IS NULL",
            [now],
        )?;
        Ok(live)
    }

    /// The most recent link, whatever its state.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn latest_access_link(&self) -> Result<Option<AccessLink>> {
        let sql = format!(
            "SELECT {LINK_COLUMNS} FROM access_links ORDER BY created_at DESC, rowid DESC LIMIT 1"
        );
        Ok(self.conn()?.query_row(&sql, [], row_to_link).optional()?)
    }

    /// The link a code belongs to, whatever its state.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn find_access_link(&self, code: &str) -> Result<Option<AccessLink>> {
        let sql = format!("SELECT {LINK_COLUMNS} FROM access_links WHERE hash = ?1");
        Ok(self
            .conn()?
            .query_row(&sql, [token::hash(code)], row_to_link)
            .optional()?)
    }

    /// Stores a request after checking the pending caps, all in one
    /// transaction so concurrent submissions cannot overshoot them.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn insert_access_request(&self, new: &NewAccessRequest) -> Result<AccessAdmission> {
        let now = now_ms();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pending = "status = 'pending' AND expires_at > ?1";
        // New-person and "more" requests have caps of their own.
        let cap = match new.kind {
            AccessKind::New => MAX_PENDING_TOTAL,
            AccessKind::More => MAX_PENDING_MORE,
        };
        let total = count(
            &tx,
            &format!("SELECT COUNT(*) FROM access_requests WHERE kind = ?2 AND {pending}"),
            params![now, new.kind.as_str()],
        )?;
        if total >= cap {
            return Ok(AccessAdmission::TotalFull);
        }
        if let Some(link) = &new.link_id {
            let n = count(
                &tx,
                &format!("SELECT COUNT(*) FROM access_requests WHERE link_id = ?2 AND {pending}"),
                params![now, link],
            )?;
            if n >= MAX_PENDING_PER_LINK {
                return Ok(AccessAdmission::LinkFull);
            }
        }
        if let Some(tok) = &new.token_id {
            let n = count(
                &tx,
                &format!("SELECT COUNT(*) FROM access_requests WHERE token_id = ?2 AND {pending}"),
                params![now, tok],
            )?;
            if n >= MAX_OPEN_PER_TOKEN {
                return Ok(AccessAdmission::TokenBusy);
            }
            let last: Option<i64> = tx.query_row(
                "SELECT MAX(created_at) FROM access_requests WHERE token_id = ?1",
                [tok],
                |r| r.get(0),
            )?;
            if let Some(at) = last.filter(|at| now - at < MORE_EVERY_MS) {
                let wait = u64::try_from(MORE_EVERY_MS - (now - at)).unwrap_or(0);
                return Ok(AccessAdmission::TooSoon(std::time::Duration::from_millis(
                    wait,
                )));
            }
        }
        let pickup = (new.kind == AccessKind::New).then(|| {
            let bytes: [u8; 32] = rand::random();
            format!("{PICKUP_PREFIX}{}", hex::encode(bytes))
        });
        let req = AccessRequest {
            id: uuid::Uuid::new_v4().to_string(),
            kind: new.kind,
            link_id: new.link_id.clone(),
            token_id: new.token_id.clone(),
            name: new.name.clone(),
            message: new.message.clone(),
            agents: new.agents.clone(),
            project: new.project.clone(),
            asked: new.asked,
            status: "pending".to_owned(),
            reason: None,
            invite_id: None,
            created_at: now,
            expires_at: now + REQUEST_DAYS * 86_400_000,
            decided_at: None,
            decided_by: None,
        };
        tx.execute(
            "INSERT INTO access_requests (id, kind, link_id, token_id, name, message, agents,
                 project, asked, pickup_hash, claim_hash, status, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'pending', ?12, ?13)",
            params![
                req.id,
                req.kind.as_str(),
                req.link_id,
                req.token_id,
                req.name,
                req.message,
                token::encode_agents(&req.agents),
                req.project,
                serde_json::to_string(&req.asked).unwrap_or_else(|_| "{}".to_owned()),
                pickup.as_deref().map(token::hash),
                pickup.as_deref().map(|p| token::hash(&claim_code_for(p))),
                req.created_at,
                req.expires_at,
            ],
        )?;
        tx.commit()?;
        Ok(AccessAdmission::Created(Box::new(req), pickup))
    }

    /// All requests, newest first.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn list_access_requests(&self) -> Result<Vec<AccessRequest>> {
        let conn = self.conn()?;
        let sql = format!(
            "SELECT {REQUEST_COLUMNS} FROM access_requests ORDER BY created_at DESC, rowid DESC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], row_to_request)?;
        rows.map(|r| r.map_err(StoreError::from).and_then(|x| x))
            .collect()
    }

    /// One token's requests, newest first.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn access_requests_for_token(&self, token_id: &str) -> Result<Vec<AccessRequest>> {
        let conn = self.conn()?;
        let sql = format!(
            "SELECT {REQUEST_COLUMNS} FROM access_requests WHERE token_id = ?1
             ORDER BY created_at DESC, rowid DESC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([token_id], row_to_request)?;
        rows.map(|r| r.map_err(StoreError::from).and_then(|x| x))
            .collect()
    }

    /// Requests the owner can still decide.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn pending_access_count(&self) -> Result<usize> {
        let conn = self.conn()?;
        count(
            &conn,
            "SELECT COUNT(*) FROM access_requests WHERE status = 'pending' AND expires_at > ?1",
            [now_ms()],
        )
    }

    /// One request by id.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn get_access_request(&self, id: &str) -> Result<Option<AccessRequest>> {
        let sql = format!("SELECT {REQUEST_COLUMNS} FROM access_requests WHERE id = ?1");
        self.conn()?
            .query_row(&sql, [id], row_to_request)
            .optional()?
            .transpose()
    }

    /// The request behind a pickup secret, with its claim code once it was
    /// accepted.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn pickup_access_request(&self, pickup: &str) -> Result<Option<Pickup>> {
        let sql = format!("SELECT {REQUEST_COLUMNS} FROM access_requests WHERE pickup_hash = ?1");
        let found = self
            .conn()?
            .query_row(&sql, [token::hash(pickup)], row_to_request)
            .optional()?
            .transpose()?;
        Ok(found.map(|request| {
            let claim_code = (request.status == "accepted").then(|| claim_code_for(pickup));
            Pickup {
                request,
                claim_code,
            }
        }))
    }

    /// The stored hash of the claim code a new person's request unlocks.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn access_claim_hash(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT claim_hash FROM access_requests WHERE id = ?1",
                [id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Marks a pending request decided. Returns `false` when it was no
    /// longer pending (decided meanwhile, or expired).
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn decide_access_request(
        &self,
        id: &str,
        accepted: bool,
        reason: Option<&str>,
        invite_id: Option<&str>,
        actor: &str,
    ) -> Result<bool> {
        let now = now_ms();
        let n = self.conn()?.execute(
            "UPDATE access_requests
             SET status = ?2, reason = ?3, invite_id = ?4, decided_at = ?5, decided_by = ?6
             WHERE id = ?1 AND status = 'pending' AND expires_at > ?5",
            params![
                id,
                if accepted { "accepted" } else { "declined" },
                reason,
                invite_id,
                now,
                actor
            ],
        )?;
        Ok(n == 1)
    }

    /// Deletes requests decided before `before_ms`, requests that expired
    /// before it, and links closed or expired before it that no request
    /// refers to any more. Pending requests are never removed.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn prune_access(&self, before_ms: i64) -> Result<usize> {
        let conn = self.conn()?;
        let n = conn.execute(
            "DELETE FROM access_requests
             WHERE (status != 'pending' AND decided_at < ?1)
                OR (status = 'pending' AND expires_at < ?1)",
            [before_ms],
        )?;
        conn.execute(
            "DELETE FROM access_links
             WHERE (closed_at < ?1 OR expires_at < ?1)
               AND NOT EXISTS (SELECT 1 FROM access_requests r WHERE r.link_id = access_links.id)",
            [before_ms],
        )?;
        Ok(n)
    }

    /// Moves every access request and link `days` into the past.
    #[cfg(test)]
    pub(crate) fn backdate_access_for_tests(&self, days: i64) {
        let d = days * 86_400_000;
        let conn = self.conn().unwrap();
        conn.execute(
            "UPDATE access_requests SET created_at = created_at - ?1,
                 expires_at = expires_at - ?1, decided_at = decided_at - ?1",
            [d],
        )
        .unwrap();
        conn.execute(
            "UPDATE access_links SET created_at = created_at - ?1,
                 expires_at = expires_at - ?1, closed_at = closed_at - ?1",
            [d],
        )
        .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_person(link: &str, name: &str) -> NewAccessRequest {
        NewAccessRequest {
            kind: AccessKind::New,
            link_id: Some(link.to_owned()),
            token_id: None,
            name: name.to_owned(),
            message: String::new(),
            agents: vec![AgentKind::Claude],
            project: None,
            asked: MoreAsk::default(),
        }
    }

    #[test]
    fn link_codes_are_hashed_and_128_bits() {
        let s = Store::open_in_memory().unwrap();
        let (link, code) = s.open_access_link(None, Some("hi"), 1, "cli").unwrap();
        assert_eq!(code.len(), LINK_PREFIX.len() + 32);
        assert_eq!(s.find_access_link(&code).unwrap().unwrap().id, link.id);
        let stored: String = s
            .conn()
            .unwrap()
            .query_row("SELECT hash FROM access_links", [], |r| r.get(0))
            .unwrap();
        assert_ne!(stored, code);
        assert!(s.find_access_link("hashi_ask_nope").unwrap().is_none());
    }

    #[test]
    fn opening_a_link_closes_the_previous_one() {
        let s = Store::open_in_memory().unwrap();
        let (_, first) = s.open_access_link(None, None, 1, "cli").unwrap();
        let (_, second) = s.open_access_link(None, None, 1, "cli").unwrap();
        let now = now_ms();
        assert_eq!(
            s.find_access_link(&first).unwrap().unwrap().status(now),
            "closed"
        );
        assert_eq!(
            s.find_access_link(&second).unwrap().unwrap().status(now),
            "open"
        );
        assert_eq!(s.close_access_links().unwrap(), 1);
        assert_eq!(s.close_access_links().unwrap(), 0);
    }

    #[test]
    fn pending_caps_per_link_and_in_total() {
        let s = Store::open_in_memory().unwrap();
        for i in 0..MAX_PENDING_PER_LINK {
            assert!(matches!(
                s.insert_access_request(&new_person("a", &format!("p{i}")))
                    .unwrap(),
                AccessAdmission::Created(..)
            ));
        }
        assert!(matches!(
            s.insert_access_request(&new_person("a", "late")).unwrap(),
            AccessAdmission::LinkFull
        ));
        for link in ["b", "c", "d"] {
            for i in 0..MAX_PENDING_PER_LINK {
                s.insert_access_request(&new_person(link, &format!("p{i}")))
                    .unwrap();
            }
        }
        assert_eq!(s.pending_access_count().unwrap(), MAX_PENDING_TOTAL);
        assert!(matches!(
            s.insert_access_request(&new_person("e", "late")).unwrap(),
            AccessAdmission::TotalFull
        ));
        // Expired requests no longer count.
        s.backdate_access_for_tests(REQUEST_DAYS + 1);
        assert!(matches!(
            s.insert_access_request(&new_person("a", "fresh")).unwrap(),
            AccessAdmission::Created(..)
        ));
    }

    /// A request for more from a new token of its own.
    fn more_from(s: &Store) -> NewAccessRequest {
        let (token, _) = s
            .create_token(&super::super::NewToken {
                name: "t".into(),
                role: crate::token::Role::User,
                session_mode: crate::token::SessionMode::Stateful,
                agents: vec![AgentKind::Claude],
                expires_in_days: None,
                note: None,
                limits: crate::token::Limits::default(),
                accounts: crate::accounts::AccountMap::default(),
                project: None,
            })
            .unwrap();
        NewAccessRequest {
            kind: AccessKind::More,
            link_id: None,
            token_id: Some(token.id),
            name: "t".to_owned(),
            message: String::new(),
            agents: Vec::new(),
            project: None,
            asked: MoreAsk::default(),
        }
    }

    #[test]
    fn a_flood_of_new_person_requests_never_blocks_a_request_for_more() {
        let s = Store::open_in_memory().unwrap();
        for link in 0..MAX_PENDING_TOTAL / MAX_PENDING_PER_LINK {
            for i in 0..MAX_PENDING_PER_LINK {
                s.insert_access_request(&new_person(&format!("l{link}"), &format!("p{i}")))
                    .unwrap();
            }
        }
        assert!(matches!(
            s.insert_access_request(&new_person("other", "late"))
                .unwrap(),
            AccessAdmission::TotalFull
        ));
        // Tokens are capped too, and each may open one "more" request an
        // hour, so the "more" cap is reached over several rounds of every
        // token asking once, with the clock moved on between rounds.
        let tokens: Vec<NewAccessRequest> = (0..super::super::MAX_TOKENS)
            .map(|_| more_from(&s))
            .collect();
        let mut made = 0;
        'rounds: loop {
            for req in &tokens {
                if made == MAX_PENDING_MORE {
                    break 'rounds;
                }
                assert!(
                    matches!(
                        s.insert_access_request(req).unwrap(),
                        AccessAdmission::Created(..)
                    ),
                    "request {made} for more was refused"
                );
                made += 1;
            }
            s.conn()
                .unwrap()
                .execute(
                    "UPDATE access_requests SET created_at = created_at - ?1 WHERE kind = 'more'",
                    [MORE_EVERY_MS * 2],
                )
                .unwrap();
        }
        // Every token now has two open, under its own limit of three, so
        // the next one is refused only by the "more" cap.
        s.conn()
            .unwrap()
            .execute(
                "UPDATE access_requests SET created_at = created_at - ?1 WHERE kind = 'more'",
                [MORE_EVERY_MS * 2],
            )
            .unwrap();
        assert!(matches!(
            s.insert_access_request(&tokens[0]).unwrap(),
            AccessAdmission::TotalFull
        ));
    }

    #[test]
    fn pickup_returns_a_derived_code_only_once_accepted() {
        let s = Store::open_in_memory().unwrap();
        let AccessAdmission::Created(req, Some(pickup)) =
            s.insert_access_request(&new_person("a", "alice")).unwrap()
        else {
            panic!("expected a pickup secret");
        };
        assert_eq!(pickup.len(), PICKUP_PREFIX.len() + 64);
        let p = s.pickup_access_request(&pickup).unwrap().unwrap();
        assert_eq!(
            (p.request.id.as_str(), p.claim_code),
            (req.id.as_str(), None)
        );
        assert!(
            s.decide_access_request(&req.id, true, None, Some("inv"), "cli")
                .unwrap()
        );
        assert!(
            !s.decide_access_request(&req.id, false, None, None, "cli")
                .unwrap()
        );
        let p = s.pickup_access_request(&pickup).unwrap().unwrap();
        let code = p.claim_code.unwrap();
        assert!(code.starts_with(crate::store::INVITE_PREFIX));
        assert_eq!(
            s.access_claim_hash(&req.id).unwrap(),
            Some(token::hash(&code))
        );
        // Nothing in the row reveals either secret.
        let row: String = s
            .conn()
            .unwrap()
            .query_row(
                "SELECT pickup_hash || claim_hash FROM access_requests",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!row.contains(&pickup) && !row.contains(&code));
    }

    #[test]
    fn prune_keeps_pending_and_recent() {
        let s = Store::open_in_memory().unwrap();
        let mk = |n: &str| match s.insert_access_request(&new_person("a", n)).unwrap() {
            AccessAdmission::Created(r, _) => r.id,
            other => panic!("{other:?}"),
        };
        let declined = mk("d");
        let _pending = mk("p");
        s.decide_access_request(&declined, false, Some("no"), None, "cli")
            .unwrap();
        let cutoff = now_ms() - 30 * 86_400_000;
        assert_eq!(s.prune_access(cutoff).unwrap(), 0);
        // 31 days on: the declined one and the expired pending one go.
        s.backdate_access_for_tests(31);
        assert_eq!(s.prune_access(cutoff).unwrap(), 1);
        assert_eq!(s.list_access_requests().unwrap().len(), 1);
        s.backdate_access_for_tests(10);
        assert_eq!(s.prune_access(cutoff).unwrap(), 1);
        assert!(s.list_access_requests().unwrap().is_empty());
    }
}
