//! An application data directory a test can do anything to.
//!
//! Integration tests need a real store: real recordings to condition, real
//! audio to publish beside, real JSON to prove was left alone. The obvious way
//! to get one is to point at the installed application's — and that is exactly
//! how a development run came to add a clip to somebody's actual library.
//!
//! So a test gets a copy instead. Everything is copied, including the audio,
//! and every absolute path inside the copied JSON is rewritten to name the copy
//! — because a store whose records still point at the originals is not isolated
//! at all, it is a deletion test aimed at the real thing.
//!
//! The copy is deleted when the sandbox is dropped.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Set on any process that must not touch the installed application's data.
///
/// Checked where the data directory is resolved rather than trusted to be
/// honoured, so forgetting to point a test somewhere else is a failure to start
/// and not a surprise later.
pub const TEST_MODE: &str = "YARNGO_TEST_MODE";
/// Where the application keeps everything. Read by both halves, so the sidecar
/// a test spawns lands in the same copy the test is looking at.
pub const DATA_DIR: &str = "YARNGO_DATA";

pub struct Sandbox {
    root: tempfile::TempDir,
}

impl Sandbox {
    /// Nothing in it. For tests that build exactly what they need.
    pub fn empty() -> Self {
        Self { root: tempfile::tempdir().expect("a temporary directory") }
    }

    /// A copy of a real store, with its records pointing at the copy.
    ///
    /// `None` when there is nothing to copy, so a test can say it needs a
    /// machine that has run the application rather than fail as though
    /// something were broken.
    pub fn copying(source: &Path) -> Option<Self> {
        let clips = source.join("clips/clips.json");
        let voices = source.join("voices/voices.json");
        if !clips.exists() || !voices.exists() {
            return None;
        }
        let sandbox = Self::empty();
        let root = sandbox.root();

        for name in ["clips", "voices"] {
            copy_tree(&source.join(name), &root.join(name)).ok()?;
        }
        if let Ok(log) = std::fs::read(source.join("consent.log")) {
            std::fs::write(root.join("consent.log"), log).ok()?;
        }
        rewrite(&root.join("clips/clips.json"), source, root)?;
        rewrite(&root.join("voices/voices.json"), source, root)?;
        Some(sandbox)
    }

    pub fn root(&self) -> &Path {
        self.root.path()
    }

    pub fn database(&self) -> PathBuf {
        self.root().join("yarngo.db")
    }

    /// Point a child process at the copy, and mark it as a run that must not
    /// find its way back to the real one.
    pub fn apply(&self, command: &mut std::process::Command) {
        command.env(DATA_DIR, self.root()).env(TEST_MODE, "1");
    }
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Point every path in a copied record at the copy.
///
/// Rewritten in the JSON rather than resolved at read time, because the reader
/// is the application and it is entitled to believe what its own store says. A
/// record naming a file outside the sandbox is the sandbox failing.
fn rewrite(file: &Path, from: &Path, to: &Path) -> Option<()> {
    let mut document: Value = serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()?;
    retarget(&mut document, &from.to_string_lossy(), &to.to_string_lossy());
    std::fs::write(file, serde_json::to_string_pretty(&document).ok()?).ok()
}

fn retarget(value: &mut Value, from: &str, to: &str) {
    match value {
        Value::String(text) if text.starts_with(from) => {
            *text = format!("{to}{}", &text[from.len()..]);
        }
        Value::Array(items) => items.iter_mut().for_each(|item| retarget(item, from, to)),
        Value::Object(fields) => {
            fields.values_mut().for_each(|field| retarget(field, from, to))
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::Sandbox;
    use serde_json::Value;

    /// The claim the whole thing rests on. A copied store whose records still
    /// name the originals is not a sandbox — it is the real store with extra
    /// steps, and a deletion test would find that out by deleting.
    #[test]
    fn a_copied_store_names_nothing_outside_itself() {
        let source = tempfile::tempdir().expect("source");
        let clips = source.path().join("clips");
        let voices = source.path().join("voices");
        std::fs::create_dir_all(&clips).expect("clips");
        std::fs::create_dir_all(&voices).expect("voices");
        std::fs::write(clips.join("one.wav"), b"RIFF").expect("audio");
        std::fs::write(voices.join("alice.wav"), b"RIFF").expect("recording");
        std::fs::write(
            clips.join("clips.json"),
            format!(
                r#"[{{"id":"c1","takes":[{{"id":"t1","path":"{}"}}]}}]"#,
                clips.join("one.wav").display()
            ),
        )
        .expect("clips.json");
        std::fs::write(
            voices.join("voices.json"),
            format!(
                r#"{{"alice":{{"reference_audio":"{}"}}}}"#,
                voices.join("alice.wav").display()
            ),
        )
        .expect("voices.json");

        let sandbox = Sandbox::copying(source.path()).expect("a copy");
        for name in ["clips/clips.json", "voices/voices.json"] {
            let document: Value =
                serde_json::from_str(&std::fs::read_to_string(sandbox.root().join(name)).unwrap())
                    .expect("json");
            let mut paths = Vec::new();
            collect(&document, &mut paths);
            assert!(!paths.is_empty(), "{name} named no files at all");
            for path in paths {
                assert!(
                    path.starts_with(&sandbox.root().to_string_lossy().to_string()),
                    "{name} still names {path}"
                );
                assert!(std::path::Path::new(&path).exists(), "{path} was not copied");
            }
        }
        // And the original is still whole.
        assert!(clips.join("one.wav").exists());
        assert!(voices.join("alice.wav").exists());
    }

    fn collect(value: &Value, into: &mut Vec<String>) {
        match value {
            Value::String(text) if text.contains('/') => into.push(text.clone()),
            Value::Array(items) => items.iter().for_each(|item| collect(item, into)),
            Value::Object(fields) => fields.values().for_each(|field| collect(field, into)),
            _ => {}
        }
    }

    #[test]
    fn nothing_to_copy_is_said_rather_than_faked() {
        let empty = tempfile::tempdir().expect("empty");
        assert!(Sandbox::copying(empty.path()).is_none());
    }
}
