//! Installing, activating and choosing runtime versions.
//!
//! A version is installed at its final immutable path — `runtimes/<id>/<version>/`
//! — and never moved, because the Python environment inside it remembers where
//! it was built. What changes is the database: a version becomes `ready` once
//! an engine has answered a handshake from its own directory, and becomes the
//! one that answers in a single row change. Rollback is that same row change
//! pointed at the version kept from before. There is no moment with two active
//! versions, no moment with none mid-switch, and no symlink anywhere.
//!
//! A failure at any step of an install leaves the version that was answering
//! exactly where it was: everything an install writes lands inside the new
//! version's directory, which is debris until the final activation and swept
//! as debris if a crash strands it.

use std::path::{Path, PathBuf};

use speech_engine::published::Published;
use speech_engine::runtime::{self, Pack, Progress};
use speech_engine::runtimes::{Descriptor, Places};
use yarngo_store::Store;

use crate::engine::Spawn;

/// The version name an install adopted in place wears. Installs before
/// versioning built one environment at a fixed path; it cannot move, so it is
/// registered where it lies and retired like any other version once something
/// newer is ready.
pub const ADOPTED: &str = "adopted";

/// A runtime the application can start: the descriptor, and which installed
/// version it came from.
#[derive(Clone, Debug)]
pub struct Ready {
    pub descriptor: Descriptor,
    pub version: String,
}

impl Ready {
    pub fn spawn(&self, data_dir: &Path) -> Spawn {
        Spawn {
            runtime: self.descriptor.clone(),
            data_dir: data_dir.to_path_buf(),
            version: Some(self.version.clone()),
        }
    }
}

/// Where one version lives. `ADOPTED` is the environment a pre-versioning
/// install built, which is where it will stay until it is retired.
fn home_of(places: &Places, id: &str, version: &str) -> PathBuf {
    if version == ADOPTED {
        places.data.join("runtime").join(id)
    } else {
        places.data.join("runtimes").join(id).join(version)
    }
}

/// The descriptor of one installed version, if it loads and everything it
/// names is there.
fn descriptor_of(places: &Places, id: &str, version: &str) -> Option<Descriptor> {
    let home = home_of(places, id, version);
    let descriptor = Descriptor::read(&home.join("runtime.json"), places)?;
    descriptor.available().then_some(descriptor)
}

/// What answers for one runtime, after startup has run: its active version if
/// it is whole, since startup already repointed anything broken.
pub fn active(store: &Store, places: &Places, id: &str) -> Option<Ready> {
    let version = store.active_runtime(id).ok()??;
    let descriptor = descriptor_of(places, id, &version)?;
    Some(Ready { descriptor, version })
}

/// Every runtime with a version that answers, for choosing and for showing.
pub fn all_active(store: &Store, places: &Places) -> Vec<Ready> {
    let Ok(rows) = store.installed_runtimes(None) else {
        return Vec::new();
    };
    let mut ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    ids.dedup();
    ids.into_iter().filter_map(|id| active(store, places, id)).collect()
}

/// Put the runtime state in order before anything starts.
///
/// Three jobs, in dependency order. Sweep: a row still `installing` is a crash
/// mid-install, and a directory without a ready row is debris — both go.
/// Adopt: a pre-versioning install that still works is registered where it
/// lies, so updating the application never breaks the runtime that was
/// answering yesterday. Repoint: an active pointer at a version that stopped
/// existing falls back to the newest other ready version rather than pinning
/// the application to a ghost.
pub fn startup(store: &Store, places: &Places, pack: &'static Pack) {
    runtime::remove_pre_pack_runtime(&places.data.join("runtime"));
    sweep(store, places, pack);
    adopt(store, places, pack);
    repoint(store, places, pack);
}

fn sweep(store: &Store, places: &Places, pack: &Pack) {
    let known = store.installed_runtimes(Some(pack.id)).unwrap_or_default();
    for runtime in &known {
        if !runtime.ready {
            let _ = std::fs::remove_dir_all(home_of(places, &runtime.id, &runtime.version));
            let _ = store.remove_runtime(&runtime.id, &runtime.version);
        }
    }

    // Directories nothing vouches for, and stray files from the layout that
    // kept descriptors directly under runtimes/<id>/.
    let parent = places.data.join("runtimes").join(pack.id);
    let Ok(entries) = std::fs::read_dir(&parent) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let vouched = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|version| {
                known.iter().any(|r| r.ready && r.version == version)
            });
        if path.is_dir() && !vouched {
            let _ = std::fs::remove_dir_all(&path);
        } else if !path.is_dir() {
            let _ = std::fs::remove_file(&path);
        }
    }
}

