//! Append-only record of sharing and access changes.

use rusqlite::params;
use serde::Serialize;

use super::{Result, Store, StoreError};
use crate::util::now_ms;

/// One audit record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    /// When it happened, Unix ms.
    pub at: i64,
    /// Who did it: a token name, `cli` or `dashboard`.
    pub actor: String,
    /// What happened, for example `token.update`.
    pub action: String,
    /// What it happened to, usually a token or invite name.
    pub target: String,
    /// Short human-readable detail. Never contains secrets.
    pub detail: String,
}

impl Store {
    /// Appends an audit record.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn audit(&self, actor: &str, action: &str, target: &str, detail: &str) -> Result<()> {
        self.conn()?.execute(
            "INSERT INTO audit (at, actor, action, target, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![now_ms(), actor, action, target, detail],
        )?;
        Ok(())
    }

    /// The most recent records, newest first.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn list_audit(&self, limit: u32) -> Result<Vec<AuditEntry>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT at, actor, action, target, detail FROM audit ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], |r| {
            Ok(AuditEntry {
                at: r.get(0)?,
                actor: r.get(1)?,
                action: r.get(2)?,
                target: r.get(3)?,
                detail: r.get(4)?,
            })
        })?;
        rows.map(|r| r.map_err(StoreError::from)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_lists_newest_first() {
        let s = Store::open_in_memory().unwrap();
        s.audit("cli", "token.create", "alice", "user").unwrap();
        s.audit("dashboard", "token.update", "alice", "paused")
            .unwrap();
        let entries = s.list_audit(10).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].action, "token.update");
        assert_eq!(s.list_audit(1).unwrap().len(), 1);
    }
}
