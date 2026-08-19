//! Voicestudio — type text, choose a voice, hear it in that voice.
//!
//! Generation takes 15-25 seconds, so it runs on a background executor and the
//! window shows an honest progress state rather than a spinner that implies
//! something faster. Everything below `EngineHandle` is backend-agnostic.

mod enrolment;
mod models;
mod settings;
mod switcher;
mod icon;
mod ui;
mod player;
mod recorder;
mod theme;
mod workspace;

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

rust_i18n::i18n!("locales", fallback = "en");

actions!(voicestudio, [Speak]);

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
    Saving,
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
const CLIP_BARS: usize = 34;

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
    /// What the running generation has written so far, polled from the engine
    /// while it works. `None` between generations.
    pub(crate) progress: Option<runtime::Progress2>,
    /// Seconds of speech this generation is expected to produce, from the word
    /// count. An estimate, and labelled as one — the exact length is not known
    /// until the model has finished.
    pub(crate) expected_s: f32,
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
            progress: None,
            expected_s: 0.0,
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
            in_setup: false,
        };
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
                    let handle = EngineHandle::spawn(&paths.python, &paths.script, &paths.work_dir)?;
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
                        // A voice from an earlier session is the selection on
                        // sight. With none, the model's own voice is, and
                        // enrolment is offered rather than demanded.
                        this.selected_voice = voices.first().map(|v| v.voice_id.clone());
                        let first_run = voices.is_empty();
                        this.voices = voices;
                        this.engine = Some(handle);
                        this.status = Status::Idle;
                        this.refresh_clips(cx);
                        this.refresh_system(cx);
                        if first_run {
                            this.open_recorder(cx);
                        } else {
                            this.warm_selected_voice(cx);
                        }
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
                    let (samples, rate) = recorder::load_wav(&path)?;
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

        self.enrolment = Enrolment::Saving;
        self.status = Status::Preparing(
            t!("enrol.learning_status").to_string(),
        );
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
                match result {
                    Ok(voices) => {
                        // Select the voice just enrolled, not whichever sorts first.
                        this.selected_voice = voices
                            .iter()
                            .find(|v| v.voice_id == enrolled_id)
                            .or_else(|| voices.first())
                            .map(|v| v.voice_id.clone());
                        this.voices = voices;
                        this.enrolment = Enrolment::Closed;
                        this.in_setup = false;
                        this.recorder = None;
                        this.consent_given = false;
                        this.imported = None;
                        this.clear_take();
                        this.status = Status::Idle;
                    }
                    Err(err) => {
                        this.enrolment = Enrolment::Rejected(format!("{err}"));
                        this.status = Status::Idle;
                    }
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
    pub(crate) fn generate(&mut self, seed: Option<u32>, cx: &mut Context<Self>) {
        let seed = seed.or(self.pinned_seed);
        let Some(engine) = self.engine.clone() else { return };
        if self.busy() {
            return;
        }
        // Nothing can speak yet. Keep the words and run them when it can.
        if !self.model_ready() {
            let text = self.text.read(cx).value().to_string();
            if !text.trim().is_empty() {
                self.queued = Some(text);
                cx.notify();
            }
            return;
        }
        let text = self.text.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }

        let request = SynthesisRequest {
            text,
            output: std::env::temp_dir().join("voicestudio_output.wav"),
            model: self.selected_model.clone(),
            // `None` is a real choice, not a missing one: it asks the model to
            // speak in its own voice rather than clone.
            voice_id: self.speaking_voice().map(str::to_owned),
            seed,
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
                if let Err(err) = &result {
                    if matches!(err, speech_engine::EngineError::NotRunning) {
                        this.engine_failed(speech_engine::EngineError::NotRunning, cx);
                        return;
                    }
                }
                this.status = match result {
                    Ok(s) => {
                        // The stored clip, not the scratch file it was written
                        // to: the sidebar lists the stored path, and loading the
                        // other one leaves the row that is playing unmarked.
                        let source = s
                            .clip
                            .as_ref()
                            .map(|c| c.path.clone())
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
                    Err(err) => Status::Failed(format!("{err}")),
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
        runtime::request_cancel();
        cx.notify();
    }

    /// Poll what the engine has written while it works. The loop ends with the
    /// generation, so an idle app is not reading a file ten times a second.
    fn tick_progress(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(150)).await;
            let reported = cx.background_spawn(async move { runtime::read_progress() }).await;
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
        self.clip_levels = recorder::load_wav(path)
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

    /// The clip, as screen 1f draws it: one card holding the transport, the
    /// waveform, and everything you would do with a finished take. The
    /// waveform is the seek track — checking one word in a 24 second clip
    /// should not mean listening to the 23 seconds before it.
    pub(crate) fn player_card(&self, cx: &mut Context<Self>) -> AnyElement {
        // The enrolment take borrows the same player. It belongs to the sheet
        // that is reviewing it, not to the workspace behind.
        if self.take_loaded() {
            return div().into_any_element();
        }
        let Some((path, _)) = self.clip.clone() else {
            return div().into_any_element();
        };
        let Some(player) = self.player.as_ref() else {
            return div()
                .text_size(px(12.5))
                .text_color(theme::hex(0x6B645A))
                .child(t!("player.no_device").to_string())
                .into_any_element();
        };

        let playing = player.is_playing();
        let progress = player.progress();
        let elapsed = format_time(player.position());
        let total = format_time(player.duration());
        let copying = path.clone();

        ui::card()
            .w_full()
            .flex_none()
            .child(
                div()
                    .h_flex()
                    .w_full()
                    .items_center()
                    .gap(px(14.0))
                    .px(px(16.0))
                    .py(px(15.0))
                    .child(
                        ui::play_button(playing, true)
                            .id("play")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_playback(cx))),
                    )
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_w(px(0.0))
                            .child(ui::waveform(
                                &self.clip_levels,
                                progress,
                                56.0,
                                theme::hex(0xFF8A1F),
                                theme::hex(0xE4DCD0),
                            ))
                            .child({
                                let track = self.track.clone();
                                canvas(move |bounds, _, _| track.set(bounds), |_, _, _, _| {})
                                    .absolute()
                                    .size_full()
                            })
                            .id("seek")
                            .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                                let Some(player) = this.player.as_ref() else { return };
                                let bounds = this.track.get();
                                if bounds.size.width <= px(0.0) {
                                    return;
                                }
                                let x = event.position().x - bounds.origin.x;
                                let fraction: f32 = (x / bounds.size.width).into();
                                if let Err(err) = player.seek_to(fraction) {
                                    this.status = Status::Failed(err);
                                }
                                this.tick_playback(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        ui::mono(format!("{elapsed} / {total}"), 12.0, theme::hex(0x6B645A))
                            .flex_none(),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .w_full()
                    .items_center()
                    .gap(px(9.0))
                    .px(px(16.0))
                    .pb(px(15.0))
                    .child(self.save_as_button(cx))
                    .when_some(self.last.clone(), |this, last| {
                        let seed = last.seed;
                        this.child(
                            // The seed carries over, so editing the words and
                            // pressing this changes only the words.
                            ui::secondary_button(
                                Some((icon::name::REFRESH, 0x5F594F)),
                                t!("clip.again").to_string(),
                            )
                            .id("again")
                            .on_click(cx.listener(move |this, _, _, cx| this.generate(seed, cx))),
                        )
                    })
                    .child(
                        ui::secondary_button(
                            Some((icon::name::CONTENT_COPY, 0x5F594F)),
                            t!("clip.copy").to_string(),
                        )
                        .id("copy-audio")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.copy_clip(&copying, cx)
                        })),
                    )
                    .child(div().flex_1())
                    .when_some(self.last.clone(), |this, last| {
                        this.child(
                            ui::mono(self.clip_facts(&last), 11.5, theme::hex(0x857D72)).flex_none(),
                        )
                    }),
            )
            .into_any_element()
    }

    /// Saving is the primary action on a finished clip, so it is the only
    /// filled button in the row.
    fn save_as_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some((source, _)) = self.clip.clone() else {
            return div().into_any_element();
        };
        div()
            .h_flex()
            .h(px(34.0))
            .px(px(14.0))
            .gap(px(7.0))
            .flex_none()
            .items_center()
            .rounded(px(8.0))
            .bg(theme::hex(0xFF6E08))
            .text_size(px(12.5))
            .font_semibold()
            .text_color(theme::hex(0xFFFEFD))
            .child(icon::icon(icon::name::DOWNLOAD, 17.0, theme::hex(0xFFFEFD)))
            .child(t!("clip.save_as").to_string())
            .id("save-as")
            .on_click(cx.listener(move |this, _, window, cx| {
                this.save_clip_as(source.clone(), window, cx)
            }))
            .into_any_element()
    }

    /// What made this clip, in the order the design states it: which model,
    /// which seed, how long it took, and how that compares to playing it.
    fn clip_facts(&self, last: &Synthesis) -> String {
        let model = self
            .models
            .iter()
            .find(|m| m.id == last.model)
            .map(|m| m.label.clone())
            .unwrap_or_else(|| last.model.clone());
        let made = t!(
            "workspace.made_with",
            model = model,
            seed = last.seed.map(|s| s.to_string()).unwrap_or_default(),
            gen = format!("{:.1}", last.gen_s)
        )
        .to_string();
        match last.rtf {
            Some(rtf) => format!(
                "{made} · {}",
                t!("workspace.realtime", rtf = workspace::realtime(rtf))
            ),
            None => made,
        }
    }

    /// Put the clip on the clipboard as a file, which is what "copy audio"
    /// means everywhere else on this machine: it pastes into Finder, Mail and
    /// Messages as the recording itself rather than as its path.
    fn copy_clip(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        let script = format!(
            "set the clipboard to (POSIX file \"{}\")",
            path.display().to_string().replace('\\', "\\\\").replace('"', "\\\"")
        );
        if let Err(err) = std::process::Command::new("osascript").arg("-e").arg(script).status() {
            self.status = Status::Failed(err.to_string());
            cx.notify();
        }
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
                        // Keep the selection valid after a removal.
                        if !voices.iter().any(|v| Some(&v.voice_id) == this.selected_voice.as_ref())
                        {
                            this.selected_voice = voices.first().map(|v| v.voice_id.clone());
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
        if matches!(self.status, Status::Installing { .. })
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

    pub(crate) fn installed_gb(&self) -> f32 {
        self.models.iter().filter(|m| m.installed).map(|m| m.size_bytes as f32).sum::<f32>() / 1e9
    }

    pub(crate) fn model_ready(&self) -> bool {
        self.selected_model
            .as_deref()
            .and_then(|id| self.models.iter().find(|m| m.id == id))
            .is_none_or(|m| m.installed)
    }

    pub(crate) fn busy(&self) -> bool {
        matches!(
            self.status,
            Status::Preparing(_) | Status::Generating | Status::Installing { .. }
        )
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
        };
        div().text_size(px(12.0)).text_color(colour).child(text).into_any_element()
    }

}

