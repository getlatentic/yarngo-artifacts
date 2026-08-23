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
//! # A descriptor is downloaded data
//!
//! Which is the whole reason it does not say what to run. An earlier version
//! carried `command`, `args` and `env` verbatim, which made it a second way to
//! execute anything on the machine — `/bin/sh -c …` outright, or quietly
//! through `PYTHONPATH` and `DYLD_INSERT_LIBRARIES`, without any of it looking
//! like an instruction to run something.
//!
//! The application owns the launch. A descriptor chooses only from what the
//! application already knows how to do:
//!
//! ```json
//! {
//!   "schema": 1,
//!   "id": "mlx",
//!   "name": "Apple silicon",
//!   "engine": "own",
//!   "program": "{venv}/bin/python3",
//!   "arguments": ["{engine}"]
//! }
//! ```
//!
//! `program` is either inside the environment the application installed for
//! this runtime, or a path inside the runtime's own directory — never absolute,
//! never climbing out. `{engine}` is whichever implementation the application
//! resolved, its own or the one that shipped. The environment is the
//! application's to set.
//!
//! None of this makes a runtime safe. Its code is executable and it was
//! downloaded; that risk is answered by verifying who published it, not here.
//! What this prevents is the descriptor being a *second* way in, and an archive
//! quietly escaping the directory it was installed into.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

/// The descriptor shapes this build knows how to read. A descriptor declaring
/// a newer one describes an arrangement this application cannot honour, so it
/// is refused rather than half-understood.
pub const DESCRIPTOR_SCHEMA: u32 = 1;

/// Where a descriptor's placeholders point.
#[derive(Clone, Debug, PartialEq)]
pub struct Places {
    /// Everything the application keeps.
    pub data: PathBuf,
    /// What shipped with the application.
    pub resources: PathBuf,
}

/// Which implementation of the protocol runs this runtime.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    /// The copy that ships with the application. For a runtime that installs
    /// dependencies the shipped engine already knows how to drive.
    #[default]
    Bundled,
    /// The runtime's own, in its own directory. Speaking the same protocol is
    /// not the same as being the same implementation, so this is never
    /// substituted for the bundled one: a runtime whose code is missing cannot
    /// be started, rather than being started by something else.
    Own,
}

/// How to start one runtime.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Descriptor {
    #[serde(default = "one")]
    pub schema: u32,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub engine: Engine,
    /// Inside this runtime's environment when it begins `{venv}` — the
    /// `.venv` in the descriptor's own directory — otherwise inside the
    /// runtime's own directory. Never absolute, never climbing out. Everything
    /// a runtime runs lives beside it, which is what lets one version be
    /// installed while another answers.
    pub program: String,
    /// Passed through as written, except for `{engine}`, which is the
    /// implementation the application resolved. Arguments are not paths the
    /// application resolves: the program they are given to is already
    /// constrained, so they are flags to it and nothing more.
    #[serde(default)]
    pub arguments: Vec<String>,
    /// Skipped when choosing automatically, and still startable by name.
    #[serde(default)]
    pub hidden: bool,
    #[serde(skip)]
    home: Option<Home>,
}

fn one() -> u32 {
    1
}

/// Where one descriptor's own things are, filled in when it is read.
#[derive(Clone, Debug, PartialEq)]
struct Home {
    places: Places,
    own: PathBuf,
}

