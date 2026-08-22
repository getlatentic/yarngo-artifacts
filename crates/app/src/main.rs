//! Voicestudio — type text, choose a voice, hear it in that voice.
//!
//! Generation takes 15-25 seconds, so it runs on a background executor and the
//! window shows an honest progress state rather than a spinner that implies
//! something faster. Everything below `EngineHandle` is backend-agnostic.

mod about;
mod clips;
mod enrolment;
mod inspector;
mod models;
mod settings;
mod storage;
mod switcher;
mod icon;
mod ui;
mod player;
mod recorder;
mod theme;
mod workspace;
mod reveal;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{
    input::{InputState, TextareaState},
    ActiveTheme, Root, StyledExt,
};
use rust_i18n::t;

use player::{format_time, AudioPlayer};
use recorder::{Quality, Recorder, ENROLMENT_SCRIPT};
use speech_engine::{
    runtime, Clip, EngineHandle, EnginePaths, InstallStatus, ModelSpec, Synthesis,
    SynthesisRequest, SystemInfo, Voice,
};

/// Which engine the application runs on, chosen at start.
///
/// The legacy sidecar keeps the clips and voices in JSON files of its own; the
/// durable one keeps them in this application's database and uses the sidecar
/// only to make audio. The second is where this is going and the first is what
/// has been running, so the switch stays until the new path has been lived with
/// and the old one can be deleted rather than kept as an option.
///
///     YARNGO_ENGINE_PROTOCOL=jsonrpc cargo run
fn start_engine(paths: &EnginePaths) -> Result<EngineHandle, speech_engine::EngineError> {
    let durable = std::env::var("YARNGO_ENGINE_PROTOCOL")
        .map(|value| value.eq_ignore_ascii_case("jsonrpc"))
        .unwrap_or(false);
    if !durable {
        return EngineHandle::spawn(&paths.python, &paths.script, &paths.work_dir);
    }
    let data_dir = speech_engine::paths::data_dir();
    let spawn = yarngo_synthesis::engine::Spawn {
        python: paths.python.clone(),
        script: paths.script.clone(),
        work_dir: paths.work_dir.clone(),
        data_dir: data_dir.clone(),
    };
    let database = data_dir.join("yarngo.db");
    EngineHandle::spawn_backend(move || {
        Ok(Box::new(yarngo_synthesis::engine::DurableEngine::open(
            &database, &data_dir, spawn,
        )?))
    })
}

rust_i18n::i18n!("locales", fallback = "en");

actions!(voicestudio, [Speak, CommitRename, CancelRename]);

/// Key context for the inline name field, so Enter and Escape mean rename only
/// while a name is open for editing.
pub(crate) const RENAME_CONTEXT: &str = "Rename";

const APP_ROOT: &str = "/Users/dev/workspace/voicestudio";

/// Which full-window screen is showing. Exclusive by construction: the previous
/// version derived three independent booleans in `render`, which meant "setting
/// up" and "enrolling" could both be true and the winner was decided by the
/// order the `when` clauses happened to run in.
///
/// This is deliberately not Zed's model. Zed composes non-exclusive views —
/// panes, docks and a modal layer stacked over a workspace — because an editor
/// shows several things at once. Here the screens genuinely replace one
/// another, so a state machine says more than a pane tree would.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// The speech runtime is not installed yet.
    Setup,
    /// Which model to put on disk. One row per model, with the licence and the
    /// download size where the name is, because both are a commitment.
    Models,
    /// The user asked to add a reference voice.
    Enrolment,
    /// Voices and clips on the left, composer on the right.
    Workspace,
}

/// Where the user is in voice enrolment. A closed set, so states like
/// "recording and reviewing at once" cannot be constructed.
#[derive(Clone, Debug)]
pub enum Enrolment {
    /// Not enrolling.
    Closed,
    /// Script shown, waiting for the user to start.
    Ready,
    Recording,
    /// Recorded and passed the quality bar; awaiting consent.
    Review(Quality),
    /// Recorded but rejected, with advice on what to fix.
    Rejected(String),
}

/// What the user is currently waiting on, if anything.
#[derive(Clone, Debug)]
pub enum Status {
    Idle,
    Preparing(String),
    /// Installing the speech runtime on first run.
    Installing { step: String, fraction: f32 },
    Generating,
    Done { output: PathBuf, audio_s: f32, gen_s: f32 },
    Failed(String),
    /// Nothing went wrong and it did not happen — the words are still there and
    /// the person is not being told their own decision was a fault.
    Refused(String),
}

/// A finished reference recording, held between stopping and saving: where it
/// landed on disk, how long it runs, and its shape for the review waveform.
/// Written out at stop rather than at save, because review has to play it.
pub struct Take {
    pub path: PathBuf,
    pub seconds: f32,
    pub levels: Vec<f32>,
}

/// Bars in the review waveform, as the design draws it.
const TAKE_BARS: usize = 32;
/// Bars in the workspace player's waveform.
const CLIP_BARS: usize = 40;

pub struct VoiceStudio {
    pub(crate) engine: Option<Arc<EngineHandle>>,
    pub(crate) models: Vec<ModelSpec>,
    pub(crate) selected_model: Option<String>,
    pub(crate) voices: Vec<Voice>,
    pub(crate) selected_voice: Option<String>,
    pub(crate) text: Entity<TextareaState>,
    pub(crate) status: Status,
    player: Option<AudioPlayer>,
    pub(crate) recorder: Option<Recorder>,
    pub(crate) enrolment: Enrolment,
    /// Consent is explicit and must be given per enrolment, never remembered.
    pub(crate) consent_given: bool,
    /// Download progress, keyed by model id, for models being installed.
    pub(crate) installs: std::collections::HashMap<String, InstallStatus>,
    pub(crate) clips: Vec<Clip>,
    system: Option<SystemInfo>,
    /// A voice being warmed in the background, if any. Deliberately not a
    /// `Status`: warming must not disable Generate, only explain a slower
    /// first clip if one is asked for before it finishes.
    pub(crate) warming: Option<String>,
    /// The settings window, and which of its panes is showing.
    pub(crate) settings_open: bool,
    pub(crate) settings_pane: settings::Pane,
    /// The seek track's painted rectangle, so a click can be turned into a
    /// position along the clip.
    track: std::rc::Rc<std::cell::Cell<Bounds<Pixels>>>,
    /// The clip text's scroll position and viewport, read back so the view can
    /// tell where its bottom edge falls between two lines.
    pub(crate) text_scroll: ScrollHandle,
    /// What the running generation has written so far, polled from the engine
    /// while it works. `None` between generations.
    pub(crate) progress: Option<runtime::Generating>,
    /// A stop has been asked for and the generation has not ended yet.
    ///
    /// Worth showing, because asking is not stopping: the engine stops where a
    /// sentence ends, so one already speaking its last part finishes it. Left
    /// unsaid, the button looks broken for as long as that takes.
    pub(crate) stopping: bool,
    /// Seconds of speech this generation is expected to produce, from the word
    /// count. An estimate, and labelled as one — the exact length is not known
    /// until the model has finished.
    pub(crate) expected_s: f32,
    /// The row a running generation belongs to — a draft id, or the id of the
    /// clip that "Generate again" was pressed on. Needed because both are
    /// possible and only one of them is a draft, so a draft flag alone cannot
    /// answer "is the thing on screen the thing that is running".
    pub(crate) generating_row: Option<String>,
    /// What the voice being enrolled will be called. A field rather than a
    /// generated name: with more than one voice, "My voice 3" tells you
    /// nothing, and the sharing case makes several the normal state.
    pub(crate) voice_name: Entity<InputState>,
    /// A reading of the script brought in from a file rather than recorded
    /// here. Held separately because the take is already on disk: it must be
    /// registered from where it is, not written out of the recorder's buffer.
    pub(crate) imported: Option<PathBuf>,
    /// Text the user asked to speak before a model was ready. Held rather than
    /// refused: the download finishes on its own, so nothing has to be retyped.
    pub(crate) queued: Option<String>,
    /// A voice the user asked to delete, held until they confirm.
    pub(crate) confirming_voice: Option<String>,
    /// A model the user asked to delete, held until they confirm. Deleting
    /// gigabytes is not something to do on a single click.
    pub(crate) confirming_delete: Option<String>,
    /// The switcher hanging off the title-bar pill. Separate from
    /// `choosing_model`: the menu is a quick change, the full screen is where
    /// downloads and licences are read.
    pub(crate) model_menu: bool,
    /// The user opened the model list from the title bar. Distinct from the
    /// first-run case, which is derived from there being no model on disk.
    pub(crate) choosing_model: bool,
    /// The generation just finished, for the metadata line under the player.
    pub(crate) last: Option<Synthesis>,
    /// The clip currently loaded in the player, if any.
    clip: Option<(PathBuf, f32)>,
    /// The reference recording awaiting review, if any.
    pub(crate) take: Option<Take>,
    /// The loaded clip's shape, for the player's waveform.
    clip_levels: Vec<f32>,
    /// Whether the recorder was opened as the last step of first-run setup.
    /// Setup gets the full screen because there is no workspace behind it yet;
    /// every later visit is a sheet over the work in progress, including the
    /// first voice added from an empty one.
    pub(crate) in_setup: bool,
    /// Clips being written. A draft is a clip that has no audio yet; it sits
    /// in the same list, so starting another one has somewhere to put the one
    /// you were on.
    pub(crate) drafts: Vec<clips::Draft>,
    /// What the workspace is pointed at. Never empty — first run opens on a
    /// blank draft, because the composer is never showing nothing.
    pub(crate) selected: clips::Selected,
    /// Counter behind draft ids, so two drafts never collide.
    pub(crate) next_draft: usize,
    /// The name being edited inline in the header, and what it belongs to.
    pub(crate) clip_name: Entity<InputState>,
    pub(crate) renaming: Option<clips::Selected>,
    /// The voice whose name is being edited in the settings window, if any.
    /// Separate from `renaming`: that one names a clip, and both windows can be
    /// on screen at once.
    pub(crate) renaming_voice: Option<String>,
    pub(crate) voice_rename: Entity<InputState>,
    /// The runtime finished installing and the user has not moved on yet.
    /// Setup holds the screen until they do: a download they watched for
    /// minutes should end by saying so, not by vanishing.
    pub(crate) runtime_done: bool,
    /// What the app has put on disk, once it has been counted.
    pub(crate) storage: Option<storage::Usage>,
    /// A clip row's menu, and where on screen it was opened from. Anchored to
    /// the click rather than to the row, because the list scrolls under it.
    pub(crate) clip_menu: Option<(String, Point<Pixels>)>,
    /// Whether the clip inspector is showing. Closed by default: the two
    /// settings it holds are stated as chips in the composer header, and the
    /// workspace keeps the width the design gives it.
    pub(crate) inspector: bool,
    /// A voice saved from the inspector, named until the next thing happens —
    /// the panel says so where the change was made.
    pub(crate) voice_saved: Option<String>,
    /// A seed held for the next generation. Pinning is what turns "generate
    /// again" from a different reading into the same reading of new words.
    pub(crate) pinned_seed: Option<u32>,
}

