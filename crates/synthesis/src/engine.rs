//! The application's engine, when Rust owns what the application knows.
//!
//! The same interface the interface already talks to, with the work behind it
//! split by who owns what: the database answers for clips, voices and consent,
//! and the sidecar answers for models and inference. Nothing asks the sidecar
//! what the person has.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::json;
use speech_engine::protocol::{Connection, Events};
use speech_engine::Generating;
use speech_engine::{
    Capabilities, Clip, DiskSpace, EngineError, InstallStatus, ModelSpec, SpeechEngine, Started,
    Synthesis, SynthesisRequest, SystemInfo, Voice,
};
use yarngo_store::deletion::{Conditioning, Invalidation, Outcome as DeletionOutcome};
use yarngo_store::Store;

use crate::{library, Layout, Outcome, Reference, Request};

/// Anything that is not inference. A model that cannot answer these has stopped
/// being an engine.
const PROMPT: Duration = Duration::from_secs(60);
/// For work that is not inference but waits behind it: the engine runs one
/// thing at a time, so a short operation asked for during a generation waits
/// for that generation.
const BEHIND_THE_MODEL: Duration = Duration::from_secs(900);
/// How long a generation asked to stop is given to stop.
///
/// Short, because the person asked for the voice to go and whatever is being
/// generated with it was going to be refused anyway. What waiting buys is an
/// engine that stays up rather than one that has to load its model again, and
/// that is worth a little and not much: cancellation is checked between
/// chunks, so a chunk already running has to finish first.
const GRACE: Duration = Duration::from_secs(30);
/// Below this, a take is mostly fixed cost — warming caches, the first pass
/// through the model — and its rate says nothing about how long a real clip
/// will take.
const RATE_FROM_TAKES_LONGER_THAN: f64 = 3.0;

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
    session: String,
}

/// What the engine has said about the generation it is running.
///
/// Sent rather than written to a file, because this engine can speak while it
/// works. Kept as the latest rather than as a stream: the interface draws where
/// the generation is now, and every earlier answer to that is out of date.
#[derive(Clone, Default)]
pub struct Reported(Arc<Mutex<Option<Generating>>>);

impl Reported {
    pub fn latest(&self) -> Option<Generating> {
        self.0.lock().ok().and_then(|held| held.clone())
    }

    fn clear(&self) {
        if let Ok(mut held) = self.0.lock() {
            *held = None;
        }
    }

    /// Follow one connection's events until it ends.
    fn follow(&self, events: Events) {
        let held = self.0.clone();
        std::thread::spawn(move || {
            while let Ok(event) = events.recv_timeout(Duration::from_secs(3600)) {
                if event.method != "job.progress" {
                    continue;
                }
                let reported = serde_json::from_value(event.params).ok();
                if let Ok(mut latest) = held.lock() {
                    *latest = reported;
                }
            }
        });
    }
}

/// A synthesis the engine has and has not answered, and what publishing it
/// will need.
struct InFlight {
    work: crate::Dispatched,
    clip_id: String,
    model: String,
}

/// A deletion whose voice still has work the engine has not finished with.
///
/// The recording cannot go while something might still be reading it, and the
/// job cannot be called cancelled until its attempt has actually ended. So the
/// barrier goes up at once and the rest waits here for the attempts named in
/// `awaiting` to settle.
struct PendingDeletion {
    voice_id: String,
    job_id: String,
    awaiting: Vec<String>,
    deadline: Instant,
    /// The engine was asked to stop, would not, and was ended. Recorded so it
    /// is not ended a second time while the replies are still arriving.
    forced: bool,
}

pub struct DurableEngine {
    store: Store,
    layout: Layout,
    running: Running,
    spawn: Spawn,
    progress: Reported,
    /// What distinguishes this run's names from every other run's.
    ///
    /// Sessions, jobs and attempts all carry it. Numbering them from one within
    /// a run made every run produce the same names, so the second one could not
    /// insert a session at all and would not have been able to insert a job.
    run: u128,
    jobs: u64,
    /// Keyed by the request whose reply finishes it. Held here rather than on
    /// the caller's stack, because the caller returned as soon as the engine
    /// had been given the work.
    in_flight: HashMap<u64, InFlight>,
    deleting: Vec<PendingDeletion>,
    grace: Duration,
}

