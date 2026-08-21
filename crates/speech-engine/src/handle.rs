//! A `Send + Sync` handle over a backend that is neither.
//!
//! The sidecar owns pipes and a child process, so it cannot be shared across
//! threads. It also takes minutes over a synthesis, which would freeze any UI
//! that called it directly. Both problems have the same answer: give the engine
//! its own thread and talk to it over channels.
//!
//! The thread submits work and does not wait for it. That distinction is the
//! point: the connection underneath carries request ids and can answer a ping
//! or a cancellation while a synthesis is still running, and a thread that sat
//! on the synthesis reply would throw that away one layer above the wire. So a
//! deferred operation is recorded here and its reply comes back as another
//! message on the same channel, taking its turn like anything else.
//!
//! Callers still get blocking methods, because a caller on a background task
//! wants an answer. What no longer blocks is the engine.

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{channel, Sender};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use crate::sidecar::MlxSidecar;
use crate::protocol::Outstanding;
use crate::{
    Clip, DiskSpace, EngineError, InstallStatus, ModelSpec, Result, SpeechEngine, Started,
    Synthesis, SynthesisRequest, SystemInfo, Voice,
};

/// Long enough for a cold model load and several minutes of speech. Not a
/// deadline anybody is waiting on — the caller has its own — but a point past
/// which an engine that has said nothing is not going to.
const GENEROUS: Duration = Duration::from_secs(1800);

/// Turn a reply into a message the engine thread will read in its turn.
///
/// A thread that does nothing but wait, so the one that matters does not. It
/// could be avoided by handing the connection a sender of this channel's type,
/// which would mean teaching the protocol layer what a `Command` is.
fn forward(sent: Outstanding, timeout: Duration, into: Sender<Command>) {
    thread::spawn(move || {
        let id = sent.id();
        let _ = into.send(Command::Replied(id, sent.wait(timeout)));
    });
}

/// Who is waiting for a reply that has not arrived.
enum Waiting {
    Synthesis(Sender<Result<Synthesis>>),
    Preparation(Sender<Result<f32>>),
}

enum Command {
    Models(bool, Sender<Result<Vec<ModelSpec>>>),
    RegisterVoice(Voice, Sender<Result<()>>),
    Voices(Sender<Result<Vec<Voice>>>),
    DeleteVoice(String, Sender<Result<()>>),
    RenameVoice(String, String, Sender<Result<Vec<Voice>>>),
    PrepareVoice(String, Option<String>, Sender<Result<f32>>),
    DeleteModel(String, Sender<Result<u64>>),
    Synthesize(SynthesisRequest, Sender<Result<Synthesis>>),
    InstallModel(String, Sender<Result<InstallStatus>>),
    InstallStatus(String, Sender<Result<InstallStatus>>),
    DiskSpace(Sender<Result<DiskSpace>>),
    Clips(Sender<Result<Vec<Clip>>>),
    DeleteClip(String, Sender<Result<Vec<Clip>>>),
    RenameClip(String, String, Sender<Result<Vec<Clip>>>),
    DuplicateClip(String, Sender<Result<Vec<Clip>>>),
    SystemInfo(Sender<Result<SystemInfo>>),
    Ping(Sender<Result<()>>),
    CancelGeneration(Sender<Result<()>>),
    Progress(Sender<Result<Option<crate::runtime::Generating>>>),
    /// A deferred operation's reply, put back on this channel by the thread
    /// that was waiting for it. Handled in turn, which is what lets everything
    /// sent in the meantime have been handled already.
    Replied(u64, Result<serde_json::Value>),
    /// The handle is gone. Needed because the thread holds a sender of its own
    /// for replies, so the channel does not close on its own.
    Stop,
}

/// Owns the engine thread. Dropping it shuts the engine down.
pub struct EngineHandle {
    tx: Mutex<Sender<Command>>,
}

impl EngineHandle {
    /// Start the engine thread. Returns once the backend has answered a ping,
    /// so a broken environment surfaces here rather than at first synthesis.
    pub fn spawn(python: &Path, script: &Path, work_dir: &Path) -> Result<Self> {
        let (python, script, work_dir) =
            (python.to_path_buf(), script.to_path_buf(), work_dir.to_path_buf());
        Self::spawn_backend(move || {
            Ok(Box::new(MlxSidecar::spawn(&python, &script, &work_dir)?) as Box<dyn SpeechEngine + Send>)
        })
    }

