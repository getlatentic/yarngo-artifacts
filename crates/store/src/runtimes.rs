//! Which runtime versions are installed, and which one answers.
//!
//! The directories never move: a version is installed at its final immutable
//! path or it is not installed. What changes is this table — a version becomes
//! `ready` once an engine has answered a handshake from its own directory, and
//! becomes the active one in a single row change. So there is exactly one
//! active version per runtime at every instant, including the instant a crash
//! happens.

use rusqlite::{params, OptionalExtension};

use crate::{Result, Store};

/// One installed (or installing) version of one runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledRuntime {
    pub id: String,
    pub version: String,
    pub ready: bool,
    pub engine_api: u32,
    pub installed_at: String,
}

/// The preference key naming the active version of one runtime.
fn active_key(id: &str) -> String {
    format!("runtime.{id}.active")
}

impl Store {
    /// A version begins existing here, as debris-until-proven: a crash before
    /// [`Store::runtime_ready`] leaves this row, and startup sweeps it.
    pub fn runtime_installing(&self, id: &str, version: &str, engine_api: u32, at: &str) -> Result<()> {
        self.raw().execute(
            "INSERT INTO runtimes (id, version, state, engine_api, installed_at)
             VALUES (?1, ?2, 'installing', ?3, ?4)
             ON CONFLICT(id, version) DO UPDATE SET
                 state = 'installing', engine_api = excluded.engine_api,
                 installed_at = excluded.installed_at, ready_at = NULL",
            params![id, version, engine_api, at],
        )?;
        Ok(())
    }

    /// The engine answered a handshake from its own directory.
    pub fn runtime_ready(&self, id: &str, version: &str, at: &str) -> Result<()> {
        let changed = self.raw().execute(
            "UPDATE runtimes SET state = 'ready', ready_at = ?3
             WHERE id = ?1 AND version = ?2",
            params![id, version, at],
        )?;
        if changed == 0 {
            return Err(crate::StoreError::Invalid(format!(
                "{id} {version} is not installing, so it cannot become ready"
            )));
        }
        Ok(())
    }