impl VoiceStudio {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let text = cx.new(|cx| {
            TextareaState::new(window, cx).placeholder(t!("compose.placeholder").to_string())
        });

        let voice_name = cx.new(|cx| {
            InputState::new(window, cx).default_value(t!("voice.mine").to_string())
        });
        let clip_name = clips::name_field(window, cx);

        let mut this = Self {
            engine: None,
            models: Vec::new(),
            selected_model: None,
            voices: Vec::new(),
            selected_voice: None,
            text,
            status: Status::Preparing("Starting the speech engine…".into()),
            // A missing output device must not stop the app from generating.
            player: AudioPlayer::new().ok(),
            recorder: None,
            enrolment: Enrolment::Closed,
            consent_given: false,
            installs: std::collections::HashMap::new(),
            clips: Vec::new(),
            system: None,
            warming: None,
            settings_open: false,
            settings_pane: settings::Pane::Models,
            voice_name,
            track: std::rc::Rc::new(std::cell::Cell::new(Bounds::default())),
            text_scroll: ScrollHandle::new(),
            progress: None,
            expected_s: 0.0,
            generating_row: None,
            stopping: false,
            imported: None,
            queued: None,
            confirming_voice: None,
            confirming_delete: None,
            model_menu: false,
            choosing_model: false,
            last: None,
            clip: None,
            take: None,
            clip_levels: Vec::new(),
            pinned_seed: None,
            inspector: false,
            clip_menu: None,
            storage: None,
            runtime_done: false,
            drafts: vec![],
            selected: clips::Selected::Draft("draft-1".into()),
            next_draft: 1,
            clip_name,
            renaming: None,
            renaming_voice: None,
            voice_rename: cx.new(|cx| InputState::new(window, cx)),
            in_setup: false,
            voice_saved: None,
        };
        // First run opens on a blank draft: the composer always has a clip.
        this.drafts.push(clips::Draft::blank());
        this.start_engine(cx);
        this
    }

    /// Engine startup loads models from disk, so it happens off the main thread.
    fn start_engine(&mut self, cx: &mut Context<Self>) {
        // A first run has no runtime. Offer to install it rather than failing.
        if !runtime::is_installed() {
            let paths = EnginePaths::resolve(std::path::Path::new(APP_ROOT));
            if paths.missing().is_some() {
                self.status = Status::Installing {
                    step: "The speech engine is not installed yet.".into(),
                    fraction: 0.0,
                };
                cx.notify();
                return;
            }
        }

        let paths = EnginePaths::resolve(std::path::Path::new(APP_ROOT));
        if let Some(missing) = paths.missing() {
            self.status = Status::Failed(missing);
            cx.notify();
            return;
        }
        cx.spawn(async move |this, cx| {
            let started = cx
                .background_spawn(async move {
                    let handle = start_engine(&paths)?;
                    let models = handle.models()?;
                    // Voices outlive the process — the sidecar keeps them on
                    // disk precisely so a 40 second preparation is paid once.
                    let voices = handle.voices()?;
                    Ok::<_, speech_engine::EngineError>((Arc::new(handle), models, voices))
                })
                .await;

            this.update(cx, |this, cx| {
                match started {
                    Ok((handle, models, voices)) => {
                        this.selected_model = models
                            .iter()
                            .find(|m| m.default)
                            .or_else(|| models.first())
                            .map(|m| m.id.clone());
                        this.models = models;
                        // The bundled voice is the selection out of the box:
                        // it works with every model and needs nothing recorded,
                        // so nothing has to happen before the first clip.
                        this.voices = voices;
                        this.engine = Some(handle);
                        this.status = Status::Idle;
                        this.refresh_clips(cx);
                        this.refresh_system(cx);
                    }
                    Err(err) => this.status = Status::Failed(format!("{err}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Build the selected voice's conditioning now, so the first clip of the
    /// session is not the slow one. Safe to call repeatedly; the sidecar keeps
    /// the result and a second call is a no-op beyond the round trip.
    pub(crate) fn warm_selected_voice(&mut self, cx: &mut Context<Self>) {
        let (Some(engine), Some(voice)) = (self.engine.clone(), self.speaking_voice().map(str::to_owned))
        else {
            return;
        };
        if self.warming.as_deref() == Some(voice.as_str()) {
            return;
        }
        self.warming = Some(voice.clone());
        cx.notify();

        let model = self.selected_model.clone();
        let warmed = voice.clone();
        cx.spawn(async move |this, cx| {
            let result =
                cx.background_spawn(async move { engine.prepare_voice(voice, model) }).await;
            this.update(cx, |this, cx| {
                if this.warming.as_deref() == Some(warmed.as_str()) {
                    this.warming = None;
                }
                // A failed warm is not worth interrupting anyone over: the
                // generation that needs it will do the work and report for real.
                if let Err(err) = result {
                    eprintln!("could not warm {warmed}: {err}");
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Bring in a reading of the script made elsewhere — an earlier install, a
    /// different machine, or the person whose voice it is sending you theirs.
    ///
    /// This works only because the script is fixed: the words are known without
    /// transcribing the file, which is the same property that lets the recorder
    /// skip speech recognition. What cannot be known is whether the file really
    /// is that script, so the panel says so and the checks below are the same
    /// ones a recording has to pass.
    pub(crate) fn import_reference(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Use this recording".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else { return };
            let Some(path) = paths.into_iter().next() else { return };

            let checked = {
                let path = path.clone();
                cx.background_spawn(async move {
                    let (samples, rate) = recorder::load_audio(&path)?;
                    let quality = recorder::assess(&samples, rate)?;
                    Ok::<_, String>((quality, recorder::envelope(&samples, TAKE_BARS)))
                })
                .await
            };

            this.update(cx, |this, cx| {
                match checked {
                    Ok((quality, levels)) => {
                        this.take = Some(Take {
                            path: path.clone(),
                            seconds: quality.seconds,
                            levels,
                        });
                        this.imported = Some(path);
                        this.consent_given = false;
                        this.enrolment = Enrolment::Review(quality);
                    }
                    Err(reason) => {
                        this.imported = None;
                        this.clear_take();
                        this.enrolment = Enrolment::Rejected(reason);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Open the recorder and suggest a name for what it will produce.
    pub(crate) fn begin_enrolment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.in_setup = false;
        let suggested = self.next_voice_name();
        self.voice_name.update(cx, |state, cx| state.set_value(suggested, window, cx));
        self.open_recorder(cx);
    }

    /// The recorder without the name suggestion, for the first run — where the
    /// field was already seeded at construction and there is no window to hand.
    fn open_recorder(&mut self, cx: &mut Context<Self>) {
        self.consent_given = false;
        self.imported = None;
        self.clear_take();
        match Recorder::new() {
            Ok(rec) => {
                self.recorder = Some(rec);
                self.enrolment = Enrolment::Ready;
            }
            Err(err) => self.enrolment = Enrolment::Rejected(err),
        }
        cx.notify();
    }

    /// Leave setup without recording. The bundled voice is already selected and
    /// works with every model, so the last step is an offer — the workspace says
    /// so, and making it a toll would contradict that before anyone gets there.
    pub(crate) fn finish_setup(&mut self, cx: &mut Context<Self>) {
        self.in_setup = false;
        self.cancel_enrolment(cx);
    }

    pub(crate) fn cancel_enrolment(&mut self, cx: &mut Context<Self>) {
        if let Some(rec) = self.recorder.as_mut() {
            rec.stop();
        }
        self.recorder = None;
        self.enrolment = Enrolment::Closed;
        self.in_setup = false;
        self.consent_given = false;
        self.imported = None;
        self.clear_take();
        cx.notify();
    }

    pub(crate) fn start_recording(&mut self, cx: &mut Context<Self>) {
        let Some(rec) = self.recorder.as_mut() else { return };
        match rec.start() {
            Ok(()) => {
                self.enrolment = Enrolment::Recording;
                self.tick_recording(cx);
            }
            // Most often macOS microphone permission, so say so plainly.
            Err(err) => self.enrolment = Enrolment::Rejected(err),
        }
        cx.notify();
    }

    /// Repaint while recording so the level meter and timer move.
    fn tick_recording(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let recording = this
                .update(cx, |this, cx| {
                    cx.notify();
                    matches!(this.enrolment, Enrolment::Recording)
                })
                .unwrap_or(false);
            if !recording {
                break;
            }
        })
        .detach();
    }

    /// Stop, and settle the take before review can ask anything of it: the
    /// samples are written to disk so review has something to play, and reduced
    /// to an envelope so it has something to draw.
    pub(crate) fn stop_recording(&mut self, cx: &mut Context<Self>) {
        let Some(rec) = self.recorder.as_mut() else { return };
        rec.stop();
        self.enrolment = match rec.check_quality() {
            Ok(quality) => {
                let path = std::env::temp_dir().join("voicestudio_enrolment.wav");
                match rec.write_wav(&path) {
                    Ok(()) => {
                        self.take = Some(Take {
                            path,
                            seconds: quality.seconds,
                            levels: rec.envelope(TAKE_BARS),
                        });
                        Enrolment::Review(quality)
                    }
                    Err(err) => Enrolment::Rejected(err),
                }
            }
            Err(reason) => Enrolment::Rejected(reason),
        };
        cx.notify();
    }

    /// Throw the take away and go back to the script. A fresh recorder rather
    /// than a reset one: the buffer is the take, and reusing it would append
    /// the second reading to the first.
    pub(crate) fn discard_take(&mut self, cx: &mut Context<Self>) {
        if let Some(rec) = self.recorder.as_mut() {
            rec.stop();
        }
        self.open_recorder(cx);
    }

    /// Consent is checked here rather than trusted from the UI, so the recording
    /// cannot become a voice profile without it.
    pub(crate) fn confirm_enrolment(&mut self, cx: &mut Context<Self>) {
        if !self.consent_given {
            return;
        }
        let Some(engine) = self.engine.clone() else { return };

        // Written when the take was stopped, so review could play it; a file
        // brought in from disk is registered from where it already is.
        let Some(path) = self.take.as_ref().map(|t| t.path.clone()) else { return };

        // A stable id per voice, so enrolling twice adds a second voice rather
        // than overwriting the first.
        let ordinal = self.voices.len() + 1;
        let voice_id = format!("voice-{ordinal}");
        let typed = self.voice_name.read(cx).value().trim().to_string();
        let label = if typed.is_empty() { self.next_voice_name() } else { typed };

        // Recorded at the moment of agreement, with the wording as it stood
        // then — not looked up later from whatever the app says by then.
        let consent = speech_engine::Consent {
            statement: t!("enrol.consent").to_string(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            source: if self.imported.is_some() { "imported" } else { "recording" }.into(),
        };

        // Registering conditions the voice, and that is the slow part — around
        // forty seconds. None of it needs the recorder on screen, so the sheet
        // closes on the way out and the work carries on behind whatever the
        // person goes back to. `warming` rather than a `Status`: it explains a
        // slower first clip without switching Generate off.
        self.enrolment = Enrolment::Closed;
        self.in_setup = false;
        self.recorder = None;
        self.consent_given = false;
        self.imported = None;
        self.clear_take();
        self.warming = Some(voice_id.clone());
        cx.notify();

        let enrolled_id = voice_id.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    engine.register_voice(Voice {
                        voice_id,
                        label,
                        reference_audio: path,
                        // The script is the transcript: reading a known sentence
                        // removes any need for ASR in the enrolment path.
                        reference_text: ENROLMENT_SCRIPT.into(),
                        // Measured by the engine once the file is stored.
                        seconds: 0.0,
                        consent,
                    })?;
                    engine.voices()
                })
                .await;

            this.update(cx, |this, cx| {
                // Whatever happened, this voice is no longer being worked on.
                // Checked rather than cleared outright: a second enrolment may
                // have started since, and that one is still going.
                if this.warming.as_deref() == Some(enrolled_id.as_str()) {
                    this.warming = None;
                }
                match result {
                    Ok(voices) => {
                        // The clip that asked for the voice switches to it —
                        // recording started from its panel, so saving finishes
                        // that thought rather than leaving it to be picked.
                        let saved = voices.iter().find(|v| v.voice_id == enrolled_id);
                        this.voice_saved =
                            saved.map(|v| crate::workspace::duration(v.seconds));
                        let picked = saved.map(|v| v.voice_id.clone());
                        this.voices = voices;
                        if let Some(id) = picked {
                            if let Some(draft) = this.draft_mut() {
                                draft.voice_id = Some(id.clone());
                            }
                            this.selected_voice = Some(id);
                            this.warm_selected_voice(cx);
                        }
                    }
                    // The recorder is long gone and the person has moved on, so
                    // this goes to the row that carries failures rather than
                    // pulling them back to a sheet they finished with.
                    Err(err) => this.status = Status::Failed(format!("{err}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Speak the composed text. `seed` pins the take: `None` draws a fresh one
    /// so a second press gives a different reading, and passing the previous
    /// seed back changes only the words.
    /// Speak whatever is selected. A draft generates itself; a finished clip
    /// generates another take of the same words, which is what "generate
    /// again" means with the text untouched.
    pub(crate) fn generate(&mut self, seed: Option<u32>, cx: &mut Context<Self>) {
        let Some(engine) = self.engine.clone() else { return };
        if self.busy() {
            return;
        }
        self.save_open_text(cx);

        // Generating again adds a take to the clip it came from; a draft makes
        // a new one.
        let mut clip_id = None;
        // The clip carries its own settings, so this is the one place they are
        // read: the draft's, or the ones the finished clip was made with.
        let (text, voice_id, model, name, seed) = match &self.selected {
            clips::Selected::Draft(_) => {
                let Some(draft) = self.draft().cloned() else { return };
                (
                    draft.text,
                    draft.voice_id,
                    draft.model.or_else(|| self.selected_model.clone()),
                    draft.name,
                    seed.or(draft.seed).or(self.pinned_seed),
                )
            }
            clips::Selected::Clip(..) => {
                let Some(clip) = self.clip().cloned() else { return };
                clip_id = Some(clip.id);
                (
                    clip.text,
                    clip.voice_id,
                    Some(clip.model),
                    Some(clip.name),
                    seed.or(self.pinned_seed),
                )
            }
        };

        self.generating_row = clip_id.clone().or_else(|| match &self.selected {
            clips::Selected::Draft(id) => Some(id.clone()),
            clips::Selected::Clip(id, _) => Some(id.clone()),
        });

        // Nothing can speak yet. Keep the words and run them when it can.
        if !self.model_ready() {
            if !text.trim().is_empty() {
                self.queued = Some(text);
                cx.notify();
            }
            return;
        }
        if text.trim().is_empty() {
            return;
        }

        // The row in the list says it is running, which is what lets you start
        // another clip without losing sight of this one.
        if let Some(draft) = self.draft_mut() {
            draft.generating = true;
        }

        let request = SynthesisRequest {
            text,
            output: std::env::temp_dir().join("voicestudio_output.wav"),
            model,
            // `None` is a real choice, not a missing one: it asks the model to
            // speak in its own voice rather than clone.
            voice_id,
            seed,
            name,
            clip_id,
        };

        self.status = Status::Generating;
        self.progress = None;
        self.expected_s = request.text.split_whitespace().count() as f32 / 3.2;
        cx.notify();
        self.tick_progress(cx);

        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { engine.synthesize(request) }).await;
            this.update(cx, |this, cx| {
                this.progress = None;
                this.stopping = false;
                if let Err(err) = &result {
                    if matches!(err, speech_engine::EngineError::NotRunning) {
                        this.engine_failed(speech_engine::EngineError::NotRunning, cx);
                        return;
                    }
                }
                this.status = match result {
                    Ok(s) => {
                        // The draft has become a clip: drop it from the list and
                        // point the workspace at what it produced.
                        this.generating_row = None;
                        this.drafts.retain(|d| !d.generating);
                        if let Some(clip) = s.clip.as_ref() {
                            let take = clip
                                .latest()
                                .map(|t| t.id.clone())
                                .unwrap_or_else(|| clip.id.clone());
                            this.selected = clips::Selected::Clip(clip.id.clone(), take);
                        }
                        // The stored clip, not the scratch file it was written
                        // to: the sidebar lists the stored path, and loading the
                        // other one leaves the row that is playing unmarked.
                        let source = s
                            .clip
                            .as_ref()
                            .and_then(|c| c.latest())
                            .map(|t| t.path.clone())
                            .unwrap_or_else(|| s.output.clone());
                        this.load_clip(&source, s.audio_s, cx);
                        this.refresh_clips(cx);
                        this.refresh_models(cx);
                        this.last = Some(s.clone());
                        Status::Done {
                            output: s.output,
                            audio_s: s.audio_s,
                            gen_s: s.gen_s,
                        }
                    }
                    Err(err) => {
                        // Still a draft, and still selected: the words are not
                        // lost because the engine refused them.
                        for draft in this.drafts.iter_mut() {
                            draft.generating = false;
                        }
                        this.generating_row = None;
                        match err {
                            speech_engine::EngineError::Refused(reason) => Status::Refused(reason),
                            other => Status::Failed(format!("{other}")),
                        }
                    }
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }


    /// Copy a finished clip somewhere the user picked. The generated file sits
    /// in a temp directory that the system may clear, so keeping it means
    /// copying it out, not linking to it.
    pub(crate) fn save_clip_as(
        &mut self,
        source: PathBuf,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let home = dirs_home();
        let chosen = cx.prompt_for_new_path(&home, Some("voice-note.wav"));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(target))) = chosen.await else { return };
            let copied = cx
                .background_spawn(async move { std::fs::copy(&source, &target).map(|_| target) })
                .await;
            this.update(cx, |this, cx| {
                if let Err(err) = copied {
                    this.status = Status::Failed(format!("{err}"));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Turn an engine error into either a message or a recovery.
    ///
    /// A dead sidecar is not the user's mistake and not something they can act
    /// on, so it restarts rather than reporting. Everything the engine holds is
    /// rebuilt from disk on the way back — models, voices, and the conditioning
    /// for the selected voice — so the only visible cost is the wait.
    pub(crate) fn engine_failed(&mut self, err: speech_engine::EngineError, cx: &mut Context<Self>) {
        // Whatever went wrong, nothing is generating any more. This used to be
        // missed on the NotRunning path — the engine restarted and the draft
        // stayed flagged as running, so the sidebar counted a clip that was not
        // being made and the title bar said generating with nothing to show.
        self.generating_row = None;
        self.progress = None;
        for draft in self.drafts.iter_mut() {
            draft.generating = false;
        }
        if matches!(err, speech_engine::EngineError::NotRunning) {
            self.engine = None;
            self.progress = None;
            self.status = Status::Preparing(t!("status.engine_restarting").to_string());
            cx.notify();
            self.start_engine(cx);
        } else {
            self.status = Status::Failed(format!("{err}"));
            cx.notify();
        }
    }

    /// Stop the running generation. It ends at the next chunk boundary rather
    /// than mid-sentence, so a one-chunk clip finishes; anything longer stops
    /// where a sentence ended.
    pub(crate) fn cancel_generation(&mut self, cx: &mut Context<Self>) {
        // Through the engine, because how a generation is asked to stop is the
        // engine's business: one is not listening while it works and has to be
        // told by file, the other is and can simply be asked.
        let Some(engine) = self.engine.clone() else { return };
        self.stopping = true;
        cx.background_spawn(async move { engine.cancel_generation() }).detach();
        cx.notify();
    }

    /// Poll where the generation has got to. The loop ends with the generation,
    /// so an idle app is not asking ten times a second.
    fn tick_progress(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(150)).await;
            let engine = this.update(cx, |this, _| this.engine.clone()).ok().flatten();
            let reported = match engine {
                Some(engine) => cx.background_spawn(async move { engine.progress() }).await,
                None => None,
            };
            let generating = this
                .update(cx, |this, cx| {
                    this.progress = reported;
                    cx.notify();
                    matches!(this.status, Status::Generating)
                })
                .unwrap_or(false);
            if !generating {
                break;
            }
        })
        .detach();
    }

    /// Hand a finished clip to the player, ready but not playing.
    pub(crate) fn load_clip(&mut self, path: &std::path::Path, audio_s: f32, cx: &mut Context<Self>) {
        self.clip = Some((path.to_path_buf(), audio_s));
        self.clip_levels = recorder::load_audio(path)
            .map(|(samples, _)| recorder::envelope(&samples, CLIP_BARS))
            .unwrap_or_default();
        if let Some(player) = self.player.as_mut() {
            if let Err(err) = player.load(path, Duration::from_secs_f32(audio_s)) {
                self.status = Status::Failed(err);
            }
        }
        cx.notify();
    }

    /// Play the take under review, or pause it. Loaded on demand rather than
    /// kept in the player: review is the one place a recording is heard before
    /// it becomes a voice, and the player otherwise belongs to the clips.
    pub(crate) fn toggle_take(&mut self, cx: &mut Context<Self>) {
        let Some(take) = self.take.as_ref() else { return };
        if self.take_loaded() {
            self.toggle_playback(cx);
            return;
        }
        let (path, seconds) = (take.path.clone(), take.seconds);
        self.load_clip(&path, seconds, cx);
        if let Some(player) = self.player.as_ref() {
            player.play();
        }
        self.tick_playback(cx);
        cx.notify();
    }

    /// The clip the transport is pointed at, if any, and whether sound is
    /// coming out of it.
    pub(crate) fn playing_clip(&self) -> Option<&std::path::Path> {
        self.clip.as_ref().map(|(path, _)| path.as_path())
    }

    pub(crate) fn is_playing(&self) -> bool {
        self.player.as_ref().map(|p| p.is_playing()).unwrap_or(false)
    }

    /// Drop the take, and the transport with it when that is what it was
    /// holding — otherwise the workspace is left pointing at a recording that
    /// is no longer under review.
    fn clear_take(&mut self) {
        if self.take_loaded() {
            if let Some(player) = self.player.as_ref() {
                player.pause();
            }
            self.clip = None;
            self.clip_levels.clear();
        }
        self.take = None;
    }

    /// Start whatever the transport is holding. Separate from `toggle_take`
    /// because the caller here has just loaded something and means to hear it.
    pub(crate) fn play_loaded(&mut self, cx: &mut Context<Self>) {
        if let Some(player) = self.player.as_ref() {
            player.play();
        }
        self.tick_playback(cx);
        cx.notify();
    }

    /// Choose the voice this clip speaks in. `None` is the bundled default,
    /// which is a real choice rather than the absence of one.
    pub(crate) fn choose_voice(&mut self, voice_id: Option<String>, cx: &mut Context<Self>) {
        // Not cleared here: saving a voice chooses it, and clearing the banner
        // from inside the choosing would erase the thing that just happened.
        // Picking a voice by hand clears it, because then it is stale.
        self.voice_saved = None;
        if let Some(draft) = self.draft_mut() {
            draft.voice_id = voice_id.clone();
        }
        self.selected_voice = voice_id;
        // Warm on selection, not on Generate: the wait belongs to the moment of
        // choosing, not to the first clip.
        self.warm_selected_voice(cx);
        cx.notify();
    }

    /// Play a voice so it can be heard before it is picked. The bundled default
    /// has no recording to play, so it speaks a line instead.
    pub(crate) fn hear_voice(&mut self, voice_id: Option<String>, cx: &mut Context<Self>) {
        match voice_id.and_then(|id| {
            self.voices.iter().find(|v| v.voice_id == id).map(|v| v.reference_audio.clone())
        }) {
            Some(reference) => {
                self.load_clip(&reference, 0.0, cx);
                self.play_loaded(cx);
            }
            // The bundled voice has no recording to play — it is the model
            // speaking as itself, so hearing it means generating a line.
            None => {
                self.choose_voice(None, cx);
                self.generate(None, cx);
            }
        }
    }

    /// Draw a new seed, so the next take is a different reading of the same
    /// words. Clearing it back to `None` is what "fresh each time" means.
    pub(crate) fn reroll_seed(&mut self, cx: &mut Context<Self>) {
        let drawn = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        if let Some(draft) = self.draft_mut() {
            draft.seed = match draft.seed {
                Some(_) => None,
                None => Some(drawn % 10_000),
            };
        }
        cx.notify();
    }

    /// Delete the clip being looked at, and put the workspace on a fresh
    /// draft — there is nothing left to look at once it is gone.
    pub(crate) fn delete_selected_clip(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.new_draft(window, cx);
        self.delete_clip(id, cx);
    }

    /// Copy a clip, audio and all, so the copy can be changed or deleted
    /// without touching the one it came from.
    pub(crate) fn duplicate_clip(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(engine) = self.engine.clone() else { return };
        cx.spawn(async move |this, cx| {
            let clips = cx.background_spawn(async move { engine.duplicate_clip(id) }).await;
            this.update(cx, |this, cx| {
                match clips {
                    Ok(clips) => this.clips = clips,
                    Err(err) => this.status = Status::Failed(format!("{err}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn open_inspector(&mut self, cx: &mut Context<Self>) {
        self.inspector = true;
        cx.notify();
    }

    pub(crate) fn toggle_inspector(&mut self, cx: &mut Context<Self>) {
        self.inspector = !self.inspector;
        cx.notify();
    }

    /// Seconds still to run, measured in chunks finished rather than seconds of
    /// audio against a guess.
    ///
    /// This used to divide by `expected_s`, which is a word count over an
    /// assumed speaking rate. Real speech regularly overshoots it, and when it
    /// did the remainder clamped to zero: the interface said "163 of 149
    /// seconds" and "about 0 seconds left" while generation carried on. Chunks
    /// are counted, not estimated, so the figure only ever moves at a chunk
    /// boundary and cannot run past the end.
    ///
    /// `None` until the first chunk lands, because until then there is no
    /// measured rate to project from — and a number invented before the first
    /// measurement is what made the estimate swing.
    pub(crate) fn seconds_left(&self) -> Option<f32> {
        let p = self.progress.as_ref()?;
        let (done, total) = (p.chunks_done, p.chunks);
        (done > 0 && total > done && p.elapsed_s > 0.0)
            .then(|| p.elapsed_s / done as f32 * (total - done) as f32)
    }

    /// How far the running generation is, by chunks finished — a fact, where
    /// seconds against an estimate moves when the estimate is wrong.
    pub(crate) fn generation_fraction(&self) -> f32 {
        match self.progress.as_ref() {
            Some(p) if p.chunks > 0 => p.chunks_done as f32 / p.chunks as f32,
            _ => 0.0,
        }
    }

    /// Whether sound is coming out and how far through, for the strip.
    pub(crate) fn player_state(&self) -> Option<(bool, f32)> {
        let player = self.player.as_ref()?;
        Some((player.is_playing(), player.progress()))
    }

    pub(crate) fn player_position(&self) -> Duration {
        self.player.as_ref().map(|p| p.position()).unwrap_or_default()
    }

    /// Turn a click on the waveform into a position in the clip. Checking one
    /// word in a 24 second take should not mean listening to the 23 before it.
    pub(crate) fn seek_from(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some(player) = self.player.as_ref() else { return };
        let bounds = self.track.get();
        if bounds.size.width <= px(0.0) {
            return;
        }
        let fraction: f32 = ((x - bounds.origin.x) / bounds.size.width).into();
        if let Err(err) = player.seek_to(fraction) {
            self.status = Status::Failed(err);
        }
        self.tick_playback(cx);
        cx.notify();
    }

    /// The words currently being spoken, which belong to the draft that is
    /// running rather than to whatever is selected now.
    pub(crate) fn running_text(&self) -> String {
        self.drafts
            .iter()
            .find(|d| d.generating)
            .map(|d| d.text.clone())
            // Generating again runs a finished clip's own words, and there is
            // no draft holding them.
            .or_else(|| self.clip().map(|c| c.text.clone()))
            .unwrap_or_default()
    }

    pub(crate) fn save_selected_clip(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(take) = self.take().cloned() else { return };
        self.save_clip_as(take.path, window, cx);
    }

    /// Put the clip on the clipboard as a file, which is what "copy audio"
    /// means everywhere else on this machine: it pastes into Finder, Mail and
    /// Messages as the recording itself rather than as its path.
    pub(crate) fn copy_selected_clip(&mut self, cx: &mut Context<Self>) {
        let Some(take) = self.take().cloned() else { return };
        let script = format!(
            "set the clipboard to (POSIX file \"{}\")",
            take.path.display().to_string().replace('\\', "\\\\").replace('"', "\\\"")
        );
        if let Err(err) = std::process::Command::new("osascript").arg("-e").arg(script).status() {
            self.status = Status::Failed(err.to_string());
            cx.notify();
        }
    }

    pub(crate) fn can_generate(&self, cx: &Context<Self>) -> bool {
        !self.enrolling_over_workspace()
            && !self.busy()
            && !self.text.read(cx).value().trim().is_empty()
            && (self.model_ready() || self.queued.is_none())
    }

    /// The line beside Generate. It says the one thing that is true right now:
    /// what is missing, what is downloading, or what this will cost.
    pub(crate) fn generate_hint(&self, cx: &Context<Self>) -> String {
        // The sheet has the floor; the row behind it says nothing.
        if self.enrolling_over_workspace() {
            return String::new();
        }
        // Right after saving a voice, the thing worth saying is what changed.
        if self.voice_saved.is_some() {
            return t!("compose.now_in_your_voice").to_string();
        }
        let text = self.text.read(cx).value().to_string();
        if text.trim().is_empty() {
            return t!("compose.write_first").to_string();
        }
        if !self.model_ready() {
            return t!("compose.model_missing", model = self.model_label()).to_string();
        }
        // Only from this machine's own measurements — a borrowed benchmark
        // would put a number on it that this Mac has never produced.
        let spoken = text.split_whitespace().count() as f32 / crate::workspace::WORDS_PER_SECOND;
        match self
            .models
            .iter()
            .find(|m| Some(m.id.as_str()) == self.clip_model())
            .and_then(|m| m.measured_rtf)
        {
            Some(rtf) => {
                t!("compose.about_to_make", seconds = format!("{:.0}", spoken * rtf)).to_string()
            }
            None => String::new(),
        }
    }

    fn take_loaded(&self) -> bool {
        match (self.take.as_ref(), self.clip.as_ref()) {
            (Some(take), Some((path, _))) => *path == take.path,
            _ => false,
        }
    }

    /// Whether the take is the sound currently coming out, and how far through
    /// it is — the two things the review waveform needs.
    pub(crate) fn take_playback(&self) -> (bool, f32) {
        if !self.take_loaded() {
            return (false, 0.0);
        }
        match self.player.as_ref() {
            Some(player) => (player.is_playing(), player.progress()),
            None => (false, 0.0),
        }
    }

    fn toggle_playback(&mut self, cx: &mut Context<Self>) {
        let Some(player) = self.player.as_ref() else { return };
        // A finished clip should replay rather than sit at the end doing nothing.
        if player.finished() {
            if let Some((path, audio_s)) = self.clip.clone() {
                self.load_clip(&path, audio_s, cx);
            }
            if let Some(player) = self.player.as_ref() {
                player.play();
            }
        } else {
            player.toggle();
        }
        self.tick_playback(cx);
        cx.notify();
    }

    /// Repaint while audio is playing so the progress bar advances. The loop
    /// ends when playback does, so an idle app is not waking up 10x a second.
    fn tick_playback(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let keep_going = this
                .update(cx, |this, cx| {
                    cx.notify();
                    this.player.as_ref().map(|p| p.is_playing()).unwrap_or(false)
                })
                .unwrap_or(false);
            if !keep_going {
                break;
            }
        })
        .detach();
    }






    /// Start a download and poll it until it settles. Models are gigabytes, so
    /// the picker shows progress rather than appearing to hang.
    pub(crate) fn install_model(&mut self, model_id: String, cx: &mut Context<Self>) {
        let Some(engine) = self.engine.clone() else { return };
        if self.installs.get(&model_id).is_some_and(InstallStatus::is_downloading) {
            return;
        }

        cx.spawn(async move |this, cx| {
            let started = {
                let engine = engine.clone();
                let model = model_id.clone();
                cx.background_spawn(async move { engine.install_model(model) }).await
            };
            if let Err(err) = started {
                this.update(cx, |this, cx| {
                    this.status = Status::Failed(format!("{err}"));
                    cx.notify();
                })
                .ok();
                return;
            }

            loop {
                let status = {
                    let engine = engine.clone();
                    let model = model_id.clone();
                    cx.background_spawn(async move { engine.install_status(model) }).await
                };
                let Ok(status) = status else { break };
                let downloading = status.is_downloading();

                let keep = this
                    .update(cx, |this, cx| {
                        let failed = status.error.clone();
                        this.installs.insert(model_id.clone(), status);
                        if let Some(err) = failed {
                            this.status = Status::Failed(err);
                        }
                        cx.notify();
                        downloading
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
                cx.background_executor().timer(Duration::from_millis(700)).await;
            }

            // Refresh the catalogue so the model reads as installed.
            let models = {
                let engine = engine.clone();
                cx.background_spawn(async move { engine.models() }).await
            };
            this.update(cx, |this, cx| {
                if let Ok(models) = models {
                    this.models = models;
                }
                this.installs.remove(&model_id);
                // The queue exists so nobody has to wait here. Run it now.
                if this.queued.is_some() && this.model_ready() {
                    this.queued = None;
                    this.generate(None, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }


    pub(crate) fn delete_model(&mut self, model_id: String, cx: &mut Context<Self>) {
        let Some(engine) = self.engine.clone() else { return };
        self.confirming_delete = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = {
                let engine = engine.clone();
                let id = model_id.clone();
                cx.background_spawn(async move {
                    engine.delete_model(id)?;
                    engine.models()
                })
                .await
            };
            this.update(cx, |this, cx| {
                match result {
                    Ok(models) => {
                        // Keep the selection on something that still exists.
                        if !models.iter().any(|m| {
                            Some(m.id.as_str()) == this.selected_model.as_deref() && m.installed
                        }) {
                            this.selected_model = models
                                .iter()
                                .find(|m| m.installed)
                                .map(|m| m.id.clone());
                        }
                        this.models = models;
                    }
                    Err(err) => this.status = Status::Failed(format!("{err}")),
                }
                this.refresh_system(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Put a voice's name into the field and hand it the keyboard.
    pub(crate) fn start_voice_rename(
        &mut self,
        voice_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(voice) = self.voices.iter().find(|v| v.voice_id == voice_id) else { return };
        let label = voice.label.clone();
        self.voice_rename.update(cx, |state, cx| state.set_value(label, window, cx));
        self.renaming_voice = Some(voice_id);
        self.confirming_voice = None;
        self.voice_rename.update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    /// Save the edited name, unless it has been emptied — a voice with no name
    /// is a row that cannot be told from the others.
    pub(crate) fn commit_voice_rename(&mut self, cx: &mut Context<Self>) {
        let Some(voice_id) = self.renaming_voice.take() else { return };
        let label = self.voice_rename.read(cx).value().trim().to_string();
        // The field has closed either way, so the pane has to be told even when
        // there is nothing to save.
        cx.notify();
        let Some(engine) = self.engine.clone() else { return };
        if label.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let voices =
                cx.background_spawn(async move { engine.rename_voice(voice_id, label) }).await;
            this.update(cx, |this, cx| {
                if let Ok(voices) = voices {
                    this.voices = voices;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn delete_voice(&mut self, voice_id: String, cx: &mut Context<Self>) {
        let Some(engine) = self.engine.clone() else { return };
        self.confirming_voice = None;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    engine.delete_voice(voice_id)?;
                    engine.voices()
                })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(voices) => {
                        // A clip pointing at a deleted voice falls back to the
                        // bundled one rather than to whichever sorts first.
                        let gone = |id: &Option<String>| {
                            id.as_ref().is_some_and(|id| !voices.iter().any(|v| &v.voice_id == id))
                        };
                        if gone(&this.selected_voice) {
                            this.selected_voice = None;
                        }
                        for draft in this.drafts.iter_mut() {
                            if gone(&draft.voice_id) {
                                draft.voice_id = None;
                            }
                        }
                        this.voices = voices;
                    }
                    Err(err) => this.status = Status::Failed(format!("{err}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Download and install the speech runtime, then start the engine.
    fn install_runtime(&mut self, archive: Option<PathBuf>, cx: &mut Context<Self>) {
        self.status = Status::Installing { step: "Starting…".into(), fraction: 0.0 };
        cx.notify();

        let (tx, rx) = std::sync::mpsc::channel::<runtime::Progress>();
        std::thread::spawn(move || {
            runtime::install_from(archive, |p| {
                let _ = tx.send(p);
            })
        });

        cx.spawn(async move |this, cx| {
            loop {
                // The installer runs on its own thread; drain what it has sent
                // without blocking the executor between updates.
                let received: Vec<_> = rx.try_iter().collect();
                let mut finished = false;
                for progress in received {
                    let done = this
                        .update(cx, |this, cx| {
                            match progress {
                                runtime::Progress::Step(step) => {
                                    let fraction = match &this.status {
                                        Status::Installing { fraction, .. } => *fraction,
                                        _ => 0.0,
                                    };
                                    this.status = Status::Installing { step, fraction };
                                }
                                runtime::Progress::Fraction(f) => {
                                    if let Status::Installing { fraction, .. } = &mut this.status {
                                        *fraction = f;
                                    }
                                }
                                runtime::Progress::Done => return true,
                                runtime::Progress::Failed(err) => {
                                    this.status = Status::Failed(err);
                                    return true;
                                }
                            }
                            cx.notify();
                            false
                        })
                        .unwrap_or(true);
                    finished |= done;
                }
                if finished {
                    break;
                }
                cx.background_executor().timer(Duration::from_millis(200)).await;
            }

            this.update(cx, |this, cx| {
                if !matches!(this.status, Status::Failed(_)) {
                    this.status = Status::Idle;
                    // Step one is finished, and says so until it is dismissed.
                    this.runtime_done = true;
                    this.start_engine(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }


    fn refresh_system(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.engine.clone() else { return };
        cx.spawn(async move |this, cx| {
            let info = cx.background_spawn(async move { engine.system_info() }).await;
            this.update(cx, |this, cx| {
                if let Ok(info) = info {
                    this.system = Some(info);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Clips are loaded with voices, since the sidebar shows both.
    fn refresh_clips(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.engine.clone() else { return };
        cx.spawn(async move |this, cx| {
            let clips = cx.background_spawn(async move { engine.clips() }).await;
            this.update(cx, |this, cx| {
                if let Ok(clips) = clips {
                    this.clips = clips;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn delete_clip(&mut self, clip_id: String, cx: &mut Context<Self>) {
        let Some(engine) = self.engine.clone() else { return };
        cx.spawn(async move |this, cx| {
            let clips = cx.background_spawn(async move { engine.delete_clip(clip_id) }).await;
            this.update(cx, |this, cx| {
                match clips {
                    Ok(clips) => this.clips = clips,
                    Err(err) => this.status = Status::Failed(format!("{err}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Model rows carry measured speed, which changes as clips accumulate.
    pub(crate) fn refresh_models(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.engine.clone() else { return };
        cx.spawn(async move |this, cx| {
            let models = cx.background_spawn(async move { engine.models() }).await;
            this.update(cx, |this, cx| {
                if let Ok(models) = models {
                    this.models = models;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The name the next enrolled voice will take, shown before saving.
    pub(crate) fn next_voice_name(&self) -> String {
        let ordinal = self.voices.len() + 1;
        if ordinal == 1 {
            t!("voice.mine").to_string()
        } else {
            format!("{} {ordinal}", t!("voice.mine"))
        }
    }

    /// The one place the screen rules live, so they can be read in full.
    ///
    /// Enrolment is not forced by an empty voice list. Every model in the
    /// catalogue speaks without a reference, so no voice means "using the
    /// model's own voice", which the workspace can show and generate from.
    /// Cloning is offered on first run and can be declined.
    pub(crate) fn screen(&self) -> Screen {
        // No engine and a failure means the runtime is what is missing, and
        // there is no workspace to fall back to.
        if self.runtime_done
            || matches!(self.status, Status::Installing { .. })
            || (self.engine.is_none() && matches!(self.status, Status::Failed(_)))
        {
            Screen::Setup
        } else if self.choosing_model
            // Nothing can be spoken until a model is on disk, so that download
            // is a step rather than an error discovered at Generate.
            || (self.engine.is_some() && self.installed_models() == 0)
        {
            Screen::Models
        } else if self.in_setup && !matches!(self.enrolment, Enrolment::Closed) {
            Screen::Enrolment
        } else {
            Screen::Workspace
        }
    }

    /// Whether the recorder is showing over the workspace rather than instead
    /// of it.
    pub(crate) fn enrolling_over_workspace(&self) -> bool {
        !matches!(self.enrolment, Enrolment::Closed) && !self.in_setup
    }

    /// The voice the next generation will use: an enrolled one, or `None` for
    /// the model's own voice. A model that cannot clone always answers `None`,
    /// so a stale selection can never reach the engine as an unusable request.
    pub(crate) fn speaking_voice(&self) -> Option<&str> {
        if !self.model_can_clone() {
            return None;
        }
        self.selected_voice
            .as_deref()
            .filter(|id| self.voices.iter().any(|v| v.voice_id == *id))
    }

    /// Whether the selected model can speak in a supplied voice. Read from the
    /// catalogue rather than assumed: a TTS-only model offered as if it cloned
    /// would fail at Generate, after the user recorded themselves.
    pub(crate) fn model_can_clone(&self) -> bool {
        self.selected_model
            .as_deref()
            .and_then(|id| self.models.iter().find(|m| m.id == id))
            .is_some_and(|m| m.supports_cloning)
    }

    /// Choosing a model can invalidate the voice: switching to a TTS-only model
    /// drops the selection rather than leaving a voice shown that is not used.
    pub(crate) fn select_model(&mut self, id: String, cx: &mut Context<Self>) {
        self.selected_model = Some(id);
        if !self.model_can_clone() {
            self.selected_voice = None;
        }
        // The conditioning is built against one model, so a switch needs a
        // fresh warm rather than inheriting the previous model's.
        self.warming = None;
        self.warm_selected_voice(cx);
        cx.notify();
    }

    pub(crate) fn installed_models(&self) -> usize {
        self.models.iter().filter(|m| m.installed).count()
    }

    pub(crate) fn model_ready(&self) -> bool {
        self.selected_model
            .as_deref()
            .and_then(|id| self.models.iter().find(|m| m.id == id))
            .is_none_or(|m| m.installed)
    }

    /// Whether anything is running — a model preparing, a generation, an
    /// install. Right for disabling Generate; wrong for deciding what the
    /// composer shows.
    pub(crate) fn busy(&self) -> bool {
        matches!(
            self.status,
            Status::Preparing(_) | Status::Generating | Status::Installing { .. }
        )
    }

    /// Whether the clip *on screen* is the one running.
    ///
    /// The composer used to key off `busy()`, which is global: selecting
    /// another clip mid-generation changed the selection and the text
    /// underneath, while the card carried on showing the running one's
    /// progress and locked words. Two clips' states on one screen. A draft
    /// carries its own `generating` flag, so the card can follow the
    /// selection and the run stays visible in the sidebar where it belongs.
    pub(crate) fn showing_generation(&self) -> bool {
        // Installing or preparing is not attached to any row, so it shows
        // wherever you are — there is nowhere else for it to go.
        if self.busy() && !matches!(self.status, Status::Generating) {
            return true;
        }
        let selected = match &self.selected {
            crate::clips::Selected::Draft(id) => id,
            crate::clips::Selected::Clip(id, _) => id,
        };
        self.generating_row.as_deref() == Some(selected.as_str())
    }

    fn status_line(&self, cx: &Context<Self>) -> AnyElement {
        let (text, colour) = match &self.status {
            Status::Idle if self.warming.is_some() => (
                t!("status.warming").to_string(),
                cx.theme().muted_foreground,
            ),
            Status::Idle if self.speaking_voice().is_none() => (
                t!("status.built_in_ready").to_string(),
                cx.theme().muted_foreground,
            ),
            Status::Idle => (t!("status.ready").to_string(), cx.theme().muted_foreground),
            Status::Preparing(what) => (what.clone(), cx.theme().foreground),
            Status::Installing { step, fraction } => (
                format!("{step}  {:.0}%", fraction * 100.0),
                cx.theme().foreground,
            ),
            // Honest about the wait: this is a 15-25s operation, not a blink.
            Status::Generating => (
                t!("status.generating").to_string(),
                cx.theme().foreground,
            ),
            Status::Done { output, audio_s, gen_s } => (
                t!("status.saved", path = output.display().to_string(),
                   audio = format!("{audio_s:.1}"), gen = format!("{gen_s:.0}")).to_string(),
                cx.theme().foreground,
            ),
            Status::Failed(err) => (t!("status.failed", reason = err).to_string(), cx.theme().danger),
            Status::Refused(reason) => (reason.clone(), cx.theme().muted_foreground),
        };
        div().text_size(px(12.0)).text_color(colour).child(text).into_any_element()
    }

}


impl Render for VoiceStudio {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let screen = self.screen();

        div()
            .relative()
            .v_flex()
            .size_full()
            .bg(cx.theme().background)
            .on_action(cx.listener(|this, _: &Speak, _, cx| this.generate(None, cx)))
            // Enter and Escape mean the same thing to either name field; which
            // one is open decides which is answered.
            .on_action(cx.listener(|this, _: &CommitRename, _, cx| {
                if this.renaming_voice.is_some() {
                    this.commit_voice_rename(cx);
                } else {
                    this.commit_rename(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &CancelRename, _, cx| {
                if this.renaming_voice.take().is_some() {
                    cx.notify();
                } else {
                    this.cancel_rename(cx);
                }
            }))
            .child(self.title_bar(window, cx))
            .child(match screen {
                Screen::Setup => self.setup_screen(cx).into_any_element(),
                Screen::Models => self.models_screen(window, cx).into_any_element(),
                Screen::Enrolment => self.enrolment_screen(window, cx).into_any_element(),
                Screen::Workspace => div()
                    .relative()
                    .h_flex()
                    .flex_1()
                    .min_h(px(0.0))
                    .child(self.sidebar(cx))
                    .child(self.composer(cx))
                    .child(self.inspector_panel(cx))
                    // Inside the body, so the title bar stays lit and the sheet
                    // sits centred on the work rather than on the window.
                    .when(self.enrolling_over_workspace(), |this| {
                        this.child(self.enrolment_sheet(window, cx))
                    })
                    .child(self.clip_menu(cx))
                    .into_any_element(),
            })
            // The design gives the workspace no status strip — the composer
            // says what is happening. A failure has nowhere else to go, so that
            // is the one thing this row still carries.
            .when(
                screen == Screen::Workspace && matches!(self.status, Status::Failed(_)),
                |this| this.child(self.status_row(cx)),
            )
            .child(self.model_menu(cx))
            .child(self.settings_window(cx))
    }
}

impl VoiceStudio {
    /// One line of state, under the working area rather than beside a button.
    fn status_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_none()
            .px(px(24.0))
            .py(px(8.0))
            .child(self.status_line(cx))
    }

    /// First run: what is missing, how big it is, and where it goes.
    /// Setup step 1, and the one state that has no workspace behind it: the
    /// runtime is what generates speech, so without it there is nothing to
    /// return to. The screen says what is blocked, what still works, and gives
    /// two ways forward — resume, or install from a file on a machine with no
    /// connection.
    /// One of the two cards that carry the offline path. Stretched to equal
    /// height so the pair reads as alternatives rather than a stack.
    fn offline_card() -> Div {
        div()
            .v_flex()
            .flex_1()
            .min_w(px(0.0))
            .items_start()
            .gap(px(9.0))
            .px(px(16.0))
            .py(px(15.0))
            .rounded(px(12.0))
            .bg(theme::hex(0xFFFDFA))
            .border_1()
            .border_color(theme::hex(0xEBE4D9))
    }

    fn offline_title(text: String) -> Div {
        div().text_size(px(12.5)).font_semibold().child(text)
    }

    /// Take the interpreter archive from disk instead of the network. Only the
    /// download is skipped: the speech packages are still installed with pip,
    /// which the card says rather than implying a fully offline install.
    fn choose_runtime_archive(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Install from this file".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            this.update(cx, |this, cx| this.install_runtime(Some(path), cx)).ok();
        })
        .detach();
    }

    fn setup_screen(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (step, fraction) = match &self.status {
            Status::Installing { step, fraction } => (step.clone(), *fraction),
            _ => (String::new(), 0.0),
        };
        let started = fraction > 0.0;
        let interrupted = matches!(self.status, Status::Failed(_));
        let reason = match &self.status {
            Status::Failed(err) => err.clone(),
            _ => String::new(),
        };

        div()
            .v_flex()
            .flex_1()
            .min_h(px(0.0))
            .items_center()
            .px(px(40.0))
            .pt(px(30.0))
            .child(
                div()
                    .v_flex()
                    .w_full()
                    .max_w(px(820.0))
                    .gap(px(18.0))
                    // Said once, at the top, on a machine that can never run
                    // this: everything below it would be a wait for nothing.
                    .when_some(runtime::host_supported().err(), |this, reason| {
                        this.child(
                            div()
                                .h_flex()
                                .w_full()
                                .items_start()
                                .gap(px(14.0))
                                .px(px(18.0))
                                .py(px(16.0))
                                .rounded(px(12.0))
                                .bg(theme::hex(0xFFF9F5))
                                .border_1()
                                .border_color(theme::hex(0xF0B7AF))
                                .child(icon::icon(icon::name::BLOCK, 19.0, theme::hex(0xC7362B)))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .text_size(px(12.5))
                                        .line_height(px(19.0))
                                        .text_color(theme::hex(0x5F594F))
                                        .child(reason),
                                ),
                        )
                    })
                    // What this is and what it will cost, before the steps —
                    // the wizard opens on a stranger's machine.
                    .child(
                        div()
                            .v_flex()
                            .child(
                                div()
                                    .font_family(theme::FONT_DISPLAY)
                                    .text_size(px(22.0))
                                    .font_semibold()
                                    .child(t!("setup.header_title").to_string()),
                            )
                            .child(
                                div()
                                    .text_size(px(13.0))
                                    .text_color(theme::hex(0x5F594F))
                                    .mt(px(3.0))
                                    .child(t!("setup.header_detail").to_string()),
                            ),
                    )
                    .child(self.stepper(1, cx))
                    // An interruption is stated with what survived it, because
                    // "did I lose the download" is the first question.
                    .when(interrupted, |this| {
                        this.child(
                            div()
                                .h_flex()
                                .w_full()
                                .items_start()
                                .gap(px(14.0))
                                .px(px(18.0))
                                .py(px(16.0))
                                .rounded(px(12.0))
                                .bg(theme::hex(0xFFF9F5))
                                .border_1()
                                .border_color(theme::hex(0xF0B7AF))
                                .child(
                                    div()
                                        .size(px(32.0))
                                        .flex_none()
                                        .rounded(px(9.0))
                                        .bg(theme::hex(0xFDEBE8))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(icon::icon(
                                            icon::name::WIFI_OFF,
                                            19.0,
                                            theme::hex(0xC7362B),
                                        )),
                                )
                                .child(
                                    div()
                                        .v_flex()
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .child(
                                            div()
                                                .font_family(theme::FONT_DISPLAY)
                                                .text_size(px(15.0))
                                                .font_semibold()
                                                .child(t!("setup.stopped").to_string()),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(12.5))
                                                .line_height(px(19.0))
                                                .text_color(theme::hex(0x5F594F))
                                                .mt(px(4.0))
                                                .child(if reason.is_empty() {
                                                    t!("setup.stopped_detail").to_string()
                                                } else {
                                                    format!(
                                                        "{} {reason}",
                                                        t!("setup.stopped_detail")
                                                    )
                                                }),
                                        ),
                                ),
                        )
                    })
                    // The runtime itself: what it is, what it costs, and the
                    // one button that moves it forward.
                    .child(
                        ui::card()
                            .w_full()
                            .child(
                                div()
                                    .h_flex()
                                    .w_full()
                                    .items_center()
                                    .gap(px(12.0))
                                    .px(px(18.0))
                                    .py(px(15.0))
                                    .border_b_1()
                                    .border_color(theme::hex(0xF1EBE1))
                                    .child(
                                        div()
                                            .size(px(34.0))
                                            .flex_none()
                                            .rounded(px(9.0))
                                            .bg(theme::bg_subtle(false))
                                            .border_1()
                                            .border_color(theme::hex(0xE4DCD0))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(icon::icon(
                                                icon::name::MEMORY,
                                                19.0,
                                                theme::hex(0x5F594F),
                                            )),
                                    )
                                    .child(
                                        div()
                                            .v_flex()
                                            .flex_1()
                                            .min_w(px(0.0))
                                            .child(
                                                div()
                                                    .text_size(px(13.5))
                                                    .font_semibold()
                                                    .child(format!(
                                                        "{} {}",
                                                        runtime::NAME,
                                                        runtime::VERSION
                                                    )),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(12.0))
                                                    .text_color(theme::hex(0x6B645A))
                                                    .mt(px(2.0))
                                                    .child(
                                                        t!(
                                                            "setup.runtime_detail",
                                                            size = format!(
                                                                "{:.0} MB",
                                                                runtime::APPROX_BYTES as f32 / 1e6
                                                            )
                                                        )
                                                        .to_string(),
                                                    ),
                                            ),
                                    )
                                    // Done, and saying so: this is the end of a
                                    // wait someone sat through.
                                    .when(self.runtime_done, |d| {
                                        d.child(
                                            div()
                                                .h_flex()
                                                .flex_none()
                                                .items_center()
                                                .gap(px(7.0))
                                                .text_size(px(12.5))
                                                .font_semibold()
                                                .text_color(theme::hex(0x1B5C41))
                                                .child(icon::filled(
                                                    icon::name::CHECK_CIRCLE,
                                                    17.0,
                                                    theme::hex(0x287A57),
                                                ))
                                                .child(t!("setup.runtime_ready").to_string()),
                                        )
                                    })
                                    .when(!started && !self.runtime_done
                                        && runtime::host_supported().is_ok(), |d| {
                                        d.child(
                                            div()
                                                .h(px(36.0))
                                                .px(px(16.0))
                                                .flex_none()
                                                .h_flex()
                                                .items_center()
                                                .gap(px(7.0))
                                                .rounded(px(8.0))
                                                .bg(theme::hex(0xFF6E08))
                                                .text_size(px(13.0))
                                                .font_semibold()
                                                .text_color(theme::hex(0xFFFEFD))
                                                .child(icon::icon(
                                                    if interrupted {
                                                        icon::name::REFRESH
                                                    } else {
                                                        icon::name::DOWNLOAD
                                                    },
                                                    17.0,
                                                    theme::hex(0xFFFEFD),
                                                ))
                                                .child(if interrupted {
                                                    t!("setup.resume").to_string()
                                                } else {
                                                    t!("setup.install").to_string()
                                                })
                                                .id("install-runtime")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.install_runtime(None, cx)
                                                })),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .v_flex()
                                    .w_full()
                                    .gap(px(10.0))
                                    .px(px(18.0))
                                    .py(px(14.0))
                                    .when(started, |d| {
                                        d.child(div().text_size(px(12.5)).child(step.clone()))
                                            .child(
                                                div()
                                                    .w_full()
                                                    .h(px(5.0))
                                                    .rounded_full()
                                                    .bg(theme::hex(0xEBE4D9))
                                                    .child(
                                                        div()
                                                            .h_full()
                                                            .rounded_full()
                                                            .bg(theme::hex(0xFF8A1F))
                                                            .w(relative(fraction)),
                                                    ),
                                            )
                                    })
                                    // What is blocked and what still works, so
                                    // the wait has a shape rather than being a
                                    // blanket "not ready".
                                    .child(ui::section_label(
                                        t!("setup.until_installed").to_string().to_uppercase(),
                                    ))
                                    .child(
                                        div()
                                            .h_flex()
                                            .w_full()
                                            .gap(px(22.0))
                                            .child(
                                                div()
                                                    .v_flex()
                                                    .gap(px(7.0))
                                                    .child(self.capability(false, t!("setup.cap_generate").to_string(), cx))
                                                    .child(self.capability(false, t!("setup.cap_models").to_string(), cx)),
                                            )
                                            .child(
                                                div()
                                                    .v_flex()
                                                    .gap(px(7.0))
                                                    // Recording is not on this
                                                    // side: setup owns the
                                                    // screen until the engine
                                                    // is up, and a take could
                                                    // not be saved without it
                                                    // anyway.
                                                    .child(self.capability(false, t!("setup.cap_record").to_string(), cx))
                                                    .child(self.capability(true, t!("setup.cap_play").to_string(), cx)),
                                            ),
                                    ),
                            ),
                    )
                    // For a machine that cannot reach the release host: the
                    // real archive URL, not a branded redirect, and a way to
                    // use a copy fetched somewhere else.
                    .when_some(runtime::download_url(), |this, url| {
                        this.child(
                            div()
                                .h_flex()
                                .w_full()
                                .items_stretch()
                                .gap(px(16.0))
                                .child(
                                    Self::offline_card()
                                        .child(Self::offline_title(
                                            t!("setup.no_connection").to_string(),
                                        ))
                                        .child(
                                            div()
                                                .text_size(px(12.0))
                                                .line_height(px(19.0))
                                                .text_color(theme::hex(0x5F594F))
                                                .child(
                                                    t!(
                                                        "setup.no_connection_detail",
                                                        size = format!(
                                                            "{:.0} MB",
                                                            runtime::ARCHIVE_BYTES as f32 / 1e6
                                                        ),
                                                        total = format!(
                                                            "{:.0} MB",
                                                            runtime::APPROX_BYTES as f32 / 1e6
                                                        )
                                                    )
                                                    .to_string(),
                                                ),
                                        )
                                        .child(div().flex_1())
                                        .child(
                                            ui::secondary_button(
                                                Some((icon::name::FOLDER_OPEN, 0x5F594F)),
                                                t!("setup.install_from_file").to_string(),
                                            )
                                            .flex_none()
                                            .id("install-from-file")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.choose_runtime_archive(window, cx)
                                            })),
                                        ),
                                )
                                .child(
                                    Self::offline_card()
                                        .child(Self::offline_title(
                                            t!("setup.direct_link").to_string(),
                                        ))
                                        .child(ui::mono(url.clone(), 11.5, theme::hex(0x5F594F)))
                                        .child(div().flex_1())
                                        .child(
                                            div()
                                                .h_flex()
                                                .flex_none()
                                                .gap(px(8.0))
                                                // Opening it is the obvious
                                                // thing to want, and reading a
                                                // 130-character URL off the
                                                // screen to type elsewhere is
                                                // not a task to leave someone.
                                                .child(
                                                    ui::secondary_button(
                                                        Some((
                                                            icon::name::OPEN_IN_NEW,
                                                            0x5F594F,
                                                        )),
                                                        t!("setup.open_link").to_string(),
                                                    )
                                                    .id("open-link")
                                                    .on_click({
                                                        let url = url.clone();
                                                        move |_, _, _| {
                                                            crate::reveal::open_url(&url)
                                                        }
                                                    }),
                                                )
                                                .child(
                                                    ui::secondary_button(
                                                        Some((
                                                            icon::name::CONTENT_COPY,
                                                            0x5F594F,
                                                        )),
                                                        t!("setup.copy_link").to_string(),
                                                    )
                                                    .id("copy-link")
                                                    .on_click(cx.listener(move |_, _, _, cx| {
                                                        cx.write_to_clipboard(
                                                            ClipboardItem::new_string(url.clone()),
                                                        );
                                                    })),
                                                ),
                                        ),
                                ),
                        )
                    })
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(theme::hex(0x857D72))
                            .child(t!("setup.picked_up_next_time").to_string()),
                    ),
            )
            .child(div().flex_1())
            // A download is a wait you are allowed to walk away from.
            .child(
                div()
                    .h_flex()
                    .w_full()
                    .h(px(62.0))
                    .flex_none()
                    .items_center()
                    .gap(px(12.0))
                    .px(px(24.0))
                    .bg(theme::bg_subtle(false))
                    .border_t_1()
                    .border_color(theme::hex(0xEBE4D9))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme::hex(0x6B645A))
                            .child(if self.runtime_done {
                                t!("setup.next_is_a_model").to_string()
                            } else {
                                t!("setup.can_close").to_string()
                            }),
                    )
                    .child(div().flex_1())
                    .child(
                        ui::secondary_button(None, t!("setup.quit").to_string())
                            .id("quit")
                            .on_click(|_, _, cx| cx.quit()),
                    )
                    // The step ends when the person says so, not when the
                    // installer does.
                    .when(self.runtime_done, |d| {
                        d.child(
                            div()
                                .h_flex()
                                .h(px(36.0))
                                .px(px(18.0))
                                .flex_none()
                                .items_center()
                                .rounded(px(8.0))
                                .bg(theme::hex(0xFF6E08))
                                .text_size(px(13.0))
                                .font_semibold()
                                .text_color(theme::hex(0xFFFEFD))
                                .id("setup-continue")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.runtime_done = false;
                                    cx.notify();
                                }))
                                .child(t!("model.continue").to_string()),
                        )
                    }),
            )
    }

    /// One line of the "until it is installed" list: blocked, or still fine.
    fn capability(&self, available: bool, label: String, cx: &Context<Self>) -> AnyElement {
        div()
            .h_flex()
            .items_center()
            .gap(px(9.0))
            .child(icon::icon(
                if available { icon::name::CHECK } else { icon::name::BLOCK },
                17.0,
                if available { cx.theme().success } else { theme::hex(0xB0A79B) },
            ))
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(theme::hex(0x5F594F))
                    .child(label),
            )
            .into_any_element()
    }

}

/// Somewhere sensible for a save panel to open. Falls back to the working
/// directory rather than failing, since the panel is still usable from there.
fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")))
}

/// The brand fonts, bundled rather than assumed present: Sora for headings and
/// Noto Sans for everything else. Noto Sans is not an aesthetic preference —
/// the guideline requires correct rendering of Yorùbá and Ìgbò tone marks, and
/// most system faces drop them.
fn load_brand_fonts(cx: &App) {
    let fonts: Vec<std::borrow::Cow<'static, [u8]>> = vec![
        std::borrow::Cow::Borrowed(include_bytes!("../fonts/Sora-Variable.ttf").as_slice()),
        std::borrow::Cow::Borrowed(include_bytes!("../fonts/NotoSans-Variable.ttf").as_slice()),
        std::borrow::Cow::Borrowed(include_bytes!("../fonts/NotoSansMono.ttf").as_slice()),
        // Ligature icon font: glyphs are selected by writing their names.
        std::borrow::Cow::Borrowed(
            include_bytes!("../fonts/MaterialSymbolsRounded.ttf").as_slice(),
        ),
    ];
    if let Err(err) = cx.text_system().add_fonts(fonts) {
        // Not fatal: the app is legible in a fallback face, just off-brand.
        eprintln!("could not load brand fonts: {err}");
    }
    if std::env::var("YARNGO_FONT_DEBUG").is_ok() {
        let names = cx.text_system().all_font_names();
        eprintln!("font-debug: {} families known", names.len());
        for n in names.iter().filter(|n| {
            let l = n.to_lowercase();
            l.contains("material") || l.contains("sora") || l.contains("noto") || l.contains("yarngo")
        }) {
            eprintln!("font-debug:   {n}");
        }
    }
}

fn main() {
    gpui_platform::application().run(move |cx| {
        gpui_component::init(cx);
        load_brand_fonts(cx);
        // "secondary" is cmd on macOS and ctrl elsewhere, so the binding is
        // right on every platform the app will be built for.
        cx.bind_keys([
            KeyBinding::new("secondary-enter", Speak, None),
            // Scoped to the rename field. Bound globally these swallowed every
            // Return in the composer and every Escape in the app, including
            // the one that dismisses the enrolment sheet.
            KeyBinding::new("enter", CommitRename, Some(RENAME_CONTEXT)),
            KeyBinding::new("escape", CancelRename, Some(RENAME_CONTEXT)),
        ]);
        // Yarngo brand palette, so the desktop app matches the rest of the product.
        theme::apply(gpui_component::ThemeMode::Light, cx);

        cx.spawn(async move |cx| {
            cx.open_window(
                WindowOptions {
                    // Below this the sidebar and composer stop coexisting; the
                    // OS enforces it so the layout never has to cope.
                    window_min_size: Some(size(px(720.0), px(560.0))),
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: point(px(120.0), px(120.0)),
                        size: size(px(1180.0), px(820.0)),
                    })),
                    titlebar: Some(TitlebarOptions {
                        // The custom bar draws the name; a system title would
                        // render it twice.
                        title: None,
                        appears_transparent: true,
                        // Centred against the bar rather than hung from the
                        // top edge: everything else in the bar is centred in
                        // `TITLE_BAR_HEIGHT`, and lights on a different line
                        // read as the title sitting crooked next to them.
                        traffic_light_position: Some(point(
                            px(12.0),
                            px((workspace::TITLE_BAR_HEIGHT - workspace::TRAFFIC_LIGHT_SIZE) / 2.0),
                        )),
                    }),
                    ..Default::default()
                },
                |window, cx| {
                    let view = cx.new(|cx| VoiceStudio::new(window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                },
            )
            .expect("failed to open window");
        })
        .detach();
    });
}
