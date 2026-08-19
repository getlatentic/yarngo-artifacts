//! A `Send + Sync` handle over a backend that is neither.
//!
//! The sidecar owns pipes and a child process, so it cannot be shared across
//! threads. It also blocks for 15-25 seconds during synthesis, which would
//! freeze any UI that called it directly. Both problems have the same answer:
//! give the engine its own thread and talk to it over channels.
//!
//! Callers get blocking methods that are safe to invoke from a background task.

use std::path::Path;
use std::sync::mpsc::{channel, Sender};
use std::sync::Mutex;
use std::thread;

use crate::sidecar::MlxSidecar;
use crate::{
    Clip, DiskSpace, EngineError, InstallStatus, ModelSpec, Result, SpeechEngine, Synthesis,
    SynthesisRequest, SystemInfo, Voice,
};

enum Command {
    Models(bool, Sender<Result<Vec<ModelSpec>>>),
    RegisterVoice(Voice, Sender<Result<()>>),
    Voices(Sender<Result<Vec<Voice>>>),
    DeleteVoice(String, Sender<Result<()>>),
    PrepareVoice(String, Option<String>, Sender<Result<f32>>),
    DeleteModel(String, Sender<Result<u64>>),
    Synthesize(SynthesisRequest, Sender<Result<Synthesis>>),
    InstallModel(String, Sender<Result<InstallStatus>>),
    InstallStatus(String, Sender<Result<InstallStatus>>),
    DiskSpace(Sender<Result<DiskSpace>>),
    Clips(Sender<Result<Vec<Clip>>>),
    DeleteClip(String, Sender<Result<Vec<Clip>>>),
    RenameClip(String, String, Sender<Result<Vec<Clip>>>),
    SystemInfo(Sender<Result<SystemInfo>>),
}

/// Owns the engine thread. Dropping it shuts the engine down.
pub struct EngineHandle {
    tx: Mutex<Sender<Command>>,
}

impl EngineHandle {
    /// Start the engine thread. Returns once the backend has answered a ping,
    /// so a broken environment surfaces here rather than at first synthesis.
    pub fn spawn(python: &Path, script: &Path, work_dir: &Path) -> Result<Self> {
        let (tx, rx) = channel::<Command>();
        let (ready_tx, ready_rx) = channel::<Result<()>>();

        let (python, script, work_dir) =
            (python.to_path_buf(), script.to_path_buf(), work_dir.to_path_buf());

        thread::Builder::new()
            .name("speech-engine".into())
            .spawn(move || {
                let mut engine = match MlxSidecar::spawn(&python, &script, &work_dir) {
                    Ok(engine) => {
                        let _ = ready_tx.send(Ok(()));
                        engine
                    }
                    Err(err) => {
                        let _ = ready_tx.send(Err(err));
                        return;
                    }
                };

                // Each arm replies on the caller's channel; a disconnected
                // caller is not an error, it just means nobody is waiting.
                for command in rx {
                    match command {
                        Command::Models(refresh, reply) => {
                            let _ = reply.send(engine.models(refresh));
                        }
                        Command::RegisterVoice(voice, reply) => {
                            let _ = reply.send(engine.register_voice(&voice));
                        }
                        Command::Voices(reply) => {
                            let _ = reply.send(engine.voices());
                        }
                        Command::DeleteVoice(id, reply) => {
                            let _ = reply.send(engine.delete_voice(&id));
                        }
                        Command::PrepareVoice(id, model, reply) => {
                            let _ = reply.send(engine.prepare_voice(&id, model.as_deref()));
                        }
                        Command::DeleteModel(model, reply) => {
                            let _ = reply.send(engine.delete_model(&model));
                        }
                        Command::Synthesize(request, reply) => {
                            let _ = reply.send(engine.synthesize(&request));
                        }
                        Command::InstallModel(model, reply) => {
                            let _ = reply.send(engine.install_model(&model));
                        }
                        Command::InstallStatus(model, reply) => {
                            let _ = reply.send(engine.install_status(&model));
                        }
                        Command::DiskSpace(reply) => {
                            let _ = reply.send(engine.disk_space());
                        }
                        Command::Clips(reply) => {
                            let _ = reply.send(engine.clips());
                        }
                        Command::DeleteClip(id, reply) => {
                            let _ = reply.send(engine.delete_clip(&id));
                        }
                        Command::RenameClip(id, name, reply) => {
                            let _ = reply.send(engine.rename_clip(&id, &name));
                        }
                        Command::SystemInfo(reply) => {
                            let _ = reply.send(engine.system_info());
                        }
                    }
                }
            })
            .map_err(|e| EngineError::Transport(format!("could not start engine thread: {e}")))?;

        ready_rx
            .recv()
            .map_err(|_| EngineError::NotRunning)??;

        Ok(Self { tx: Mutex::new(tx) })
    }

