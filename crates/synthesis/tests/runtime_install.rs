//! Installing a runtime version can only ever improve things.
//!
//! The property every test here defends: whatever goes wrong — a tampered
//! archive, a failing dependency step, an engine that will not answer, a crash
//! at any point — the version that was answering keeps answering. The new
//! version becomes the one that answers only after an engine has spoken from
//! its own directory, and then in a single row change.
//!
//! The repositories are the signed fixtures under
//! `crates/speech-engine/tests/fixtures/tuf`, so every install here crosses
//! the same verification a real one does.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard};

use speech_engine::runtime;
use speech_engine::runtimes::Places;
use yarngo_store::Store;
use yarngo_synthesis::runtimes as installer;
use yarngo_testing::Sandbox;

/// The environment is process-wide, so tests that set it take turns.
static ENV: Mutex<()> = Mutex::new(());

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../speech-engine/tests/fixtures/tuf")
}

struct Rig {
    sandbox: Sandbox,
    _guard: MutexGuard<'static, ()>,
}

impl Rig {
    /// A machine ready to install: interpreter already down, a uv that builds
    /// a working stand-in environment, resources carrying a protocol-speaking
    /// bundled engine, and the repository at `repo`.
    fn new(repo: &str) -> Self {
        let guard = ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let sandbox = Sandbox::empty();
        let root = sandbox.root();

        // The interpreter, already at its pin, so nothing downloads.
        let base = runtime::interpreter_base(root, runtime::pack());
        std::fs::create_dir_all(base.parent().unwrap()).expect("interpreter dir");
        written(&base, "#!/bin/sh\nexit 0\n");

        // A uv that "builds" the environment: a python that answers the import
        // probe itself and hands everything else to the real interpreter.
        let uv = root.join("fake-uv");
        written(
            &uv,
            r#"#!/bin/sh
home=""
previous=""
for argument in "$@"; do
  if [ "$previous" = "--project" ]; then home="$argument"; fi
  previous="$argument"
done
[ -n "$home" ] || { echo "no --project" >&2; exit 9; }
mkdir -p "$home/.venv/bin"
cat > "$home/.venv/bin/python3" <<'WRAPPER'
#!/bin/sh
if [ "$1" = "-c" ]; then exit 0; fi
exec /usr/bin/python3 "$@"
WRAPPER
chmod +x "$home/.venv/bin/python3"
exit 0
"#,
        );

        // The bundled engine, speaking the protocol, where resources point.
        let sidecar = root.join("resources/sidecar");
        std::fs::create_dir_all(&sidecar).expect("sidecar dir");
        let script = yarngo_testing::standin::script(&sidecar, "    pass");
        std::fs::rename(&script, sidecar.join("engine.py")).expect("engine.py");

        unsafe {
            std::env::set_var("YARNGO_TEST_MODE", "1");
            std::env::set_var("YARNGO_DATA", root);
            std::env::set_var("YARNGO_UV", &uv);
            std::env::set_var("YARNGO_TRUST_ROOT", fixtures().join(repo).join("root.json"));
            std::env::set_var(
                "YARNGO_REPOSITORY",
                format!("file://{}", fixtures().join(repo).display()),
            );
        }
        Self { sandbox, _guard: guard }
    }

    fn root(&self) -> &Path {
        self.sandbox.root()
    }

    fn places(&self) -> Places {
        Places {
            data: self.root().to_path_buf(),
            resources: self.root().join("resources"),
        }
    }

    fn store(&self) -> Store {
        Store::open(&self.sandbox.database()).expect("store")
    }

    fn install(&self) -> Result<installer::Ready, String> {
        installer::install(
            &self.store(),
            &self.places(),
            runtime::pack(),
            None,
            &mut |_| {},
        )
    }

    /// Point the repository somewhere that is not one, as a machine offline
    /// or behind a broken proxy is pointed.
    fn go_offline(&self) {
        unsafe {
            std::env::set_var("YARNGO_REPOSITORY", "file:///nowhere/at/all");
        }
    }

    /// Point it at a different fixture repository.
    fn point_at(&self, repo: &str) {
        unsafe {
            std::env::set_var(
                "YARNGO_REPOSITORY",
                format!("file://{}", fixtures().join(repo).display()),
            );
        }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("YARNGO_TEST_MODE");
            std::env::remove_var("YARNGO_DATA");
            std::env::remove_var("YARNGO_UV");
            std::env::remove_var("YARNGO_TRUST_ROOT");
            std::env::remove_var("YARNGO_REPOSITORY");
        }
    }
}

fn written(path: &Path, contents: &str) {
    std::fs::write(path, contents).expect("write");
    let _ = Command::new("chmod").arg("+x").arg(path).status();
}

