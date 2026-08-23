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
///
/// A test or development run says so by setting `YARNGO_TEST_MODE`, and one
/// that has not been pointed somewhere else will not start. That is deliberate
/// and it is not defensive programming: a development build once inferred this
/// path, generated for a minute, and added a clip to somebody's real library.
/// Refusing here is the only place that covers every way of arriving at it.
pub fn data_dir() -> PathBuf {
    let resolved = match std::env::var("YARNGO_DATA") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => installed_data_dir(),
    };
    if let Err(refusal) = isolated(
        &resolved,
        &installed_data_dir(),
        std::env::var_os("YARNGO_TEST_MODE").is_some(),
    ) {
        panic!("{refusal}");
    }
    resolved
}

/// Whether this run may use this directory.
///
/// Separated from reading the environment so the rule can be stated once and
/// checked without a process to set variables on.
pub fn isolated(resolved: &Path, installed: &Path, test_mode: bool) -> Result<(), String> {
    if test_mode && resolved == installed {
        return Err(format!(
            "this is a test or development run and its data directory is the installed \
             application's ({}). Point YARNGO_DATA at a copy — yarngo_testing::Sandbox \
             makes one.",
            resolved.display()
        ));
    }
    Ok(())
}

/// Where the installed application keeps its data, whatever this process was
/// told. The thing a test must not be looking at.
pub fn installed_data_dir() -> PathBuf {
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

/// Where the placeholders in a runtime descriptor point.
///
/// Assembled here because this is what knows the two layouts — a bundle and a
/// checkout — and a descriptor should not have to.
pub fn places() -> crate::runtimes::Places {
    crate::runtimes::Places {
        data: data_dir(),
        runtime: runtime_dir(),
        resources: resource_root(),
    }
}

/// The directory shipped resources live in, whichever layout this is.
pub fn resource_root() -> PathBuf {
    bundle_resources().unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packaging")
    })
}

/// A file shipped alongside the app: inside `Contents/Resources` in a bundle,
/// or under `packaging/` in a checkout. `None` when it is not there, so the
/// caller can fall back rather than build a path to nothing.
pub fn resource(name: &str) -> Option<PathBuf> {
    let candidates = [
        bundle_resources().map(|r| r.join(name)),
        std::env::var("CARGO_MANIFEST_DIR")
            .ok()
            .map(|d| PathBuf::from(d).join("../../packaging").join(name)),
    ];
    candidates.into_iter().flatten().find(|p| p.exists())
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

        // The runtime this app installed wins. Failing that, an interpreter
        // already on the machine that can import the speech package — which is
        // also what lets a checkout run without installing anything.
        //
        // There used to be one developer's venv path hard-coded here, which
        // shipped inside the binary and meant nothing on anyone else's Mac.
        let python = if installed.exists() {
            installed
        } else {
            crate::runtime::existing_interpreter().unwrap_or(installed)
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

#[cfg(test)]
mod tests {
    use super::isolated;
    use std::path::Path;

    const INSTALLED: &str = "/Users/someone/Library/Application Support/Yarngo Studio";

    #[test]
    fn a_test_run_may_not_use_the_installed_directory() {
        let refusal = isolated(Path::new(INSTALLED), Path::new(INSTALLED), true)
            .expect_err("a test run was allowed at the real store");
        assert!(refusal.contains("YARNGO_DATA"), "the refusal does not say what to do");
    }

    #[test]
    fn a_test_run_pointed_somewhere_else_is_fine() {
        assert!(isolated(Path::new("/tmp/copy"), Path::new(INSTALLED), true).is_ok());
    }

    /// The application itself uses its own directory, which is the whole point
    /// of it. The rule is about test runs, not about the path.
    #[test]
    fn the_application_uses_its_own_directory() {
        assert!(isolated(Path::new(INSTALLED), Path::new(INSTALLED), false).is_ok());
    }
}
