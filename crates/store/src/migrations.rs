//! Bringing a database up to the schema this build expects.
//!
//! `user_version` rather than a table of applied migrations: SQLite keeps it in
//! the header, it costs nothing to read, and a migration and the version it
//! sets move together inside one transaction — so a database is never at a
//! version it does not have the schema for.

use rusqlite::Connection;

use crate::Result;

/// Each step, in order. Index plus one is the `user_version` it produces.
const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_initial.sql"),
    include_str!("../migrations/0002_execution_outputs.sql"),
    include_str!("../migrations/0003_voice_duration.sql"),
    include_str!("../migrations/0004_reference_text.sql"),
    include_str!("../migrations/0005_consent_fingerprint.sql"),
    include_str!("../migrations/0006_preferences.sql"),
];

pub fn apply(connection: &mut Connection) -> Result<()> {
    let current: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    for (index, statements) in MIGRATIONS.iter().enumerate() {
        let version = index as i64 + 1;
        if version <= current {
            continue;
        }
        let transaction = connection.transaction()?;
        transaction.execute_batch(statements)?;
        // Not a bound parameter: PRAGMA does not take one.
        transaction.execute_batch(&format!("PRAGMA user_version = {version};"))?;
        transaction.commit()?;
    }
    Ok(())
}

pub fn version(connection: &Connection) -> Result<i64> {
    Ok(connection.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}