impl Descriptor {
    /// Read one, and refuse it if it asks for anything it may not have.
    ///
    /// `None` rather than an error for anything malformed: a directory of
    /// runtimes is a place other things put files, and one unreadable entry is
    /// not a reason to have no runtimes.
    pub fn read(path: &Path, places: &Places) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        let mut descriptor: Descriptor = serde_json::from_str(&text).ok()?;
        let own = path.parent()?.to_path_buf();
        descriptor.home = Some(Home { places: places.clone(), own });
        descriptor.acceptable().ok()?;
        Some(descriptor)
    }

    /// Whether this is a descriptor this application will act on.
    pub fn acceptable(&self) -> Result<(), String> {
        if self.schema > DESCRIPTOR_SCHEMA {
            return Err(format!(
                "describes schema {} and this application knows {DESCRIPTOR_SCHEMA}",
                self.schema
            ));
        }
        if self.id.is_empty()
            || !self
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            // It becomes a directory name, and one that could contain a
            // separator would decide where the runtime lives.
            return Err(format!("{:?} is not a runtime name", self.id));
        }
        if self.name.trim().is_empty() {
            return Err("has nothing to call itself".into());
        }
        self.program_path().map(|_| ())
    }

    /// The program to run, refused if it names anywhere it may not.
    pub fn program_path(&self) -> Result<PathBuf, String> {
        let Some(home) = &self.home else {
            return Err("was not read from anywhere".into());
        };
        let (root, rest) = match self.program.strip_prefix("{venv}/") {
            // The environment built for this runtime, beside the descriptor.
            // Not a shared location: two versions of one runtime each own an
            // environment, and which one runs is decided by which directory
            // this descriptor was read from.
            Some(rest) => (home.own.join(".venv"), rest),
            None => (home.own.clone(), self.program.as_str()),
        };
        if self.program.contains('{') && !self.program.starts_with("{venv}/") {
            return Err(format!("program {:?} uses a placeholder there is no such thing as", self.program));
        }
        inside(&root, Path::new(rest)).map_err(|why| format!("program {why}"))
    }

    /// The implementation of the protocol that runs it.
    pub fn engine_path(&self) -> Result<PathBuf, String> {
        let Some(home) = &self.home else {
            return Err("was not read from anywhere".into());
        };
        match self.engine {
            Engine::Own => {
                let own = home.own.join("engine.py");
                if !own.exists() {
                    // Never the bundled one instead. Speaking this protocol is
                    // not the same as knowing this runtime, and starting the
                    // wrong implementation is worse than not starting.
                    return Err(format!(
                        "says it brought its own engine and {} is not there",
                        own.display()
                    ));
                }
                Ok(own)
            }
            Engine::Bundled => Ok(home.places.resources.join("sidecar").join("engine.py")),
        }
    }

    /// Whether everything it needs is actually here.
    pub fn available(&self) -> bool {
        self.program_path().is_ok_and(|p| p.exists()) && self.engine_path().is_ok()
    }

    /// The command that starts it.
    ///
    /// The environment is the application's: a downloaded descriptor setting
    /// `PYTHONPATH` or `DYLD_INSERT_LIBRARIES` would be running its own code
    /// without ever naming a program. What the application does set is where
    /// its own things are — the data directory, and the catalogue of models it
    /// offers. A runtime running its own engine from its own directory has no
    /// way to find either by looking around itself, and guessing is how an
    /// engine ends up with no models and no explanation.
    pub fn command(&self) -> Result<Command, String> {
        let home = self.home.as_ref().ok_or("was not read from anywhere")?;
        let engine = self.engine_path()?;
        let mut command = Command::new(self.program_path()?);
        for argument in &self.arguments {
            command.arg(argument.replace("{engine}", &engine.to_string_lossy()));
        }
        command.env("YARNGO_DATA", &home.places.data);
        command.env("YARNGO_CATALOG", home.places.resources.join("catalog.json"));
        command.current_dir(&home.own);
        Ok(command)
    }
}

/// Where a relative path lands under a root, or why it may not land at all.
fn inside(root: &Path, path: &Path) -> Result<PathBuf, String> {
    use std::path::Component;
    if path.is_absolute() {
        return Err(format!("{} is an absolute path", path.display()));
    }
    let mut landing = root.to_path_buf();
    for part in path.components() {
        match part {
            Component::Normal(part) => landing.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(format!("{} climbs out of the runtime", path.display()))
            }
            other => return Err(format!("{} contains {other:?}", path.display())),
        }
    }
    if !landing.starts_with(root) {
        return Err(format!("{} resolves outside the runtime", path.display()));
    }
    Ok(landing)
}