fn adopt(store: &Store, places: &Places, pack: &Pack) {
    let any = store
        .installed_runtimes(Some(pack.id))
        .map(|rows| !rows.is_empty())
        .unwrap_or(true);
    if any {
        return;
    }
    let home = home_of(places, pack.id, ADOPTED);
    if !runtime::venv_python(&home).exists() {
        return;
    }
    // The descriptor the old layout kept elsewhere predates this schema;
    // written fresh where the environment actually is.
    if runtime::describe_home(pack, &home).is_err() {
        return;
    }
    if descriptor_of(places, pack.id, ADOPTED).is_none() {
        return;
    }
    let at = now();
    // Ready without a handshake: this is the install that was answering
    // yesterday, and refusing to adopt it would break a working machine on an
    // application update. If it no longer starts, repointing at spawn failure
    // is the net underneath.
    let _ = store.runtime_installing(pack.id, ADOPTED, 1, &at);
    let _ = store.runtime_ready(pack.id, ADOPTED, &at);
    if store.active_runtime(pack.id).ok().flatten().is_none() {
        let _ = store.activate_runtime(pack.id, ADOPTED, &at);
    }
}

fn repoint(store: &Store, places: &Places, pack: &Pack) {
    let active = store.active_runtime(pack.id).ok().flatten();
    let whole = active
        .as_deref()
        .is_some_and(|version| descriptor_of(places, pack.id, version).is_some());
    if whole {
        return;
    }
    let fallback = newest_ready(store, places, pack, active.as_deref());
    let _ = store.repoint_runtime(pack.id, fallback.as_deref(), &now());
}

/// The newest ready version that actually loads, excluding `not_this`.
fn newest_ready(
    store: &Store,
    places: &Places,
    pack: &Pack,
    not_this: Option<&str>,
) -> Option<String> {
    let mut rows = store.installed_runtimes(Some(pack.id)).ok()?;
    rows.retain(|row| row.ready && Some(row.version.as_str()) != not_this);
    // installed_runtimes orders oldest first; the newest install wins.
    rows.iter()
        .rev()
        .find(|row| descriptor_of(places, pack.id, &row.version).is_some())
        .map(|row| row.version.clone())
}

/// Install the best release this build can use, and make it the one that
/// answers.
///
/// The order is the point. Everything is written into the new version's own
/// directory; the engine is started from that directory and answers the
/// handshake; only then does the database mark it ready and — separately —
/// active. A failure anywhere before that leaves the version that was
/// answering untouched, and the debris is removed here and swept again at
/// startup if a crash prevents even that.
pub fn install(
    store: &Store,
    places: &Places,
    pack: &'static Pack,
    archive: Option<PathBuf>,
    report: &mut dyn FnMut(Progress),
) -> Result<Ready, String> {
    let outcome = installation(store, places, pack, archive, report);
    match &outcome {
        Ok(ready) => {
            report(Progress::Step(format!("Runtime {} is answering.", ready.version)));
            report(Progress::Fraction(1.0));
            report(Progress::Done);
        }
        Err(why) => report(Progress::Failed(why.clone())),
    }
    outcome
}

fn installation(
    store: &Store,
    places: &Places,
    pack: &'static Pack,
    archive: Option<PathBuf>,
    report: &mut dyn FnMut(Progress),
) -> Result<Ready, String> {
    runtime::host_supported()?;

    // What to install, in order: the newest published release this build can
    // use; failing that whatever already answers; failing that the recipe
    // inside the application.
    //
    // The middle step is the one worth stating. Not being able to reach or
    // believe the repository says nothing about the runtime already on the
    // disk, and replacing it with the one inside the application would be a
    // silent downgrade to a different implementation — decided by a network
    // failure, on a machine whose voices were made by the runtime it just
    // discarded. Unreachable and untrustworthy are the same answer here: we
    // were not told anything we can act on.
    let published = Published::newest(pack.id);
    let version = match &published {
        Ok(release) => release.version().to_string(),
        Err(why) => {
            eprintln!("no published runtime: {why}");
            if let Some(answering) = active(store, places, pack.id) {
                report(Progress::Step(format!(
                    "Keeping the runtime already installed, {}.",
                    answering.version
                )));
                report(Progress::Fraction(1.0));
                return Ok(answering);
            }
            report(Progress::Step("Using the runtime this build ships with.".into()));
            format!("bundled-{}", env!("CARGO_PKG_VERSION"))
        }
    };

    // Already here and whole: activation is the only thing left to want.
    if store
        .installed_runtimes(Some(pack.id))
        .unwrap_or_default()
        .iter()
        .any(|row| row.ready && row.version == version)
        && descriptor_of(places, pack.id, &version).is_some()
    {
        store.activate_runtime(pack.id, &version, &now()).map_err(|e| e.to_string())?;
        let descriptor = descriptor_of(places, pack.id, &version)
            .ok_or("the installed runtime stopped loading between two looks")?;
        return Ok(Ready { descriptor, version });
    }

    let home = home_of(places, pack.id, &version);
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).map_err(|e| format!("cannot create {}: {e}", home.display()))?;
    store
        .runtime_installing(pack.id, &version, speech_engine::catalogue::ENGINE_API, &now())
        .map_err(|e| e.to_string())?;

    let built = build(places, pack, &version, published.ok().as_ref(), &home, archive, report);
    let descriptor = match built {
        Ok(descriptor) => descriptor,
        Err(why) => {
            // The debris rule: nothing outside `home` was written, so removing
            // `home` removes the failure.
            let _ = std::fs::remove_dir_all(&home);
            let _ = store.remove_runtime(pack.id, &version);
            return Err(why);
        }
    };

    let at = now();
    store.runtime_ready(pack.id, &version, &at).map_err(|e| e.to_string())?;
    store.activate_runtime(pack.id, &version, &at).map_err(|e| e.to_string())?;
    retire(store, places, pack, &version);
    Ok(Ready { descriptor, version })
}

