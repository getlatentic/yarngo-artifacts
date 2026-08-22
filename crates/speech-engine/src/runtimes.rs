//! Runtimes the application can speak to, and how to start them.
//!
//! The protocol has always been the contract: a child process, JSON-RPC over
//! its stdio, a versioned handshake. What was not a contract was starting one —
//! the application knew it was launching a Python interpreter over a script
//! called `engine.py`, which made "the engine" one particular program rather
//! than anything that answers.
//!
//! A runtime now says how to start it, in a file beside itself. The shipped one
//! is described the same way as any other, so the path the application takes to
//! its own engine is the path a third one would take.
//!
//! ```json
//! {
//!   "id": "mlx",
//!   "name": "Apple silicon",
//!   "command": "{runtime}/mlx/.venv/bin/python",
//!   "args": ["{resources}/sidecar/engine.py"],
//!   "env": { "YARNGO_DATA": "{data}" }
//! }
//! ```
//!
//! A descriptor is something the person installed, and it names a program to
//! run. That is the same trust as any editor extension or language server: what
//! it can do is what the person running it can do.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

/// Where a descriptor's placeholders point.
#[derive(Clone, Debug, PartialEq)]
pub struct Places {
    /// Everything the application keeps.
    pub data: PathBuf,
    /// Where installed runtimes live.
    pub runtime: PathBuf,
    /// What shipped with the application.
    pub resources: PathBuf,
    /// The directory this descriptor was read from.
    pub own: PathBuf,
}

/// How to start one runtime.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Descriptor {
    pub id: String,
    pub name: String,
    /// The program to run. Placeholders are substituted before it is used.
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Skipped in the picker, and still startable by name — for a runtime that
    /// is installed but not what the person wants used by default.
    #[serde(default)]
    pub hidden: bool,
    #[serde(skip)]
    places: Option<Places>,
}

impl Descriptor {
    /// Read one, remembering where it came from so `{self}` means something.
    pub fn read(path: &Path, places: &Places) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        let mut descriptor: Descriptor = serde_json::from_str(&text).ok()?;
        if descriptor.id.trim().is_empty() || descriptor.command.trim().is_empty() {
            return None;
        }
        descriptor.places = Some(Places {
            own: path.parent().map(Path::to_path_buf).unwrap_or_default(),
            ..places.clone()
        });
        Some(descriptor)
    }

    /// One described in code rather than read from a file — for a caller that
    /// knows the command outright. Nothing is substituted into it, because
    /// there is no file for `{self}` to mean anything relative to.
    pub fn running(
        id: impl Into<String>,
        name: impl Into<String>,
        command: impl Into<PathBuf>,
        args: Vec<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            command: command.into().to_string_lossy().into_owned(),
            args,
            env: BTreeMap::new(),
            hidden: false,
            places: None,
        }
    }

    /// Something the runtime should be started with. Applied after whatever the
    /// descriptor itself asked for, so a caller can be specific about the
    /// environment a particular run needs.
    pub fn with_env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(name.into(), value.into());
        self
    }

    fn fill(&self, text: &str) -> String {
        let Some(places) = &self.places else {
            return text.to_string();
        };
        text.replace("{data}", &places.data.to_string_lossy())
            .replace("{runtime}", &places.runtime.to_string_lossy())
            .replace("{resources}", &places.resources.to_string_lossy())
            .replace("{self}", &places.own.to_string_lossy())
    }

    pub fn program(&self) -> PathBuf {
        PathBuf::from(self.fill(&self.command))
    }

    /// Whether the program it names is actually there.
    ///
    /// Asked before it is offered, so a runtime whose interpreter has been
    /// removed is absent from the list rather than an error at first use.
    pub fn available(&self) -> bool {
        let program = self.program();
        // A bare name is looked up on PATH, which is not this to resolve.
        if program.components().count() == 1 {
            return true;
        }
        program.exists()
    }

    /// The command that starts it, ready to spawn.
    pub fn command(&self) -> Command {
        let mut command = Command::new(self.program());
        command.args(self.args.iter().map(|arg| self.fill(arg)));
        for (name, value) in &self.env {
            command.env(name, self.fill(value));
        }
        // The working directory is the runtime's own, so a relative path in a
        // descriptor means what its author meant.
        if let Some(places) = &self.places {
            if places.own.is_dir() {
                command.current_dir(&places.own);
            }
        }
        command
    }
}

/// Every runtime this machine offers, the shipped one first.
///
/// Installed runtimes live one directory each under `runtimes/`, so a runtime
/// is a folder somebody can add or remove without the application knowing it
/// was coming.
pub fn discover(places: &Places) -> Vec<Descriptor> {
    let mut found = Vec::new();
    let shipped = places.resources.join("runtimes");
    let installed = places.data.join("runtimes");
    for root in [shipped, installed] {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .map(|path| if path.is_dir() { path.join("runtime.json") } else { path })
            .filter(|path| path.extension().is_some_and(|e| e == "json"))
            .collect();
        // Read in a stated order rather than whatever the filesystem returns,
        // so which runtime the application picks is the same on every machine.
        paths.sort();
        for path in paths {
            let Some(descriptor) = Descriptor::read(&path, places) else {
                continue;
            };
            // First wins: a shipped runtime is not silently replaced by an
            // installed one that took its name.
            if !found.iter().any(|other: &Descriptor| other.id == descriptor.id) {
                found.push(descriptor);
            }
        }
    }
    found
}

