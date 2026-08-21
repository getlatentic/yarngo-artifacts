//! Opening the database, and the settings that must hold on every connection.

use std::path::Path;

use rusqlite::Connection;

use crate::{migrations, Result};

pub struct Store {
    connection: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
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