/// Shorten a quote to fit a card without cutting mid-word where avoidable.
pub(crate) fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    match cut.rsplit_once(' ') {
        Some((head, _)) if head.len() > max / 2 => format!("{head}…"),
        _ => format!("{cut}…"),
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
            .child(self.title_bar(window, cx))
            .child(match screen {
                Screen::Setup => self.setup_screen(cx).into_any_element(),
                Screen::Models => self.models_screen(window, cx).into_any_element(),
                Screen::Enrolment => self.enrolment_screen(window, cx).into_any_element(),
                Screen::Workspace => div()
                    .h_flex()
                    .flex_1()
                    .min_h(px(0.0))
                    .child(self.sidebar(cx))
                    .child(self.composer(cx))
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
            .when(self.enrolling_over_workspace(), |this| {
                this.child(self.enrolment_sheet(window, cx))
            })
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
                                    .when(!started, |d| {
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
                                                    .child(self.capability(true, t!("setup.cap_record").to_string(), cx))
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
                                                    t!("setup.no_connection_detail").to_string(),
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
                                            ui::secondary_button(
                                                Some((icon::name::CONTENT_COPY, 0x5F594F)),
                                                t!("setup.copy_link").to_string(),
                                            )
                                            .flex_none()
                                            .id("copy-link")
                                            .on_click(cx.listener(move |_, _, _, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    url.clone(),
                                                ));
                                            })),
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
                            .child(t!("setup.can_close").to_string()),
                    )
                    .child(div().flex_1())
                    .child(
                        ui::secondary_button(None, t!("setup.quit").to_string())
                            .id("quit")
                            .on_click(|_, _, cx| cx.quit()),
                    ),
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
}

fn main() {
    gpui_platform::application().run(move |cx| {
        gpui_component::init(cx);
        load_brand_fonts(cx);
        // "secondary" is cmd on macOS and ctrl elsewhere, so the binding is
        // right on every platform the app will be built for.
        cx.bind_keys([KeyBinding::new("secondary-enter", Speak, None)]);
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
                        traffic_light_position: Some(point(px(12.0), px(16.0))),
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
