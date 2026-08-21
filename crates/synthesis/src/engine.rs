//! The application's engine, when Rust owns what the application knows.
//!
//! The same interface the interface already talks to, with the work behind it
//! split by who owns what: the database answers for clips, voices and consent,
//! and the sidecar answers for models and inference. Nothing asks the sidecar
//! what the person has.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::json;
use speech_engine::protocol::{Connection, Events};
use speech_engine::{
    Capabilities, Clip, DiskSpace, EngineError, InstallStatus, ModelSpec, SpeechEngine, Synthesis,
    SynthesisRequest, SystemInfo, Voice,
};
use yarngo_store::deletion::{Conditioning, Invalidation, Outcome as DeletionOutcome};
use yarngo_store::Store;

use crate::{library, Layout, Outcome, Reference, Request};

/// Long enough for a cold model load and a minute of speech; short enough that
/// a wedged engine is eventually reported rather than waited on for ever.
const PATIENCE: Duration = Duration::from_secs(900);
/// Anything that is not inference. A model that cannot answer these has stopped
/// being an engine.
const PROMPT: Duration = Duration::from_secs(60);

/// How to start a sidecar, kept because deleting a voice may have to end one
/// and put another in its place.
#[derive(Clone, Debug)]
pub struct Spawn {
    pub python: PathBuf,
    pub script: PathBuf,
    pub work_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl Spawn {
    fn start(&self) -> Result<(Connection, Events), EngineError> {
        let mut command = Command::new(&self.python);
        command
            .arg(&self.script)
            .arg("--protocol")
            .arg("jsonrpc")
            .current_dir(&self.work_dir);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command
            .env("YARNGO_DATA", &self.data_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| EngineError::Transport(format!("could not start sidecar: {e}")))?;
        let stdin = child.stdin.take().ok_or(EngineError::NotRunning)?;
        let stdout = child.stdout.take().ok_or(EngineError::NotRunning)?;
        let (connection, events) = Connection::attach(child, stdin, stdout, None);
        connection
            .initialize(PROMPT)
            .map_err(|e| EngineError::Transport(format!("{e}")))?;
        Ok((connection, events))
    }
}

/// The sidecar currently answering, and the session its work belongs to.
struct Running {
    connection: Connection,
    _events: Events,
    session: String,
}

pub struct DurableEngine {
    store: Store,
    layout: Layout,
    running: Running,
    spawn: Spawn,
    sessions: u64,
    jobs: u64,
}

impl DurableEngine {
    /// Open the database, start a sidecar, and account for whatever the last
    /// run left behind before anything new is allowed to start.
    pub fn open(database: &Path, data_dir: &Path, spawn: Spawn) -> Result<Self, EngineError> {
        let mut store = Store::open(database).map_err(store_error)?;
        let layout = Layout::under(data_dir);
        layout.prepare().map_err(|e| EngineError::Transport(e.to_string()))?;

        // Whatever was running when this last stopped is over, whether or not
        // anything said so at the time.
        store.reconcile_ended_sessions(&now()).map_err(store_error)?;
        adopt_legacy(&mut store, data_dir)?;

        let (connection, events) = spawn.start()?;
        let session = "session-1".to_string();
        store
            .open_session(&session, "mlx", &now())
            .map_err(store_error)?;
        let mut engine = Self {
            store,
            layout,
            running: Running { connection, _events: events, session },
            spawn,
            sessions: 1,
            jobs: 0,
        };
        engine.finish_what_was_left()?;
        Ok(engine)
    }

    /// Publications the last run did not reach, and audio nothing will claim.
    fn finish_what_was_left(&mut self) -> Result<(), EngineError> {
        let at = now();
        let mut synthesis = crate::Synthesis {
            store: &mut self.store,
            layout: &self.layout,
            session_id: &self.running.session,
        };
        synthesis
            .reconcile(&at)
            .map_err(|e| EngineError::Transport(e.to_string()))?;
        // Deletions the last run began and could not finish. Until one is
        // finished the voice is refused for new work, so this runs before
        // anything is offered.
        let unfinished = self.store.unfinished_voice_deletions().map_err(store_error)?;
        for (voice_id, job_id) in unfinished {
            let mut conditioning = EngineConditioning {
                running: &mut self.running,
                spawn: &self.spawn,
            };
            self.store
                .finish_voice_deletion(&voice_id, &job_id, &mut conditioning, &at)
                .map_err(store_error)?;
        }
        Ok(())
    }