    /// Make one ready version the one that answers. One row changes inside a
    /// transaction, so a crash leaves either the old choice or the new one —
    /// never both, never neither.
    pub fn activate_runtime(&self, id: &str, version: &str, at: &str) -> Result<()> {
        let transaction = self.raw().unchecked_transaction()?;
        let ready: Option<String> = transaction
            .query_row(
                "SELECT state FROM runtimes WHERE id = ?1 AND version = ?2",
                params![id, version],
                |row| row.get(0),
            )
            .optional()?;
        if ready.as_deref() != Some("ready") {
            return Err(crate::StoreError::Invalid(format!(
                "{id} {version} is not ready, and what is not ready does not become active"
            )));
        }
        transaction.execute(
            "INSERT INTO preferences (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value,
                                            updated_at = excluded.updated_at",
            params![active_key(id), version, at],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// The version of `id` that answers, if one does.
    pub fn active_runtime(&self, id: &str) -> Result<Option<String>> {
        self.preference(&active_key(id))
    }

    /// Every version this database knows about, for `id` or for all of them.
    pub fn installed_runtimes(&self, id: Option<&str>) -> Result<Vec<InstalledRuntime>> {
        let mut rows = Vec::new();
        let mut push = |row: &rusqlite::Row<'_>| -> rusqlite::Result<()> {
            rows.push(InstalledRuntime {
                id: row.get(0)?,
                version: row.get(1)?,
                ready: row.get::<_, String>(2)? == "ready",
                engine_api: row.get(3)?,
                installed_at: row.get(4)?,
            });
            Ok(())
        };
        match id {
            Some(id) => {
                let mut statement = self.raw().prepare(
                    "SELECT id, version, state, engine_api, installed_at
                       FROM runtimes WHERE id = ?1 ORDER BY installed_at, version",
                )?;
                let mut found = statement.query(params![id])?;
                while let Some(row) = found.next()? {
                    push(row)?;
                }
            }
            None => {
                let mut statement = self.raw().prepare(
                    "SELECT id, version, state, engine_api, installed_at
                       FROM runtimes ORDER BY id, installed_at, version",
                )?;
                let mut found = statement.query([])?;
                while let Some(row) = found.next()? {
                    push(row)?;
                }
            }
        }
        Ok(rows)
    }

    /// Forget one version. Refused for the active one: the pointer never dangles
    /// by way of this call — activate something else first.
    pub fn remove_runtime(&self, id: &str, version: &str) -> Result<()> {
        if self.active_runtime(id)?.as_deref() == Some(version) {
            return Err(crate::StoreError::Invalid(format!(
                "{id} {version} is active; activate another version before removing it"
            )));
        }
        self.raw().execute(
            "DELETE FROM runtimes WHERE id = ?1 AND version = ?2",
            params![id, version],
        )?;
        Ok(())
    }

    /// The active pointer names a version that stopped existing or stopped
    /// working. Point at `instead`, or at nothing.
    pub fn repoint_runtime(&self, id: &str, instead: Option<&str>, at: &str) -> Result<()> {
        match instead {
            Some(version) => self.activate_runtime(id, version, at),
            None => {
                self.raw()
                    .execute("DELETE FROM preferences WHERE key = ?1", params![active_key(id)])?;
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(&dir.path().join("test.db")).expect("store");
        (dir, store)
    }

    #[test]
    fn what_is_not_ready_does_not_become_active() {
        let (_dir, store) = store();
        store.runtime_installing("mlx", "1.0.0", 1, "t1").expect("row");
        let refused = store.activate_runtime("mlx", "1.0.0", "t2");
        assert!(refused.is_err(), "an installing version was activated");
        assert_eq!(store.active_runtime("mlx").expect("read"), None);

        store.runtime_ready("mlx", "1.0.0", "t3").expect("ready");
        store.activate_runtime("mlx", "1.0.0", "t4").expect("activate");
        assert_eq!(store.active_runtime("mlx").expect("read").as_deref(), Some("1.0.0"));
    }

    #[test]
    fn what_never_installed_cannot_be_marked_ready() {
        let (_dir, store) = store();
        assert!(
            store.runtime_ready("mlx", "9.9.9", "t1").is_err(),
            "a version with no row became ready"
        );
    }

    /// The pointer must never dangle by way of removal — the failure mode
    /// where "uninstall old versions" quietly uninstalls the one answering.
    #[test]
    fn the_active_version_cannot_be_removed() {
        let (_dir, store) = store();
        store.runtime_installing("mlx", "1.0.0", 1, "t1").expect("row");
        store.runtime_ready("mlx", "1.0.0", "t1").expect("ready");
        store.activate_runtime("mlx", "1.0.0", "t1").expect("activate");

        assert!(store.remove_runtime("mlx", "1.0.0").is_err(), "the active version was removed");
        assert_eq!(store.active_runtime("mlx").expect("read").as_deref(), Some("1.0.0"));

        store.runtime_installing("mlx", "2.0.0", 1, "t2").expect("row");
        store.runtime_ready("mlx", "2.0.0", "t2").expect("ready");
        store.activate_runtime("mlx", "2.0.0", "t2").expect("switch");
        store.remove_runtime("mlx", "1.0.0").expect("now removable");
    }

    /// Activation is one transaction: after any sequence of activations there
    /// is exactly one active version, and it is one that was ready.
    #[test]
    fn there_is_exactly_one_active_version_at_every_step() {
        let (_dir, store) = store();
        for version in ["1.0.0", "2.0.0", "3.0.0"] {
            store.runtime_installing("mlx", version, 1, "t").expect("row");
            store.runtime_ready("mlx", version, "t").expect("ready");
            store.activate_runtime("mlx", version, "t").expect("activate");
            assert_eq!(
                store.active_runtime("mlx").expect("read").as_deref(),
                Some(version),
                "after activating {version}"
            );
        }
    }

    #[test]
    fn two_runtimes_hold_their_versions_apart() {
        let (_dir, store) = store();
        for (id, version) in [("mlx", "1.0.0"), ("torch", "0.5.0")] {
            store.runtime_installing(id, version, 1, "t").expect("row");
            store.runtime_ready(id, version, "t").expect("ready");
            store.activate_runtime(id, version, "t").expect("activate");
        }
        assert_eq!(store.active_runtime("mlx").expect("read").as_deref(), Some("1.0.0"));
        assert_eq!(store.active_runtime("torch").expect("read").as_deref(), Some("0.5.0"));
        assert_eq!(store.installed_runtimes(None).expect("all").len(), 2);
    }
}