/// Everything that happens inside the new version's directory.
fn build(
    places: &Places,
    pack: &'static Pack,
    version: &str,
    published: Option<&Published>,
    home: &Path,
    archive: Option<PathBuf>,
    report: &mut dyn FnMut(Progress),
) -> Result<Descriptor, String> {
    match published {
        Some(release) => {
            release.stage_recipe(home)?;
            if release.stage_engine(pack.id, home)? {
                report(Progress::Step(format!(
                    "Runtime {version} brings its own implementation."
                )));
            }
        }
        None => runtime::stage_bundled(pack, home)?,
    }

    runtime::build_env(&places.data, pack, home, archive, report)?;
    runtime::describe_home(pack, home)?;

    let descriptor = Descriptor::read(&home.join("runtime.json"), places)
        .ok_or("the installed runtime's descriptor does not load")?;
    if !descriptor.available() {
        return Err("the installed runtime's descriptor names things that are not there".into());
    }

    // The proof: an engine, started from this directory, answering this
    // protocol with capabilities this application can use. A runtime that
    // cannot pass this is not one to activate, however cleanly it installed.
    report(Progress::Step("Checking the runtime answers…".into()));
    report(Progress::Fraction(0.95));
    let spawn = Spawn {
        runtime: descriptor.clone(),
        data_dir: places.data.clone(),
        version: Some(version.to_string()),
    };
    // The protocol name and version are enforced inside the handshake itself —
    // an engine speaking something else never gets as far as capabilities.
    let capabilities = spawn
        .probe()
        .map_err(|why| format!("the runtime did not answer its handshake: {why}"))?;
    if !capabilities.cloning() {
        return Err("the runtime answered, but cannot synthesise speech".into());
    }
    Ok(descriptor)
}

/// Keep the version that answers and the one before it; retire the rest.
///
/// One previous version is rollback. More is a disk full of pasts nobody asked
/// to keep — each is hundreds of megabytes of environment.
fn retire(store: &Store, places: &Places, pack: &Pack, active: &str) {
    let Ok(rows) = store.installed_runtimes(Some(pack.id)) else {
        return;
    };
    let mut previous: Vec<&yarngo_store::runtimes::InstalledRuntime> = rows
        .iter()
        .filter(|row| row.ready && row.version != active)
        .collect();
    // Oldest first from the store; keep the last — the newest previous.
    previous.pop();
    for row in previous {
        let _ = store.remove_runtime(&row.id, &row.version);
        let _ = std::fs::remove_dir_all(home_of(places, &row.id, &row.version));
    }
}

/// Whether the repository offers something newer than what answers. Answers
/// without changing anything, so the application can offer rather than act.
///
/// Newer, not merely different. A repository naming an older release than the
/// one installed is a publisher rolling something back, and whatever that is it
/// is not an update — offering it as one would put a downgrade behind a button
/// labelled with somebody else's word.
pub fn update_available(store: &Store, pack: &Pack) -> Result<Option<String>, String> {
    let offered = Published::newest(pack.id)?.version().to_string();
    let Some(active) = store.active_runtime(pack.id).map_err(|e| e.to_string())? else {
        // Nothing answers, so this is not an update either — it is the install
        // the setup screen exists for.
        return Ok(None);
    };
    let newer = active != offered && speech_engine::catalogue::at_least(&offered, &active);
    Ok(newer.then_some(offered))
}

fn now() -> String {
    crate::engine::now()
}
