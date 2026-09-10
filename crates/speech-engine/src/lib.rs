//! Speech synthesis behind one interface, so the runtime stays swappable.
//!
//! v1 ships a single backend (MLX via a Python sidecar, Apple Silicon only), but
//! the model catalogue is user-visible and the trait admits other backends —
//! CrispASR for Windows and Linux, or a CPU-class model on low-spec machines —
//! without the application layer knowing which one answered.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub mod catalogue;
pub mod handle;
pub mod paths;
pub mod protocol;
pub mod published;
pub mod runtime;
pub mod runtimes;
pub mod trust;
pub mod unpack;

pub use handle::EngineHandle;

/// A model the user can pick between. Only commercially licensed models belong
/// here; anything under non-commercial terms must never reach the catalogue.
// Every field already carries a serde default, so the type has one; deriving
// it says so, and lets a test name only the fields it is about.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ModelSpec {
    pub id: String,
    /// What this app recommends it *for* — "Fast", "Best quality". A
    /// characterisation, not an identity: two of these are the same model at
    /// different precisions, and none of them is what the model is called.
    pub label: String,
    /// What it actually is — "dots.tts MF", "Step-Audio-EditX". The label is
    /// ours; this is the name a licence or a paper is under, and the only one
    /// that means anything to someone checking either.
    #[serde(default)]
    pub name: String,
    pub licence: String,
    /// Whether the weights are present locally. A model in the catalogue is
    /// offered whether or not it is installed; picking an absent one downloads.
    #[serde(default)]
    pub installed: bool,
    #[serde(default)]
    pub size_bytes: u64,
    /// What the download will cost, read from the hub rather than estimated.
    /// `0` means never looked up — shown as unknown, never as a guess.
    #[serde(default)]
    pub download_bytes: u64,
    /// Quantisation, derived from the checkpoint variant rather than described.
    #[serde(default)]
    pub precision: String,
    /// Mean real-time factor from this user's own clips, if any exist. `None`
    /// means never used here — better than a number measured on another machine.
    #[serde(default)]
    pub measured_rtf: Option<f32>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub supports_cloning: bool,
    /// Held in memory right now, so switching to it costs nothing.
    #[serde(default)]
    pub resident: bool,
    /// Last measured seconds to load on this machine. `None` means never
    /// loaded here — the switcher says so rather than estimating.
    #[serde(default)]
    pub load_s: Option<f32>,
}

/// A reference voice, registered once so its cost is not paid per generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Voice {
    pub voice_id: String,
    pub label: String,
    pub reference_audio: PathBuf,
    #[serde(default)]
    pub reference_text: String,
    /// Length of the reference recording. `0.0` means a voice registered before
    /// the field existed — shown as unknown rather than as zero seconds.
    #[serde(default)]
    pub seconds: f32,
    /// What the user agreed to, and under which build. Written to an
    /// append-only log at registration; a voice cannot be made without it.
    #[serde(default)]
    pub consent: Consent,
    /// Measured when the take was accepted. Absent for voices enrolled before
    /// the measurement existed — unknown, not zero.
    #[serde(default)]
    pub snr_db: Option<f32>,
    #[serde(default)]
    pub sample_rate_hz: Option<u32>,
}

/// The claim that permitted a voice to exist.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Consent {
    /// The exact wording agreed to, stored rather than referenced, so a later
    /// change to the app's copy cannot rewrite what was agreed.
    pub statement: String,
    pub app_version: String,
    /// `recording` or `imported` — an imported voice may not be the user's own.
    pub source: String,
}

/// One reading of a clip. Generating again adds a take rather than a second
/// clip: the words are the same, only the reading differs, so they belong
/// together under one name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Take {
    pub id: String,
    pub path: PathBuf,
    pub audio_s: f32,
    pub gen_s: f32,
    /// The seed that produced it, so it can be reproduced exactly.
    #[serde(default)]
    pub seed: Option<u32>,
    pub created: String,
}