    fn dispatch<T>(&self, make: impl FnOnce(Sender<Result<T>>) -> Command) -> Result<T> {
        let (reply_tx, reply_rx) = channel();
        self.tx
            .lock()
            .map_err(|_| EngineError::NotRunning)?
            .send(make(reply_tx))
            .map_err(|_| EngineError::NotRunning)?;
        reply_rx.recv().map_err(|_| EngineError::NotRunning)?
    }

    /// Remove a model's weights. Returns the bytes freed.
    pub fn delete_model(&self, model: impl Into<String>) -> Result<u64> {
        let id = model.into();
        self.dispatch(|reply| Command::DeleteModel(id, reply))
    }

    /// The catalogue as it stands on disk.
    pub fn models(&self) -> Result<Vec<ModelSpec>> {
        self.dispatch(|reply| Command::Models(false, reply))
    }

    /// The catalogue with download sizes refreshed from the hub. Needs network
    /// and takes a second or two, so it is a deliberate call, not the default.
    pub fn models_with_sizes(&self) -> Result<Vec<ModelSpec>> {
        self.dispatch(|reply| Command::Models(true, reply))
    }

    pub fn register_voice(&self, voice: Voice) -> Result<()> {
        self.dispatch(|reply| Command::RegisterVoice(voice, reply))
    }

    pub fn voices(&self) -> Result<Vec<Voice>> {
        self.dispatch(Command::Voices)
    }

    pub fn delete_voice(&self, voice_id: impl Into<String>) -> Result<()> {
        let id = voice_id.into();
        self.dispatch(|reply| Command::DeleteVoice(id, reply))
    }

    /// Warm a voice so the first generation does not pay for it. Blocks for
    /// roughly 40 seconds; call it from a background task.
    pub fn prepare_voice(&self, voice_id: impl Into<String>, model: Option<String>) -> Result<f32> {
        let id = voice_id.into();
        self.dispatch(|reply| Command::PrepareVoice(id, model, reply))
    }

    /// Blocks for the length of a generation. Call it from a background task.
    pub fn synthesize(&self, request: SynthesisRequest) -> Result<Synthesis> {
        self.dispatch(|reply| Command::Synthesize(request, reply))
    }

    pub fn install_model(&self, model: impl Into<String>) -> Result<InstallStatus> {
        let model = model.into();
        self.dispatch(|reply| Command::InstallModel(model, reply))
    }

    pub fn install_status(&self, model: impl Into<String>) -> Result<InstallStatus> {
        let model = model.into();
        self.dispatch(|reply| Command::InstallStatus(model, reply))
    }

    pub fn disk_space(&self) -> Result<DiskSpace> {
        self.dispatch(Command::DiskSpace)
    }

    pub fn clips(&self) -> Result<Vec<Clip>> {
        self.dispatch(Command::Clips)
    }

    pub fn delete_clip(&self, clip_id: impl Into<String>) -> Result<Vec<Clip>> {
        let id = clip_id.into();
        self.dispatch(|reply| Command::DeleteClip(id, reply))
    }

    pub fn rename_clip(
        &self,
        clip_id: impl Into<String>,
        name: impl Into<String>,
    ) -> Result<Vec<Clip>> {
        let (id, name) = (clip_id.into(), name.into());
        self.dispatch(|reply| Command::RenameClip(id, name, reply))
    }

    pub fn system_info(&self) -> Result<SystemInfo> {
        self.dispatch(Command::SystemInfo)
    }
}
