//! Opening the database, and the settings that must hold on every connection.

use std::path::Path;

use rusqlite::Connection;

use crate::{migrations, Result, StoreError};

pub struct Store {
    connection: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        // The directory before the file. On a machine that has never run this
        // application there is nothing here at all, and rusqlite reports that
        // as "unable to open database file" — which reads as a corrupt store
        // rather than an empty disk. Every caller wants the same thing, so it
        // happens here rather than in each of them.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                StoreError::Invalid(format!("cannot create {}: {e}", parent.display()))
            })?;
        }
        Self::prepare(Connection::open(path)?)
    }

    /// For tests, and for validating an import before anything is kept.
    pub fn in_memory() -> Result<Self> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(connection: Connection) -> Result<Self> {
        // Off by default in SQLite, and per connection rather than per
        // database: a schema full of references that are not enforced is a
        // schema that documents an intention.
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        let mut store = Self { connection };
        migrations::apply(&mut store.connection)?;
        Ok(store)
    }

    pub fn raw(&self) -> &Connection {
        &self.connection
    }

    pub fn raw_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }
}

#[cfg(test)]
mod tests {

    /// A machine that has never run this application has no directory to put a
    /// database in, and the failure it produced read as a corrupt store rather
    /// than an empty disk. Found by installing on one.
    #[test]
    fn a_store_opens_where_nothing_has_ever_been() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let never = scratch.path().join("Application Support/Yarngo Studio/yarngo.db");
        assert!(!never.parent().expect("parent").exists());

        crate::Store::open(&never).expect("a first run must not need a directory to exist");
        assert!(never.exists(), "no database was created");

        // And opening it again is the ordinary case, not a second first run.
        crate::Store::open(&never).expect("reopen");
    }
}
