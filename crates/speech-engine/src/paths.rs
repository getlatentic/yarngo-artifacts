//! Finding the sidecar and its interpreter, in a dev checkout or a bundle.
//!
//! Two layouts have to work from one code path:
//!
//! ```text
//! dev checkout                 macOS bundle
//! repo/                        Yarngo Studio.app/Contents/
//!   sidecar/engine.py            Resources/sidecar/engine.py
//!   target/debug/voicestudio     MacOS/voicestudio
//! ```
//!
//! The script ships inside the bundle. The Python runtime does not yet — it is
//! several gigabytes of interpreter, MLX and model weights, so it is installed
//! into application support rather than embedded, which is also what lets it be
//! downloaded and updated separately from the app.

use std::path::{Path, PathBuf};

/// Where the speech runtime lives once installed.
pub fn runtime_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("YARNGO_RUNTIME_DIR") {
        return PathBuf::from(dir);
    }
    data_dir().join("runtime")
}

/// Application support directory for installed runtimes, models and voices.
pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("YARNGO_DATA") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    if cfg!(target_os = "macos") {
        PathBuf::from(home).join("Library/Application Support/Yarngo Studio")
    } else if cfg!(target_os = "windows") {
        std::env::var("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(home))
            .join("Yarngo Studio")
    } else {
        PathBuf::from(home).join(".local/share/yarngo-studio")
    }
}

/// The `Contents/Resources` directory when running from a macOS bundle.
fn bundle_resources() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // …/Contents/MacOS/voicestudio -> …/Contents/Resources
    let contents = exe.parent()?.parent()?;
    if contents.file_name()? != "Contents" {
        return None;
    }
    let resources = contents.join("Resources");
    resources.is_dir().then_some(resources)
}

/// Directory holding `sidecar/engine.py`, whichever layout we are in.
fn sidecar_root(dev_root: &Path) -> PathBuf {
    if let Ok(dir) = std::env::var("YARNGO_SIDECAR_ROOT") {
        return PathBuf::from(dir);
    }
    bundle_resources().unwrap_or_else(|| dev_root.to_path_buf())
}

#[derive(Debug, Clone)]
pub struct EnginePaths {
    pub python: PathBuf,
    pub script: PathBuf,
    pub work_dir: PathBuf,
}

impl EnginePaths {
    /// Resolve for the current layout. `dev_root` is the repository root, used
    /// only when not running from a bundle.
    pub fn resolve(dev_root: &Path) -> Self {
        let root = sidecar_root(dev_root);
        // Packagers flatten resource globs, so the script may sit directly in
        // Resources rather than under a sidecar/ directory. Accept either.
        let nested = root.join("sidecar").join("engine.py");
        let script = if nested.exists() { nested } else { root.join("engine.py") };

        let runtime = runtime_dir();
        let installed = crate::runtime::interpreter(&runtime);

        // An installed runtime wins; otherwise fall back to a developer venv so
        // a checkout runs without an install step.
        let python = if installed.exists() {
            installed
        } else if let Ok(explicit) = std::env::var("YARNGO_PYTHON") {
            PathBuf::from(explicit)
        } else {
            PathBuf::from("/Users/dev/workspace/voice-clone-bench/mlx-speech/.venv/bin/python")
        };

        // The sidecar imports mlx_speech, which resolves from the interpreter's
        // own environment; the working directory only needs to exist.
        let work_dir = python
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.clone());

        Self { python, script, work_dir }
    }

    /// Whether everything needed to start the engine is present, so the app can
    /// say what is missing instead of failing at spawn.
    pub fn missing(&self) -> Option<String> {
        if !self.script.exists() {
            return Some(format!("sidecar script not found at {}", self.script.display()));
        }
        if !self.python.exists() {
            return Some(format!(
                "speech runtime not installed (looked for {})",
                self.python.display()
            ));
        }
        None
    }
}