impl Take {
    /// Inference seconds per audio second. Lower is better; 1.0 is real time.
    pub fn rtf(&self) -> Option<f32> {
        (self.audio_s > 0.0).then(|| self.gen_s / self.audio_s)
    }
}

/// Where a generation has got to, as the engine last reported it.
///
/// Sent while it works rather than written to a file for someone to poll: the
/// engine can speak while it is speaking, and the file was only ever there
/// because an older one could not.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Generating {
    // Defaulted rather than required: a report that arrives without one of
    // these is still a report, and refusing to read it turns a partial answer
    // into no answer at all — which looks exactly like nothing running.
    #[serde(default)]
    pub written_s: f32,
    #[serde(default)]
    pub elapsed_s: f32,
    #[serde(default)]
    pub chunks_done: u32,
    #[serde(default)]
    pub chunks: u32,
}

/// A generated clip, kept until the user deletes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub id: String,
    /// Short enough for a sidebar row, taken from the words themselves.
    pub title: String,
    /// What the user calls it. Seeded from the first words and theirs to
    /// change; renaming leaves the audio and the text alone.
    #[serde(default)]
    pub name: String,
    pub text: String,
    #[serde(default)]
    pub voice_id: Option<String>,
    pub model: String,
    pub created: String,
    /// Newest first. Never empty — a clip whose audio has gone is not listed.
    #[serde(default)]
    pub takes: Vec<Take>,
}

impl Clip {
    /// The most recent reading, which is what the workspace opens on.
    pub fn latest(&self) -> Option<&Take> {
        self.takes.first()
    }

    pub fn take(&self, id: &str) -> Option<&Take> {
        self.takes.iter().find(|t| t.id == id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesisRequest {
    pub text: String,
    pub output: PathBuf,
    /// `None` uses the catalogue default.
    pub model: Option<String>,
    /// The clip this reading belongs to. `None` starts a new one; naming an
    /// existing clip adds a take to it.
    #[serde(default)]
    pub clip_id: Option<String>,
    /// `None` synthesises without cloning.
    pub voice_id: Option<String>,
    /// Pin the take. `None` draws a fresh seed, which is what makes "generate
    /// again" give a different reading; passing the previous seed back is what
    /// makes it give the same one with different words.
    #[serde(default)]
    pub seed: Option<u32>,
    /// What to call the clip. `None` lets the engine name it from the first
    /// words, which is what an unnamed draft wants.
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Synthesis {
    /// Present unless the caller asked for a throwaway generation.
    #[serde(default)]
    pub clip: Option<Clip>,
    pub output: PathBuf,
    pub model: String,
    pub audio_s: f32,
    pub gen_s: f32,
    /// Inference seconds per audio second. Lower is better; 1.0 is real time.
    pub rtf: Option<f32>,
    #[serde(default)]
    pub seed: Option<u32>,
    pub sample_rate: u32,
}

/// What a backend can actually do, so the UI offers only what is available.
/// Progress of a model download.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallStatus {
    pub model: String,
    /// `absent`, `downloading`, `installed` or `failed`.
    pub state: String,
    #[serde(default)]
    pub downloaded_bytes: u64,
    #[serde(default)]
    pub total_bytes: u64,
    #[serde(default)]
    pub error: Option<String>,
}

impl InstallStatus {
    pub fn fraction(&self) -> f32 {
        if self.total_bytes == 0 {
            return 0.0;
        }
        (self.downloaded_bytes as f32 / self.total_bytes as f32).clamp(0.0, 1.0)
    }

    pub fn is_downloading(&self) -> bool {
        self.state == "downloading"
    }
}

/// Free space, so a multi-gigabyte download can be refused before it starts.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct DiskSpace {
    pub free_bytes: u64,
    pub total_bytes: u64,
}

/// What this machine is, read from it rather than assumed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    pub os: String,
    pub chip: String,
    pub memory_bytes: u64,
    pub free_bytes: u64,
    pub data_dir: String,
}

/// What a runtime said it can do, at the handshake.
///
/// Asked once and kept, because the answer cannot change while the process
/// lives — and acted on, which is the difference between a protocol that
/// advertises capabilities and one that negotiates them. A feature the runtime
/// did not claim is not offered, rather than offered and failing at the call.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Capabilities {
    pub protocol: String,
    pub version: u32,
    /// Which inference stack is behind it. For saying so, not for branching on:
    /// what a runtime can do is the method list, not its name.
    pub backend: String,
    /// Whether forgetting one voice's conditioning forgets every voice's.
    /// Stated at the handshake rather than discovered when a deletion reports
    /// it, because a deletion cannot ask afterwards.
    pub conditioning_eviction: String,
    methods: std::collections::BTreeSet<String>,
}