/// The one to start: the person's choice if it is here and works, otherwise the
/// first that does.
pub fn choose(runtimes: &[Descriptor], preferred: Option<&str>) -> Option<Descriptor> {
    if let Some(preferred) = preferred {
        if let Some(found) = runtimes.iter().find(|r| r.id == preferred && r.available()) {
            return Some(found.clone());
        }
    }
    runtimes
        .iter()
        .find(|r| !r.hidden && r.available())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::{choose, discover, Descriptor, Places};
    use std::path::PathBuf;

    fn places(root: &std::path::Path) -> Places {
        Places {
            data: root.join("data"),
            runtime: root.join("data/runtime"),
            resources: root.join("resources"),
            own: PathBuf::new(),
        }
    }

    fn write(path: &std::path::Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn a_descriptor_says_where_its_placeholders_point() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("resources/runtimes/mlx.json");
        write(
            &path,
            r#"{"id":"mlx","name":"Apple silicon","command":"{runtime}/bin/python",
                "args":["{resources}/sidecar/engine.py"],"env":{"YARNGO_DATA":"{data}"}}"#,
        );
        let descriptor = Descriptor::read(&path, &places(dir.path())).expect("read");
        assert_eq!(descriptor.program(), dir.path().join("data/runtime/bin/python"));
        let command = descriptor.command();
        let args: Vec<_> = command.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args, [dir.path().join("resources/sidecar/engine.py").to_string_lossy()]);
        let env: Vec<_> = command
            .get_envs()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned())))
            .collect();
        assert_eq!(
            env,
            [("YARNGO_DATA".to_string(), Some(dir.path().join("data").to_string_lossy().into_owned()))]
        );
    }

    /// `{self}` is what lets a runtime ship its own interpreter beside itself
    /// without knowing where it will be installed.
    #[test]
    fn a_runtime_can_point_at_its_own_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data/runtimes/piper/runtime.json");
        write(&path, r#"{"id":"piper","name":"Piper","command":"{self}/bin/piper"}"#);
        let descriptor = Descriptor::read(&path, &places(dir.path())).expect("read");
        assert_eq!(
            descriptor.program(),
            dir.path().join("data/runtimes/piper/bin/piper")
        );
    }

    #[test]
    fn installed_runtimes_are_found_beside_the_shipped_one() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("resources/runtimes/mlx.json"),
            r#"{"id":"mlx","name":"Apple silicon","command":"/usr/bin/true"}"#,
        );
        write(
            &dir.path().join("data/runtimes/piper/runtime.json"),
            r#"{"id":"piper","name":"Piper","command":"/usr/bin/true"}"#,
        );
        let found = discover(&places(dir.path()));
        assert_eq!(
            found.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["mlx", "piper"],
            "the shipped runtime comes first"
        );
    }

    /// A name is not a claim on it. An installed runtime calling itself `mlx`
    /// must not quietly become the engine the application starts.
    #[test]
    fn an_installed_runtime_cannot_take_the_shipped_ones_name() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("resources/runtimes/mlx.json"),
            r#"{"id":"mlx","name":"Shipped","command":"/usr/bin/true"}"#,
        );
        write(
            &dir.path().join("data/runtimes/mlx/runtime.json"),
            r#"{"id":"mlx","name":"Impostor","command":"/usr/bin/false"}"#,
        );
        let found = discover(&places(dir.path()));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Shipped");
    }

    #[test]
    fn a_runtime_whose_program_is_gone_is_not_offered() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("resources/runtimes/here.json"),
            r#"{"id":"here","name":"Here","command":"/usr/bin/true"}"#,
        );
        write(
            &dir.path().join("resources/runtimes/gone.json"),
            r#"{"id":"gone","name":"Gone","command":"/nowhere/at/all"}"#,
        );
        let found = discover(&places(dir.path()));
        assert_eq!(found.len(), 2, "both are described");
        assert_eq!(choose(&found, None).expect("one works").id, "here");
        assert!(
            choose(&found, Some("gone")).is_some_and(|r| r.id == "here"),
            "a preference for something that is not there falls back rather than failing"
        );
    }

    #[test]
    fn nonsense_is_skipped_rather_than_fatal() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("resources/runtimes/broken.json"), "not json at all");
        write(&dir.path().join("resources/runtimes/empty.json"), r#"{"id":"","command":""}"#);
        write(
            &dir.path().join("resources/runtimes/fine.json"),
            r#"{"id":"fine","name":"Fine","command":"/usr/bin/true"}"#,
        );
        let found = discover(&places(dir.path()));
        assert_eq!(found.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["fine"]);
    }
}