    /// The same thread and the same channel, over whichever backend the caller
    /// builds.
    ///
    /// Built on the engine thread rather than handed in, because a backend owns
    /// pipes and a child process and belongs to the one thread that will use it.
    pub fn spawn_backend(
        build: impl FnOnce() -> Result<Box<dyn SpeechEngine + Send>> + Send + 'static,
    ) -> Result<Self> {
        let (tx, rx) = channel::<Command>();
        let (ready_tx, ready_rx) = channel::<Result<()>>();
        let replies = tx.clone();

        thread::Builder::new()
            .name("speech-engine".into())
            .spawn(move || {
                let mut engine = match build() {
                    Ok(engine) => {
                        let _ = ready_tx.send(Ok(()));
                        engine
                    }
                    Err(err) => {
                        let _ = ready_tx.send(Err(err));
                        return;
                    }
                };
                let mut outstanding: HashMap<u64, Waiting> = HashMap::new();

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
                        Command::RenameVoice(id, label, reply) => {
                            let _ = reply.send(engine.rename_voice(&id, &label));
                        }
                        Command::PrepareVoice(id, model, reply) => {
                            match engine.start_preparation(&id, model.as_deref()) {
                                Ok(Started::Done(answer)) => {
                                    let _ = reply.send(answer);
                                }
                                Ok(Started::Awaiting(sent)) => {
                                    outstanding.insert(sent.id(), Waiting::Preparation(reply));
                                    forward(sent, GENEROUS, replies.clone());
                                }
                                Err(err) => {
                                    let _ = reply.send(Err(err));
                                }
                            }
                        }
                        Command::DeleteModel(model, reply) => {
                            let _ = reply.send(engine.delete_model(&model));
                        }
                        Command::Synthesize(request, reply) => {
                            match engine.start_synthesis(&request) {
                                Ok(Started::Done(answer)) => {
                                    let _ = reply.send(answer);
                                }
                                Ok(Started::Awaiting(sent)) => {
                                    outstanding.insert(sent.id(), Waiting::Synthesis(reply));
                                    forward(sent, GENEROUS, replies.clone());
                                }
                                Err(err) => {
                                    let _ = reply.send(Err(err));
                                }
                            }
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
                        Command::DuplicateClip(id, reply) => {
                            let _ = reply.send(engine.duplicate_clip(&id));
                        }
                        Command::SystemInfo(reply) => {
                            let _ = reply.send(engine.system_info());
                        }
                        Command::Ping(reply) => {
                            let _ = reply.send(engine.ping());
                        }
                        Command::CancelGeneration(reply) => {
                            let _ = reply.send(engine.cancel_generation());
                        }
                        Command::Progress(reply) => {
                            let _ = reply.send(Ok(engine.progress()));
                        }
                        Command::Replied(id, result) => match outstanding.remove(&id) {
                            Some(Waiting::Synthesis(reply)) => {
                                let _ = reply.send(engine.finish_synthesis(id, result));
                            }
                            Some(Waiting::Preparation(reply)) => {
                                let _ = reply.send(engine.finish_preparation(id, result));
                            }
                            // Nobody is waiting: the caller gave up, or this is
                            // a reply to something already settled another way.
                            None => {}
                        },
                        Command::Stop => break,
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

    pub fn rename_voice(
        &self,
        voice_id: impl Into<String>,
        label: impl Into<String>,
    ) -> Result<Vec<Voice>> {
        let (id, label) = (voice_id.into(), label.into());
        self.dispatch(|reply| Command::RenameVoice(id, label, reply))
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

    pub fn duplicate_clip(&self, clip_id: impl Into<String>) -> Result<Vec<Clip>> {
        let id = clip_id.into();
        self.dispatch(|reply| Command::DuplicateClip(id, reply))
    }

    /// Whether the engine is there. Answerable while it is working, which is
    /// the only reason to ask.
    pub fn ping(&self) -> Result<()> {
        self.dispatch(Command::Ping)
    }

    /// Ask the running generation to stop.
    pub fn cancel_generation(&self) -> Result<()> {
        self.dispatch(Command::CancelGeneration)
    }

    /// Where the running generation has got to.
    pub fn progress(&self) -> Option<crate::runtime::Generating> {
        self.dispatch(Command::Progress).ok().flatten()
    }

    pub fn system_info(&self) -> Result<SystemInfo> {
        self.dispatch(Command::SystemInfo)
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        // Said rather than inferred from a closed channel: the thread keeps a
        // sender so replies can come back to it, so the channel outlives this.
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(Command::Stop);
        }
    }
}
