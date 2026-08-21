//! Where generated audio goes, decided here and nowhere else.
//!
//! The engine is told a path; it does not choose one. That is what lets a
//! deletion know every file it has to look for, a restart know which files it
//! is allowed to remove, and two attempts at the same clip not write over each
//! other — the staged name comes from the attempt, and there is only ever one
//! attempt with that name.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Layout {
    staging: PathBuf,
    clips: PathBuf,
}

impl Layout {
    /// Rooted at the application's data directory.
    pub fn under(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        Self {
            staging: root.join("staging"),
            clips: root.join("clips"),
        }
    }

    /// Where an attempt writes while it is still an attempt.
    ///
    /// Named `.partial` because that is what it is until something has read it:
    /// anything sweeping this directory can tell a file that is being written
    /// from one that is finished without asking the database.
    pub fn staged(&self, execution_id: &str) -> PathBuf {
        self.staging.join(format!("{}.partial.wav", safe(execution_id)))
    }

    /// Where the audio lives once it is the person's.
    pub fn take(&self, clip_id: &str, take_id: &str) -> PathBuf {
        self.clips.join(format!("{}-{}.wav", safe(clip_id), safe(take_id)))
    }

    pub fn staging_dir(&self) -> &Path {
        &self.staging
    }

    pub fn prepare(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.staging)?;
        std::fs::create_dir_all(&self.clips)
    }
}

/// Identifiers become file names, so they must not be able to become paths.
/// An id carrying a separator would otherwise put a file outside the directory
/// the layout is the whole point of.
fn safe(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::Layout;

    #[test]
    fn an_identifier_cannot_escape_the_directory_it_names() {
        let layout = Layout::under("/data");
        let staged = layout.staged("../../etc/passwd");
        assert_eq!(staged.parent().unwrap(), std::path::Path::new("/data/staging"));
        assert!(!staged.to_string_lossy().contains(".."));
    }

    #[test]
    fn one_attempt_has_one_staged_name() {
        let layout = Layout::under("/data");
        assert_eq!(layout.staged("exec-1"), layout.staged("exec-1"));
        assert_ne!(layout.staged("exec-1"), layout.staged("exec-2"));
    }
}