impl DurableEngine {
    /// Open the database, start a sidecar, and account for whatever the last
    /// run left behind before anything new is allowed to start.
    pub fn open(database: &Path, data_dir: &Path, spawn: Spawn) -> Result<Self, EngineError> {
        let mut store = Store::open(database).map_err(store_error)?;
        let layout = Layout::under(data_dir);
        layout.prepare().map_err(|e| EngineError::Transport(e.to_string()))?;

        // Whatever was running when this last stopped is over, whether or not
        // anything said so at the time. A run that was killed said nothing, so
        // its session is closed here first — otherwise its attempts stay
        // recorded as running and there is nothing for recovery to find.
        let at = now();
        store
            .end_abandoned_sessions(&at, "process_exited")
            .map_err(store_error)?;
        store.reconcile_ended_sessions(&at).map_err(store_error)?;
        adopt_legacy(&mut store, data_dir)?;

        let (connection, events) = spawn.start()?;
        let progress = Reported::default();
        progress.follow(events);
        let run = stamp();
        let session = format!("session-{run}");
        store
            .open_session(&session, "mlx", &now())
            .map_err(store_error)?;
        let mut engine = Self {
            store,
            layout,
            running: Running { connection, session },
            spawn,
            progress,
            run,
            jobs: 0,
            in_flight: HashMap::new(),
            deleting: Vec::new(),
            grace: GRACE,
        };
        engine.finish_what_was_left()?;
        Ok(engine)
    }