    fn call(&self, method: &str, params: serde_json::Value, patience: Duration) -> Result<serde_json::Value, EngineError> {
        self.running
            .connection
            .request(method, params, patience)
            .map_err(|e| EngineError::Transport(format!("{e}")))
    }

    fn next_job(&mut self) -> (String, String) {
        self.jobs += 1;
        (
            format!("job-{}-{}", self.sessions, self.jobs),
            format!("job-{}-{}/1", self.sessions, self.jobs),
        )
    }
}

fn store_error(error: yarngo_store::StoreError) -> EngineError {
    EngineError::Transport(error.to_string())
}

/// The same format the store already holds, in local time, so a clip made today
/// sorts against one made last month rather than against a different clock.
fn now() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

fn stamp() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default()
}

/// A working name taken from the first words, and the person's to change.
fn working_name(text: &str) -> String {
    let name: String = text
        .split_whitespace()
        .take(5)
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|c: char| " .,;:!?—-".contains(c))
        .to_string();
    if name.is_empty() {
        "Untitled clip".into()
    } else {
        name
    }
}

impl SpeechEngine for DurableEngine {
    fn capabilities(&self) -> Capabilities {
        Capabilities { cloning: true, streaming: false }
    }

    fn models(&mut self, refresh: bool) -> Result<Vec<ModelSpec>, EngineError> {
        let reply = self.call("model.list", json!({ "refresh": refresh }), PROMPT)?;
        Ok(serde_json::from_value(reply["models"].clone()).unwrap_or_default())
    }

    fn delete_model(&mut self, model: &str) -> Result<u64, EngineError> {
        let reply = self.call("model.delete", json!({ "model": model }), PROMPT)?;
        Ok(reply["freed_bytes"].as_u64().unwrap_or(0))
    }

    fn install_model(&mut self, model: &str) -> Result<InstallStatus, EngineError> {
        let reply = self.call("model.install", json!({ "model": model }), PROMPT)?;
        serde_json::from_value(reply).map_err(|e| EngineError::Transport(e.to_string()))
    }

    fn install_status(&mut self, model: &str) -> Result<InstallStatus, EngineError> {
        let reply = self.call("model.install_status", json!({ "model": model }), PROMPT)?;
        serde_json::from_value(reply).map_err(|e| EngineError::Transport(e.to_string()))
    }

    fn system_info(&mut self) -> Result<SystemInfo, EngineError> {
        let reply = self.call("system_info", json!({}), PROMPT)?;
        serde_json::from_value(reply).map_err(|e| EngineError::Transport(e.to_string()))
    }

    fn disk_space(&mut self) -> Result<DiskSpace, EngineError> {
        // From the one answer about this machine, rather than a second method
        // asking half of the same question.
        let reply = self.call("system_info", json!({}), PROMPT)?;
        serde_json::from_value(reply).map_err(|e| EngineError::Transport(e.to_string()))
    }

    fn prepare_voice(&mut self, voice_id: &str, model: Option<&str>) -> Result<f32, EngineError> {
        let Some((_, audio, _)) = library::reference(&self.store, voice_id).map_err(store_error)?
        else {
            return Err(EngineError::Transport(format!("no such voice: {voice_id}")));
        };
        let reply = self.call(
            "conditioning.prepare",
            json!({ "reference_audio": audio, "model": model }),
            PATIENCE,
        )?;
        Ok(reply["prepared_s"].as_f64().unwrap_or(0.0) as f32)
    }

    fn voices(&mut self) -> Result<Vec<Voice>, EngineError> {
        library::voices(&self.store).map_err(store_error)
    }

