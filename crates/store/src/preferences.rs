//! What the person chose, kept between runs.
//!
//! Small, named things that are neither content nor state a job depends on:
//! which speech runtime to start, which model is selected. Kept here rather
//! than in a file beside the database, because a choice that disagrees with the
//! records is a choice nothing can act on.

use rusqlite::{params, OptionalExtension};

use crate::{Result, Store};

/// Which runtime to start, by its id. Absent means the first that works.
pub const RUNTIME: &str = "runtime";

impl Store {
    pub fn preference(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .raw()
            .query_row(
                "SELECT value FROM preferences WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Set one, or clear it. Clearing is not the same as setting an empty
    /// string: one means "whatever the application would pick", the other
    /// means a choice of nothing, and only the first is a thing to want.
    pub fn set_preference(&self, key: &str, value: Option<&str>, at: &str) -> Result<()> {
        match value {
            Some(value) => self.raw().execute(
                "INSERT INTO preferences (key, value, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value,
                                                updated_at = excluded.updated_at",
                params![key, value, at],
            )?,
            None => self
                .raw()
                .execute("DELETE FROM preferences WHERE key = ?1", params![key])?,
        };
        Ok(())
    }
}