    /// How long a generation asked to stop is given before the process running
    /// it is ended. For tests, which cannot spend the real grace on every case
    /// that reaches the end of it.
    pub fn with_grace(mut self, grace: Duration) -> Self {
        self.grace = grace;
        self
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
                progress: &self.progress,
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

    /// Stop every synthesis still open for this voice.
    ///
    /// A job that was never dispatched is cancelled outright: nothing is
    /// running, so there is nothing to ask. One that is running is recorded as
    /// asked to stop and then asked — which reaches the engine while its
    /// synthesis is still pending, because the connection carries ids.
    ///
    /// Returns the attempts that have to end before the recording can go.
    fn stop_work_for(&mut self, voice_id: &str, at: &str) -> Result<Vec<String>, EngineError> {
        let open = self
            .store
            .open_synthesis_for_voice(voice_id)
            .map_err(store_error)?;
        let mut awaiting = Vec::new();
        for mut job in open {
            match job.current_execution().map(str::to_string) {
                None => {
                    job.request_cancel();
                    self.store.save_progress(&job, None, at).map_err(store_error)?;
                }
                Some(execution_id) => {
                    self.request_stop(&job.id, &execution_id)?;
                    if self.running_now(&execution_id) {
                        awaiting.push(execution_id);
                    }
                }
            }
        }
        Ok(awaiting)
    }

    /// Whether this engine is the one running that attempt.
    ///
    /// An attempt named by a job but not here belongs to a session that has
    /// ended, and nothing is waiting for it to stop.
    fn running_now(&self, execution_id: &str) -> bool {
        self.in_flight
            .values()
            .any(|work| work.work.execution.id == execution_id)
    }

    /// Finish deletions whose work has ended.
    fn advance_deletions(&mut self) {
        for deletion in &mut self.deleting {
            deletion
                .awaiting
                .retain(|execution_id| {
                    self.in_flight
                        .values()
                        .any(|work| &work.work.execution.id == execution_id)
                });
        }
        let ready: Vec<(String, String)> = self
            .deleting
            .iter()
            .filter(|deletion| deletion.awaiting.is_empty())
            .map(|deletion| (deletion.voice_id.clone(), deletion.job_id.clone()))
            .collect();
        self.deleting.retain(|deletion| !deletion.awaiting.is_empty());
        for (voice_id, job_id) in ready {
            if let Err(failure) = self.complete_deletion(&voice_id, &job_id) {
                eprintln!("could not finish deleting {voice_id}: {failure}");
            }
        }
    }

    /// Forget it, or end the process that will not.
    fn complete_deletion(&mut self, voice_id: &str, job_id: &str) -> Result<(), EngineError> {
        let mut conditioning = EngineConditioning {
            running: &mut self.running,
            spawn: &self.spawn,
            progress: &self.progress,
        };
        let outcome = self
            .store
            .finish_voice_deletion(voice_id, job_id, &mut conditioning, &now())
            .map_err(store_error)?;
        match outcome {
            DeletionOutcome::Deleted { .. } | DeletionOutcome::AlreadyDeleted => Ok(()),
            // The recording stays and the voice stays refused for new work.
            // Said plainly rather than reported as success: nothing here can
            // show the engine has forgotten it.
            DeletionOutcome::Blocked { reason } => Err(EngineError::Transport(reason)),
        }
    }

    /// Ask one attempt to stop.
    ///
    /// The job is moved and persisted before anything is sent. A process that
    /// disappears between the two leaves a job recorded as asked to stop, which
    /// a restart can finish; the other order leaves one nobody knows was
    /// cancelled and an engine still working on it.
    fn request_stop(&mut self, job_id: &str, execution_id: &str) -> Result<(), EngineError> {
        let at = now();
        if let Some(work) = self.in_flight.values_mut().find(|w| w.work.job.id == job_id) {
            work.work.job.request_cancel();
            self.store
                .save_progress(&work.work.job, Some(&work.work.execution), &at)
                .map_err(store_error)?;
        }
        self.running
            .connection
            .request(
                "job.cancel",
                json!({ "job_id": job_id, "execution_id": execution_id }),
                PROMPT,
            )
            .map(|_| ())
            .map_err(|e| EngineError::Transport(format!("{e}")))
    }

    fn next_job(&mut self) -> (String, String) {
        self.jobs += 1;
        (
            format!("job-{}-{}", self.run, self.jobs),
            format!("job-{}-{}/1", self.run, self.jobs),
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
        let mut models: Vec<ModelSpec> =
            serde_json::from_value(reply["models"].clone()).unwrap_or_default();
        // How fast a model has been is a fact about the person's own takes, so
        // it is answered from where those are kept. The engine knowing it would
        // mean the engine reading them.
        for model in &mut models {
            model.measured_rtf = self
                .store
                .measured_rate(&model.id, RATE_FROM_TAKES_LONGER_THAN)
                .map_err(store_error)?
                .map(|rate| rate as f32);
        }
        Ok(models)
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

    fn ping(&mut self) -> Result<(), EngineError> {
        self.call("ping", json!({}), PROMPT).map(|_| ())
    }

    /// Asked for on the same connection the generation is running on, which is
    /// the point of the connection carrying ids.
    ///
    /// The job is moved and persisted first. If this process disappears between
    /// the two, a restart finds a job that was asked to stop and finishes the
    /// asking; the other order finds a job nobody knows was cancelled.
    fn cancel_generation(&mut self) -> Result<(), EngineError> {
        let running: Vec<(String, String)> = self
            .in_flight
            .values()
            .map(|work| (work.work.job.id.clone(), work.work.execution.id.clone()))
            .collect();
        for (job_id, execution_id) in running {
            self.request_stop(&job_id, &execution_id)?;
        }
        Ok(())
    }

    /// A generation asked to stop and still going is ended.
    ///
    /// Cooperative cancellation is checked between chunks, so an engine part
    /// way through one takes as long as that chunk. Past the grace, the answer
    /// is not to wait longer: the person asked for the voice to go, and a
    /// process that has been asked to stop and has not is a process that can
    /// still speak in it. Ending it is what makes that untrue.
    ///
    /// Its pending requests then fail, which arrives here as the attempt being
    /// interrupted, and the deletion finishes on that.
    fn attend(&mut self) {
        let overdue = self
            .deleting
            .iter()
            .any(|deletion| !deletion.forced && Instant::now() >= deletion.deadline);
        if overdue {
            let mut conditioning = EngineConditioning {
                running: &mut self.running,
                spawn: &self.spawn,
                progress: &self.progress,
            };
            let ended = conditioning.terminate();
            // A replacement failing leaves the application without an engine,
            // which is a problem — and not one that justifies keeping a
            // recording somebody asked to delete.
            let replaced = conditioning.restart();
            for deletion in &mut self.deleting {
                deletion.forced = true;
            }
            if let Err(failure) = ended {
                eprintln!("could not end the engine holding a deleted voice: {failure}");
            }
            if let Err(failure) = replaced {
                eprintln!("no engine is running: {failure}");
            }
        }
        self.advance_deletions();
    }

    fn progress(&mut self) -> Option<Generating> {
        self.progress.latest()
    }

    fn disk_space(&mut self) -> Result<DiskSpace, EngineError> {
        // From the one answer about this machine, rather than a second method
        // asking half of the same question.
        let reply = self.call("system_info", json!({}), PROMPT)?;
        serde_json::from_value(reply).map_err(|e| EngineError::Transport(e.to_string()))
    }

    /// Conditioning takes about forty seconds, which is long enough that a
    /// deletion arriving during it must not queue behind it.
    fn start_preparation(
        &mut self,
        voice_id: &str,
        model: Option<&str>,
    ) -> Result<Started<f32>, EngineError> {
        let Some((_, audio, _)) = library::reference(&self.store, voice_id).map_err(store_error)?
        else {
            return Err(EngineError::Transport(format!("no such voice: {voice_id}")));
        };
        let sent = self
            .running
            .connection
            .send("conditioning.prepare", json!({ "reference_audio": audio, "model": model }))
            .map_err(|e| EngineError::Transport(format!("{e}")))?;
        Ok(Started::Awaiting(sent))
    }

    fn finish_preparation(
        &mut self,
        _id: u64,
        reply: Result<serde_json::Value, EngineError>,
    ) -> Result<f32, EngineError> {
        Ok(reply?["prepared_s"].as_f64().unwrap_or(0.0) as f32)
    }

    fn voices(&mut self) -> Result<Vec<Voice>, EngineError> {
        library::voices(&self.store).map_err(store_error)
    }

    /// Enrol a voice: put the recording where it belongs, then record it.
    ///
    /// The recorder leaves its take in a temporary file, and a saved voice
    /// cannot depend on one. Where it goes is decided here; the engine cuts the
    /// silence off it and says how long what is left runs, because that needs
    /// an audio stack and nothing else here has one.
    fn register_voice(&mut self, voice: &Voice) -> Result<(), EngineError> {
        self.layout.prepare().map_err(|e| EngineError::Transport(e.to_string()))?;
        let stored = self.layout.reference(&voice.voice_id);
        let prepared = self.call(
            "audio.prepare_reference",
            json!({
                "source": voice.reference_audio.to_string_lossy(),
                "output_path": stored.to_string_lossy(),
            }),
            BEHIND_THE_MODEL,
        )?;
        let enrolled = Voice {
            reference_audio: stored,
            seconds: prepared["seconds"].as_f64().unwrap_or(0.0) as f32,
            ..voice.clone()
        };
        library::register_voice(&mut self.store, &enrolled, &now()).map_err(store_error)
    }

    fn rename_voice(&mut self, voice_id: &str, label: &str) -> Result<Vec<Voice>, EngineError> {
        library::rename_voice(&self.store, voice_id, label).map_err(store_error)?;
        self.voices()
    }

    /// Put the barrier up, stop what is running, and finish when it has.
    ///
    /// Returns once the voice is refused: from that moment nothing new can be
    /// conditioned with it, generated with it, or published against it. What
    /// comes after — an attempt actually ending, the recording leaving the disk
    /// — waits for work already running, and cannot be waited for here: the
    /// reply that settles that work arrives on this same thread.
    fn delete_voice(&mut self, voice_id: &str) -> Result<(), EngineError> {
        let (job_id, _) = self.next_job();
        let at = now();
        let began = self
            .store
            .begin_voice_deletion(voice_id, &job_id, &at)
            .map_err(store_error)?;
        if !began {
            // Already gone. Answered against the tombstone rather than started
            // a second time.
            return Ok(());
        }
        let awaiting = self.stop_work_for(voice_id, &at)?;
        if awaiting.is_empty() {
            return self.complete_deletion(voice_id, &job_id);
        }
        self.deleting.push(PendingDeletion {
            voice_id: voice_id.to_string(),
            job_id,
            awaiting,
            deadline: Instant::now() + self.grace,
            forced: false,
        });
        Ok(())
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


    fn start_synthesis(
        &mut self,
        request: &SynthesisRequest,
    ) -> Result<Started<Synthesis>, EngineError> {
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
                    // Refused here rather than after a minute of inference —
                    // and refused, not failed: the voice is gone because
                    // somebody removed it, which is the deletion working.
                    return Err(EngineError::Refused(
                        "the voice this clip was made with was deleted".into(),
                    ));
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

        let (work, sent) = {
            let mut synthesis = crate::Synthesis {
                store: &mut self.store,
                layout: &self.layout,
                session_id: &self.running.session,
            };
            let pending = synthesis
                .begin(&job_id, &execution_id, &asked, &at)
                .map_err(|e| EngineError::Transport(e.to_string()))?;
            synthesis
                .dispatch(pending, &self.running.connection, &at)
                .map_err(|e| EngineError::Transport(e.to_string()))?
        };
        self.in_flight
            .insert(sent.id(), InFlight { work, clip_id, model });
        Ok(Started::Awaiting(sent))
    }

    fn finish_synthesis(
        &mut self,
        id: u64,
        reply: Result<serde_json::Value, EngineError>,
    ) -> Result<Synthesis, EngineError> {
        let Some(InFlight { work, clip_id, model }) = self.in_flight.remove(&id) else {
            return Err(EngineError::Transport(
                "a reply arrived for a synthesis nothing was waiting for".into(),
            ));
        };
        // Progress describes a generation that is running. Cleared once none
        // is, because a value left behind draws a bar for work that finished.
        if self.in_flight.is_empty() {
            self.progress.clear();
        }
        let ended = self.running.connection.has_ended();
        let outcome = {
            let mut synthesis = crate::Synthesis {
                store: &mut self.store,
                layout: &self.layout,
                session_id: &self.running.session,
            };
            let finished = synthesis
                .arrived(work, reply, ended, &now())
                .map_err(|e| EngineError::Transport(e.to_string()))?;
            match finished {
                crate::Finished::Produced { mut job, execution, output } => synthesis
                    .publish(&mut job, &execution, &output, &now())
                    .map_err(|e| EngineError::Transport(e.to_string()))?,
                crate::Finished::Settled(outcome) => outcome,
            }
        };

        // Something may have been waiting for exactly this attempt to end.
        self.advance_deletions();

        let Outcome::Published { take_id, path } = outcome else {
            return Err(match outcome {
                // Nothing went wrong: it was stopped, or what it made was not
                // wanted. Both are answers, not faults.
                Outcome::Rejected { detail, .. } => EngineError::Refused(detail),
                Outcome::Cancelled => EngineError::Refused("the generation was stopped".into()),
                Outcome::Failed { detail } => EngineError::Transport(detail),
                Outcome::Interrupted => EngineError::NotRunning,
                Outcome::Published { .. } => unreachable!(),
            });
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
    progress: &'a Reported,
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
        // Whatever the old engine last said it was doing, it is not doing now.
        self.progress.clear();
        self.progress.follow(events);
        Ok(())
    }
}