    fn register_voice(&mut self, voice: &Voice) -> Result<(), EngineError> {
        library::register_voice(&mut self.store, voice, &now()).map_err(store_error)
    }

    fn rename_voice(&mut self, voice_id: &str, label: &str) -> Result<Vec<Voice>, EngineError> {
        library::rename_voice(&self.store, voice_id, label).map_err(store_error)?;
        self.voices()
    }

    fn delete_voice(&mut self, voice_id: &str) -> Result<(), EngineError> {
        let (job_id, _) = self.next_job();
        let at = now();
        self.store
            .begin_voice_deletion(voice_id, &job_id, &at)
            .map_err(store_error)?;
        let mut conditioning = EngineConditioning {
            running: &mut self.running,
            spawn: &self.spawn,
        };
        let outcome = self
            .store
            .finish_voice_deletion(voice_id, &job_id, &mut conditioning, &at)
            .map_err(store_error)?;
        match outcome {
            DeletionOutcome::Deleted { .. } | DeletionOutcome::AlreadyDeleted => Ok(()),
            // The recording stays and the voice stays refused for new work. Said
            // plainly rather than reported as success: nothing here can show the
            // engine has forgotten it.
            DeletionOutcome::Blocked { reason } => Err(EngineError::Transport(reason)),
        }
    }

    fn clips(&mut self) -> Result<Vec<Clip>, EngineError> {
        library::clips(&self.store).map_err(store_error)
    }

    fn rename_clip(&mut self, clip_id: &str, name: &str) -> Result<Vec<Clip>, EngineError> {
        library::rename_clip(&self.store, clip_id, name).map_err(store_error)?;
        self.clips()
    }

    fn delete_clip(&mut self, clip_id: &str) -> Result<Vec<Clip>, EngineError> {
        library::delete_clip(&mut self.store, clip_id, &now()).map_err(store_error)?;
        self.clips()
    }

    fn duplicate_clip(&mut self, clip_id: &str) -> Result<Vec<Clip>, EngineError> {
        let new_id = format!("clip-{}", stamp());
        library::duplicate_clip(&self.store, clip_id, &new_id, &now()).map_err(store_error)?;
        self.clips()
    }

    fn synthesize(&mut self, request: &SynthesisRequest) -> Result<Synthesis, EngineError> {
        let at = now();
        let model = request.model.clone().unwrap_or_default();

        // Which voice this is spoken in belongs to the clip, so generating
        // again reads it from there rather than from whatever is selected now.
        let voice_id = match &request.clip_id {
            Some(clip) => library::clip_voice(&self.store, clip)
                .map_err(store_error)?
                .flatten(),
            None => request.voice_id.clone(),
        };
        let reference = match &voice_id {
            Some(voice) => {
                let Some((revision, audio, label)) =
                    library::reference(&self.store, voice).map_err(store_error)?
                else {
                    // Refused here rather than after a minute of inference.
                    return Err(EngineError::Transport(format!(
                        "the voice this clip was made with is no longer available"
                    )));
                };
                Some((revision, audio, label))
            }
            None => None,
        };

        let clip_id = match &request.clip_id {
            Some(clip) => clip.clone(),
            None => {
                let clip_id = format!("clip-{}", stamp());
                library::create_clip(
                    &self.store,
                    &clip_id,
                    &request.name.clone().unwrap_or_else(|| working_name(&request.text)),
                    request.text.trim(),
                    reference.as_ref().map(|(r, _, l)| (r.as_str(), l.as_str())),
                    &model,
                    &at,
                )
                .map_err(store_error)?;
                clip_id
            }
        };

        let (job_id, execution_id) = self.next_job();
        let asked = Request {
            clip_id: clip_id.clone(),
            text: request.text.trim().to_string(),
            reference: reference.map(|(_, audio, _)| Reference { audio, text: None }),
            model: request.model.clone(),
            seed: request.seed.map(|s| s as i64),
        };

        let outcome = {
            let mut synthesis = crate::Synthesis {
                store: &mut self.store,
                layout: &self.layout,
                session_id: &self.running.session,
            };
            let pending = synthesis
                .begin(&job_id, &execution_id, &asked, &at)
                .map_err(|e| EngineError::Transport(e.to_string()))?;
            synthesis
                .generate(pending, &self.running.connection, PATIENCE, &now())
                .map_err(|e| EngineError::Transport(e.to_string()))?
        };

        let Outcome::Published { take_id, path } = outcome else {
            return Err(EngineError::Transport(match outcome {
                Outcome::Rejected { detail, .. } => detail,
                Outcome::Failed { detail } => detail,
                Outcome::Cancelled => "the generation was stopped".into(),
                Outcome::Interrupted => "the engine stopped while generating".into(),
                Outcome::Published { .. } => unreachable!(),
            }));
        };

        // Read back rather than assembled from the reply: what the interface
        // shows should be what the database holds.
        let clip = library::clips(&self.store)
            .map_err(store_error)?
            .into_iter()
            .find(|c| c.id == clip_id);
        let take = clip.as_ref().and_then(|c| c.take(&take_id)).cloned();
        Ok(Synthesis {
            output: path,
            model: clip.as_ref().map(|c| c.model.clone()).unwrap_or(model),
            audio_s: take.as_ref().map(|t| t.audio_s).unwrap_or_default(),
            gen_s: take.as_ref().map(|t| t.gen_s).unwrap_or_default(),
            rtf: take.as_ref().and_then(|t| t.rtf()),
            seed: take.as_ref().and_then(|t| t.seed),
            sample_rate: 24_000,
            clip,
        })
    }
}