/// What answers right now, read the way the application reads it.
fn answering(rig: &Rig) -> Option<String> {
    rig.store().active_runtime(runtime::pack().id).expect("active")
}

#[test]
fn a_published_release_is_installed_verified_and_activated() {
    let rig = Rig::new("repo");
    let ready = rig.install().expect("install");

    assert_eq!(ready.version, "1.0.0");
    assert_eq!(answering(&rig).as_deref(), Some("1.0.0"));
    let home = rig.root().join("runtimes/mlx/1.0.0");
    assert!(home.join(".venv/bin/python3").exists(), "no environment at the final path");
    assert!(
        std::fs::read_to_string(home.join("engine.py"))
            .expect("the release's own engine")
            .starts_with("# the published engine"),
        "not the published implementation"
    );

    // Rows agree with the disk: exactly one version, ready.
    let rows = rig.store().installed_runtimes(Some("mlx")).expect("rows");
    assert_eq!(rows.len(), 1);
    assert!(rows[0].ready);
}

#[test]
fn installing_again_changes_nothing_but_says_so() {
    let rig = Rig::new("repo");
    rig.install().expect("first");
    let again = rig.install().expect("second");
    assert_eq!(again.version, "1.0.0");
    assert_eq!(rig.store().installed_runtimes(Some("mlx")).expect("rows").len(), 1);
}

#[test]
fn a_machine_with_no_repository_installs_what_shipped() {
    let rig = Rig::new("repo");
    rig.go_offline();
    let bundled = rig.install().expect("offline install");
    assert!(bundled.version.starts_with("bundled-"), "{}", bundled.version);
    assert_eq!(answering(&rig), Some(bundled.version.clone()));
    // Run by the engine that ships, because it brought none of its own.
    assert_eq!(
        bundled.descriptor.engine_path().expect("engine"),
        rig.root().join("resources/sidecar/engine.py")
    );
}

/// A published runtime, once installed, is not given up because the repository
/// went away.
///
/// Losing sight of the repository says nothing about the runtime already on the
/// disk. Replacing it with the one inside the application would be a silent
/// downgrade to a different implementation, decided by a network failure — and
/// on a machine whose voices were made by the runtime it just discarded.
#[test]
fn a_published_runtime_is_not_replaced_by_the_bundled_one_when_the_repository_goes_away() {
    let rig = Rig::new("repo");
    let published = rig.install().expect("install the published release");
    assert_eq!(published.version, "1.0.0");

    rig.go_offline();
    let after = rig.install();

    assert_eq!(
        answering(&rig).as_deref(),
        Some("1.0.0"),
        "an unreachable repository downgraded a working published runtime"
    );
    if let Ok(ready) = &after {
        assert_eq!(ready.version, "1.0.0", "installed something else instead");
    }
}

/// The same, but the repository is reachable and lying: correctly signed by
/// keys this build never trusted. That is a stronger signal than silence, and
/// it must not be rewarded with a downgrade either.
#[test]
fn a_repository_we_do_not_trust_does_not_replace_a_working_runtime() {
    let rig = Rig::new("repo");
    rig.install().expect("install the published release");

    rig.point_at("impostor");
    let _ = rig.install();

    assert_eq!(
        answering(&rig).as_deref(),
        Some("1.0.0"),
        "another publisher's repository caused a downgrade by being refused"
    );
}

#[test]
fn a_tampered_target_leaves_the_answering_runtime_untouched() {
    let rig = Rig::new("repo");
    rig.go_offline();
    let before = rig.install().expect("something answering first");

    // Back online — against a repository whose engine archive was edited
    // after signing.
    let copy = rig.root().join("served");
    copy_tree(&fixtures().join("repo"), &copy);
    let target = std::fs::read_dir(copy.join("targets"))
        .expect("targets")
        .map(|e| e.expect("entry").path())
        .find(|p| p.to_string_lossy().ends_with("engine.tar.gz"))
        .expect("the engine target");
    flip_a_byte(&target);
    unsafe {
        std::env::set_var("YARNGO_TRUST_ROOT", copy.join("root.json"));
        std::env::set_var("YARNGO_REPOSITORY", format!("file://{}", copy.display()));
    }

    let refused = rig.install().expect_err("a tampered archive was installed");
    assert!(refused.to_lowercase().contains("hash mismatch"), "{refused}");

    assert_eq!(answering(&rig), Some(before.version.clone()), "the active version moved");
    assert!(
        !rig.root().join("runtimes/mlx/1.0.0").exists(),
        "debris from the refused install was left behind"
    );
}

