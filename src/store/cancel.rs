//! Cancelling a request: conditional updates, so a cancel never overwrites
//! a request that has already finished.

use rusqlite::params;

use super::{Result, Store};
use crate::util::now_ms;

impl Store {
    /// Fails request `id` with `error` if it is still queued. Returns whether
    /// it was.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn cancel_queued(&self, id: &str, error: &str) -> Result<bool> {
        self.fail_if(id, "queued", error)
    }

    /// Fails request `id` with `error` if it is still running. Returns whether
    /// it was. Its worker notices within [`crate::queue::ROW_CHECK_EVERY`]
    /// even without a signal, and stops the run.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn cancel_running(&self, id: &str, error: &str) -> Result<bool> {
        self.fail_if(id, "running", error)
    }

    fn fail_if(&self, id: &str, status: &str, error: &str) -> Result<bool> {
        let n = self.conn()?.execute(
            "UPDATE requests SET status = 'failed', error = ?1, completed_at = ?2
             WHERE id = ?3 AND status = ?4",
            params![error, now_ms(), id, status],
        )?;
        Ok(n > 0)
    }
}