impl Capabilities {
    /// What the other end said at the handshake.
    ///
    /// Absent fields are absent capabilities, not defaults: a runtime that did
    /// not say it can do something is one this application will not ask.
    pub fn from_handshake(reply: &serde_json::Value) -> Self {
        Self {
            protocol: reply["protocol"].as_str().unwrap_or_default().to_string(),
            version: reply["version"].as_u64().unwrap_or_default() as u32,
            backend: reply["backend"].as_str().unwrap_or_default().to_string(),
            conditioning_eviction: reply["conditioning_eviction"]
                .as_str()
                .unwrap_or("unknown")
                .to_string(),
            methods: reply["methods"]
                .as_array()
                .map(|names| {
                    names.iter().filter_map(|n| n.as_str().map(str::to_string)).collect()
                })
                .unwrap_or_default(),
        }
    }

    pub fn can(&self, method: &str) -> bool {
        self.methods.contains(method)
    }

    pub fn methods(&self) -> impl Iterator<Item = &str> {
        self.methods.iter().map(String::as_str)
    }

    /// Whether it can speak in a person's own voice, which needs both halves:
    /// something to turn a recording into conditioning, and a synthesis that
    /// accepts a reference.
    pub fn cloning(&self) -> bool {
        self.can("conditioning.prepare") && self.can("synthesis.generate")
    }

    /// Whether a voice can be enrolled. A runtime that cannot prepare a
    /// recording has no way to take one, so the offer is withdrawn rather than
    /// failing after somebody has spoken into a microphone.
    pub fn enrolment(&self) -> bool {
        self.can("audio.prepare_reference") && self.cloning()
    }

    /// Whether a generation can be stopped once it has started.
    pub fn cancellation(&self) -> bool {
        self.can("job.cancel")
    }

    /// Whether models can be installed and removed from inside the application.
    pub fn model_management(&self) -> bool {
        self.can("model.install") && self.can("model.delete")
    }

    /// Whether deleting a voice can be made to take effect in memory, which is
    /// what the privacy claim rests on. Without it a deletion has to end the
    /// process instead.
    pub fn conditioning_invalidation(&self) -> bool {
        self.can("conditioning.invalidate")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("engine transport failed: {0}")]
    Transport(String),
    #[error("engine rejected the request: {0}")]
    Rejected(String),
    #[error("engine is not running")]
    NotRunning,
    /// The work ran and the application would not keep what it produced.
    ///
    /// Not a fault of the engine, the transport, or the request. A generation
    /// refused because the voice was deleted while it ran did exactly what it
    /// was told to, and calling it a failure would tell the person their own
    /// deletion broke something. The message is the reason, unadorned, because
    /// it is a sentence for them and not a diagnostic.
    #[error("{0}")]
    Refused(String),
}

pub type Result<T> = std::result::Result<T, EngineError>;

/// One speech backend. Implementations are expected to keep models resident
/// between calls: loading dominates cost, and a desktop app should pay it once.
/// What became of an operation that was begun.
///
/// Refused before it started, or handed over with the reply still to come. The
/// second is the ordinary case: the engine has the work and this thread is free
/// for whatever is asked next.
pub enum Started<T> {
    /// Answered outright, without the engine being asked.
    Done(Result<T>),
    /// The engine has it. The reply finishes it.
    Awaiting(protocol::Outstanding),
}