#[test]
fn a_failing_dependency_step_leaves_the_answering_runtime_untouched() {
    let rig = Rig::new("repo");
    rig.go_offline();
    let before = rig.install().expect("something answering first");

    // The repository is fine; the machine is not — uv refuses.
    unsafe {
        std::env::set_var(
            "YARNGO_REPOSITORY",
            format!("file://{}", fixtures().join("repo").display()),
        );
    }
    let uv = rig.root().join("fake-uv");
    written(&uv, "#!/bin/sh\necho 'disk full' >&2\nexit 2\n");

    let refused = rig.install().expect_err("a failed dependency step was called installed");
    assert!(refused.contains("disk full"), "{refused}");
    assert_eq!(answering(&rig), Some(before.version.clone()));
    assert!(!rig.root().join("runtimes/mlx/1.0.0").exists(), "debris left behind");
}

#[test]
fn an_engine_that_does_not_answer_is_not_activated() {
    let rig = Rig::new("repo");
    rig.go_offline();
    let before = rig.install().expect("something answering first");

    // This release installs cleanly and its engine dies on arrival.
    unsafe {
        std::env::set_var(
            "YARNGO_REPOSITORY",
            format!("file://{}", fixtures().join("repo").display()),
        );
    }
    let uv = rig.root().join("fake-uv");
    written(
        &uv,
        r#"#!/bin/sh
home=""
previous=""
for argument in "$@"; do
  if [ "$previous" = "--project" ]; then home="$argument"; fi
  previous="$argument"
done
mkdir -p "$home/.venv/bin"
printf '#!/bin/sh\nif [ "$1" = "-c" ]; then exit 0; fi\nexit 1\n' > "$home/.venv/bin/python3"
chmod +x "$home/.venv/bin/python3"
exit 0
"#,
    );

    let refused = rig.install().expect_err("an engine that cannot start was activated");
    assert!(refused.contains("handshake"), "{refused}");
    assert_eq!(answering(&rig), Some(before.version.clone()));
}

#[test]
fn an_engine_speaking_a_different_api_is_not_activated() {
    let rig = Rig::new("newapi");
    let refused = rig.install().expect_err("an engine speaking api 2 was activated");
    assert!(refused.contains("speaks"), "{refused}");
    assert_eq!(answering(&rig), None, "nothing was answering, and nothing should be now");
    let rows = rig.store().installed_runtimes(Some("mlx")).expect("rows");
    assert!(rows.is_empty(), "a refused install left rows: {rows:?}");
}

#[test]
fn an_engine_without_synthesis_is_not_activated() {
    let rig = Rig::new("mute");
    let refused = rig.install().expect_err("an engine that cannot speak was activated");
    assert!(refused.contains("synthesise"), "{refused}");
    assert_eq!(answering(&rig), None);
}

#[test]
fn a_crash_mid_install_is_swept_and_the_answering_runtime_recovered() {
    let rig = Rig::new("repo");
    rig.go_offline();
    let before = rig.install().expect("something answering first");

    // What a crash strands: a row still installing, and a half-written home.
    let store = rig.store();
    store.runtime_installing("mlx", "1.0.0", 1, "t9").expect("row");
    let home = rig.root().join("runtimes/mlx/1.0.0");
    std::fs::create_dir_all(&home).expect("half-written home");
    std::fs::write(home.join("uv.lock"), b"half").expect("debris");
    // And a directory nothing vouches for at all.
    std::fs::create_dir_all(rig.root().join("runtimes/mlx/9.9.9")).expect("stray");

    installer::startup(&store, &rig.places(), runtime::pack());

    assert_eq!(answering(&rig), Some(before.version.clone()));
    assert!(!home.exists(), "the crashed install's debris survived startup");
    assert!(!rig.root().join("runtimes/mlx/9.9.9").exists(), "the stray survived");
    let rows = store.installed_runtimes(Some("mlx")).expect("rows");
    assert!(
        rows.iter().all(|row| row.ready),
        "an installing row survived startup: {rows:?}"
    );
}

#[test]
fn a_crash_between_ready_and_activation_leaves_exactly_one_answering() {
    let rig = Rig::new("repo");
    rig.go_offline();
    let before = rig.install().expect("something answering first");

    // Ready but never activated — the crash landed between the two row
    // changes. The old version answers; the new one is there to be chosen.
    let store = rig.store();
    let home = rig.root().join("runtimes/mlx/1.0.0");
    let bundled_home = rig.root().join("runtimes/mlx").join(&before.version);
    copy_tree(&bundled_home, &home);
    store.runtime_installing("mlx", "1.0.0", 1, "t9").expect("row");
    store.runtime_ready("mlx", "1.0.0", "t9").expect("ready");

    installer::startup(&store, &rig.places(), runtime::pack());
    assert_eq!(
        answering(&rig),
        Some(before.version.clone()),
        "a version nothing activated started answering"
    );

    // And it is selectable, in one step.
    store.activate_runtime("mlx", "1.0.0", "t10").expect("activate");
    assert_eq!(answering(&rig).as_deref(), Some("1.0.0"));
}