/// Bring across what the JSON store holds, once, the first time this runs.
///
/// Read-only, and skipped the moment there is anything here: the legacy files
/// stay exactly as they are, so the old path still works and switching back is
/// switching back rather than starting again. Running it twice compares rather
/// than skips, which is what makes it safe to attempt on every start.
fn adopt_legacy(store: &mut Store, data_dir: &Path) -> Result<(), EngineError> {
    let empty: bool = store
        .raw()
        .query_row("SELECT NOT EXISTS (SELECT 1 FROM clips UNION ALL SELECT 1 FROM voice_profiles)", [], |row| row.get(0))
        .map_err(|e| store_error(e.into()))?;
    if !empty {
        return Ok(());
    }
    let Some(legacy) = yarngo_store::import::Legacy::read(data_dir) else {
        return Ok(());
    };
    let report = store.import_legacy(&legacy).map_err(store_error)?;
    eprintln!(
        "adopted the existing store: {} clip(s), {} voice(s), {} take(s), {} consent record(s)",
        report.clips, report.voices, report.takes, report.consent_events
    );
    for missing in &report.missing_files {
        eprintln!("  a record points at a file that is not there: {missing}");
    }
    Ok(())
}

/// Making the engine forget a voice, and ending it when it will not.
struct EngineConditioning<'a> {
    running: &'a mut Running,
    spawn: &'a Spawn,
}

impl Conditioning for EngineConditioning<'_> {
    fn invalidate(&mut self) -> Result<Invalidation, String> {
        let reply = self
            .running
            .connection
            .request("conditioning.invalidate", json!({}), PROMPT)
            .map_err(|e| e.to_string())?;
        let removed = reply["entries_removed"].as_u64().unwrap_or(0);
        Ok(match reply["status"].as_str() {
            Some("already_empty") => Invalidation::AlreadyEmpty,
            _ => Invalidation::Cleared { entries_removed: removed },
        })
    }

    fn terminate(&mut self) -> Result<(), String> {
        // Killed, not asked. The question is whether the old process can still
        // speak in the voice, and one that has been asked politely and has not
        // answered is one that still can.
        self.running.connection.kill().map_err(|e| e.to_string())
    }

    fn restart(&mut self) -> Result<(), String> {
        let (connection, events) = self.spawn.start().map_err(|e| e.to_string())?;
        self.running.connection = connection;
        self.running._events = events;
        Ok(())
    }
}