pub trait SpeechEngine {
    fn capabilities(&self) -> Capabilities;

    /// Models this backend can offer the user right now.
    /// Remove a model's weights and report the bytes freed.
    fn delete_model(&mut self, model: &str) -> Result<u64>;

    /// `refresh` permits one online look-up of download sizes. Off by default
    /// so the catalogue stays readable with no network.
    fn models(&mut self, refresh: bool) -> Result<Vec<ModelSpec>>;

    /// Register a reference voice. Doing this once at enrolment rather than per
    /// request is the difference between roughly 43s and 15s for a short note.
    fn register_voice(&mut self, voice: &Voice) -> Result<()>;

    fn voices(&mut self) -> Result<Vec<Voice>>;

    fn delete_voice(&mut self, voice_id: &str) -> Result<()>;

    /// Change what a voice is called. The recording and the consent it was
    /// given under are untouched, and so are the clips already made with it.
    fn rename_voice(&mut self, voice_id: &str, label: &str) -> Result<Vec<Voice>>;

    /// Begin a synthesis, and return as soon as the engine has it.
    ///
    /// Not a blocking call, because the thread that owns the backend has to
    /// stay free: a cancellation arriving while this runs is the whole reason
    /// the connection carries request ids.
    fn start_synthesis(&mut self, request: &SynthesisRequest) -> Result<Started<Synthesis>>;

    /// Finish one. `id` is the request whose reply arrived.
    fn finish_synthesis(&mut self, id: u64, reply: Result<serde_json::Value>) -> Result<Synthesis>;

    /// The same, for conditioning, which is the other operation long enough
    /// that everything behind it would be a fault.
    fn start_preparation(&mut self, voice_id: &str, model: Option<&str>) -> Result<Started<f32>>;

    fn finish_preparation(&mut self, id: u64, reply: Result<serde_json::Value>) -> Result<f32>;

    /// Begin downloading a model. Returns immediately; poll `install_status`.
    fn install_model(&mut self, model: &str) -> Result<InstallStatus>;

    fn install_status(&mut self, model: &str) -> Result<InstallStatus>;

    fn disk_space(&mut self) -> Result<DiskSpace>;

    fn clips(&mut self) -> Result<Vec<Clip>>;

    fn delete_clip(&mut self, clip_id: &str) -> Result<Vec<Clip>>;

    fn rename_clip(&mut self, clip_id: &str, name: &str) -> Result<Vec<Clip>>;

    fn duplicate_clip(&mut self, clip_id: &str) -> Result<Vec<Clip>>;

    fn system_info(&mut self) -> Result<SystemInfo>;

    /// Whether the engine is there, asked in a way it can answer while it is
    /// working.
    fn ping(&mut self) -> Result<()>;

    /// Ask the running generation to stop.
    ///
    /// Cooperative: it stops at the next point where stopping leaves something
    /// whole. Whether it stopped is reported by how the generation ends, not by
    /// this returning.
    fn cancel_generation(&mut self) -> Result<()>;

    /// A moment in which nothing was asked of the engine.
    ///
    /// A backend with a deadline of its own — work it asked to stop and which
    /// has not stopped — acts on it here. Called by the thread that owns the
    /// backend, which is the only place something can safely be done about it:
    /// the reply that would settle the work arrives on that same thread, so a
    /// deadline enforced by waiting would be a deadline that never fires.
    fn attend(&mut self) {}

    /// What the running generation has produced so far, if one is running.
    ///
    /// On the engine rather than read from a fixed file, because how it is
    /// known depends on the backend: one writes it to disk because it cannot
    /// speak while it works, and the other sends it.
    fn progress(&mut self) -> Option<crate::Generating>;
}