#[test]
fn a_version_that_breaks_later_falls_back_to_the_previous_one() {
    let rig = Rig::new("repo");
    rig.go_offline();
    let bundled = rig.install().expect("first install");
    unsafe {
        std::env::set_var(
            "YARNGO_REPOSITORY",
            format!("file://{}", fixtures().join("repo").display()),
        );
    }
    let published = rig.install().expect("second install");
    assert_eq!(answering(&rig), Some(published.version.clone()));
    assert_ne!(bundled.version, published.version);

    // The active version's directory is destroyed behind the store's back.
    std::fs::remove_dir_all(rig.root().join("runtimes/mlx/1.0.0")).expect("break it");

    let store = rig.store();
    installer::startup(&store, &rig.places(), runtime::pack());
    assert_eq!(
        answering(&rig),
        Some(bundled.version.clone()),
        "the previous ready version was not selected"
    );
}

#[test]
fn only_the_answering_version_and_one_previous_are_kept() {
    let rig = Rig::new("repo");
    rig.go_offline();
    let first = rig.install().expect("bundled");

    // Sidestep versions to simulate a history: rename the bundled row.
    let store = rig.store();
    let places = rig.places();
    store.runtime_installing("mlx", "0.9.0", 1, "t1").expect("row");
    store.runtime_ready("mlx", "0.9.0", "t1").expect("ready");
    copy_tree(
        &rig.root().join("runtimes/mlx").join(&first.version),
        &rig.root().join("runtimes/mlx/0.9.0"),
    );

    unsafe {
        std::env::set_var(
            "YARNGO_REPOSITORY",
            format!("file://{}", fixtures().join("repo").display()),
        );
    }
    let newest = installer::install(&store, &places, runtime::pack(), None, &mut |_| {})
        .expect("published install");
    assert_eq!(newest.version, "1.0.0");

    let mut versions: Vec<String> = store
        .installed_runtimes(Some("mlx"))
        .expect("rows")
        .into_iter()
        .map(|row| row.version)
        .collect();
    versions.sort();
    assert_eq!(versions.len(), 2, "history should be active plus one: {versions:?}");
    assert!(versions.contains(&"1.0.0".to_string()));
    assert!(
        !rig.root().join("runtimes/mlx/0.9.0").exists()
            || versions.contains(&"0.9.0".to_string()),
        "a retired version's directory survived"
    );
}

#[test]
fn the_engine_starts_without_the_repository() {
    let rig = Rig::new("repo");
    let installed = rig.install().expect("install");
    rig.go_offline();

    // Choosing and starting consult only the store and the disk. No part of
    // spawn may depend on the repository being reachable.
    let store = rig.store();
    installer::startup(&store, &rig.places(), runtime::pack());
    let ready = installer::active(&store, &rig.places(), "mlx")
        .expect("the installed runtime stopped answering when the network went away");
    assert_eq!(ready.version, installed.version);
    let capabilities = ready
        .spawn(rig.root())
        .probe()
        .expect("the engine did not start offline");
    assert!(capabilities.cloning());
}

#[test]
fn an_update_is_offered_only_when_the_repository_has_something_newer() {
    let rig = Rig::new("repo");
    rig.go_offline();
    let bundled = rig.install().expect("offline install");

    // Back online: the repository offers 1.0.0 and something older answers.
    unsafe {
        std::env::set_var(
            "YARNGO_REPOSITORY",
            format!("file://{}", fixtures().join("repo").display()),
        );
    }
    let store = rig.store();
    let offered = installer::update_available(&store, runtime::pack())
        .expect("read the repository")
        .expect("an update should be offered");
    assert_eq!(offered, "1.0.0");
    assert_eq!(answering(&rig), Some(bundled.version.clone()), "offering is not acting");

    rig.install().expect("take it");
    assert_eq!(
        installer::update_available(&store, runtime::pack()).expect("read again"),
        None,
        "what is answering is what is offered"
    );
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create dir");
    for entry in std::fs::read_dir(from).expect("read dir") {
        let entry = entry.expect("dir entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("copy file");
        }
    }
}

fn flip_a_byte(path: &Path) {
    let mut bytes = std::fs::read(path).expect("read to tamper with");
    let last = bytes.len() - 2;
    bytes[last] ^= 0x20;
    std::fs::write(path, bytes).expect("write tampered");
}
