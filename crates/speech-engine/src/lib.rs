//! Speech synthesis behind one interface, so the runtime stays swappable.
//!
//! v1 ships a single backend (MLX via a Python sidecar, Apple Silicon only), but
//! the model catalogue is user-visible and the trait admits other backends —
//! CrispASR for Windows and Linux, or a CPU-class model on low-spec machines —
//! without the application layer knowing which one answered.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub mod handle;
pub mod paths;
pub mod runtime;
pub mod sidecar;

pub use handle::EngineHandle;
pub use paths::EnginePaths;

/// A model the user can pick between. Only commercially licensed models belong
/// here; anything under non-commercial terms must never reach the catalogue.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelSpec {
    pub id: String,
    pub label: String,
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

/// A generated clip, kept until the user deletes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub id: String,
    /// Short enough for a sidebar row, taken from the words themselves.
    pub title: String,
    pub text: String,
    pub path: PathBuf,
    #[serde(default)]
    pub voice_id: Option<String>,
    pub model: String,
    pub audio_s: f32,
    pub gen_s: f32,
    /// The seed that produced this clip, so it can be reproduced exactly.
    #[serde(default)]
    pub seed: Option<u32>,
    pub created: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesisRequest {
    pub text: String,
    pub output: PathBuf,
    /// `None` uses the catalogue default.
    pub model: Option<String>,
    /// `None` synthesises without cloning.
    pub voice_id: Option<String>,
    /// Pin the take. `None` draws a fresh seed, which is what makes "generate
    /// again" give a different reading; passing the previous seed back is what
    /// makes it give the same one with different words.
    #[serde(default)]
    pub seed: Option<u32>,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Capabilities {
    pub cloning: bool,
    pub streaming: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("engine transport failed: {0}")]
    Transport(String),
    #[error("engine rejected the request: {0}")]
    Rejected(String),
    #[error("engine is not running")]
    NotRunning,
}

pub type Result<T> = std::result::Result<T, EngineError>;

/// One speech backend. Implementations are expected to keep models resident
/// between calls: loading dominates cost, and a desktop app should pay it once.
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

    /// Materialise a voice's conditioning ahead of time. Optional to call and
    /// safe to repeat; it only moves work earlier. Returns seconds spent.
    fn prepare_voice(&mut self, voice_id: &str, model: Option<&str>) -> Result<f32>;

    fn synthesize(&mut self, request: &SynthesisRequest) -> Result<Synthesis>;

    /// Begin downloading a model. Returns immediately; poll `install_status`.
    fn install_model(&mut self, model: &str) -> Result<InstallStatus>;

    fn install_status(&mut self, model: &str) -> Result<InstallStatus>;

    fn disk_space(&mut self) -> Result<DiskSpace>;

    fn clips(&mut self) -> Result<Vec<Clip>>;

    fn delete_clip(&mut self, clip_id: &str) -> Result<Vec<Clip>>;

    fn system_info(&mut self) -> Result<SystemInfo>;
}
