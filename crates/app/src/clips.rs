//! Clips as the app's unit of work, drafts included.
//!
//! Iteration 4 makes a clip a thing you name and set up, not just the last
//! output. So a clip exists from the moment you start writing: a draft, in the
//! list, with its own voice and model, which becomes a real clip when it is
//! generated. That is what lets "start another clip" mean anything while one is
//! still running, and what gives the sidebar something to show before any audio
//! exists.
//!
//! Text belongs to the draft rather than to the composer, so switching between
//! two of them does not lose what was typed in either.

use gpui::*;
use gpui_component::input::InputState;
use rust_i18n::t;

use crate::VoiceStudio;

/// A clip being written. Everything it needs to generate, so generating is a
/// matter of handing this to the engine.
#[derive(Clone, Debug)]
pub struct Draft {
    pub id: String,
    /// `None` until the user names it — the sidecar names a generated clip from
    /// its own first words, so an unnamed draft has nothing to carry over.
    pub name: Option<String>,
    pub text: String,
    pub voice_id: Option<String>,
    pub model: Option<String>,
    pub seed: Option<u32>,
    /// True while this draft is the one the engine is working on.
    pub generating: bool,
    /// Whether this clip was actually started — by New clip, or by editing a
    /// finished one's words. The draft the app opens on is not: until then
    /// there is no clip yet, only an empty composer, and the list says so with
    /// its own empty state.
    pub started: bool,
}

impl Draft {
    fn new(id: usize, voice_id: Option<String>, model: Option<String>) -> Self {
        Self {
            id: format!("draft-{id}"),
            name: None,
            text: String::new(),
            voice_id,
            model,
            seed: None,
            generating: false,
            started: true,
        }
    }

    /// The empty draft the app opens on, which is not yet a clip.
    pub fn blank() -> Self {
        Self { started: false, ..Self::new(1, None, None) }
    }

    pub fn title(&self) -> String {
        self.name.clone().unwrap_or_else(|| t!("clip.untitled").to_string())
    }
}

/// What the workspace is pointed at. There is always one — an empty draft on
/// first run — because the composer is never showing nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selected {
    Draft(String),
    /// A clip, and which of its takes is being listened to. Generating again
    /// adds a take, so "the clip" is never enough on its own.
    Clip(String, String),
}

impl VoiceStudio {
    /// The draft being written, if the selection is one.
    pub(crate) fn draft(&self) -> Option<&Draft> {
        match &self.selected {
            Selected::Draft(id) => self.drafts.iter().find(|d| &d.id == id),
            Selected::Clip(..) => None,
        }
    }

    pub(crate) fn draft_mut(&mut self) -> Option<&mut Draft> {
        match &self.selected {
            Selected::Draft(id) => {
                let id = id.clone();
                self.drafts.iter_mut().find(|d| d.id == id)
            }
            Selected::Clip(..) => None,
        }
    }

    /// The finished clip being looked at, if the selection is one.
    pub(crate) fn clip(&self) -> Option<&speech_engine::Clip> {
        match &self.selected {
            Selected::Clip(id, _) => self.clips.iter().find(|c| &c.id == id),
            Selected::Draft(_) => None,
        }
    }

    /// The take being listened to: the one selected, or the newest if that one
    /// has gone.
    pub(crate) fn take(&self) -> Option<&speech_engine::Take> {
        let clip = self.clip()?;
        match &self.selected {
            Selected::Clip(_, take) => clip.take(take).or_else(|| clip.latest()),
            Selected::Draft(_) => None,
        }
    }

    /// The voice and model in force for whatever is selected — the draft's own
    /// choice, or the settings the finished clip was made with.
    pub(crate) fn clip_voice(&self) -> Option<&str> {
        match &self.selected {
            Selected::Draft(_) => self.draft().and_then(|d| d.voice_id.as_deref()),
            Selected::Clip(..) => self.clip().and_then(|c| c.voice_id.as_deref()),
        }
    }

    pub(crate) fn clip_model(&self) -> Option<&str> {
        match &self.selected {
            Selected::Draft(_) => self
                .draft()
                .and_then(|d| d.model.as_deref())
                .or(self.selected_model.as_deref()),
            Selected::Clip(..) => self.clip().map(|c| c.model.as_str()),
        }
    }

    /// The name of a voice as the interface says it: the bundled default when
    /// nothing is chosen, otherwise the label the user gave it.
    pub(crate) fn voice_name(&self, voice_id: Option<&str>) -> String {
        match voice_id {
            None => t!("voice.default").to_string(),
            Some(id) => self
                .voices
                .iter()
                .find(|v| v.voice_id == id)
                .map(|v| v.label.clone())
                .unwrap_or_else(|| t!("voice.default").to_string()),
        }
    }

    /// A model this machine can actually generate with.
    ///
    /// What carries over is normally right — the next clip usually wants the
    /// last one's model. But a finished clip holds the model that *made* it,
    /// which may be one this backend has never had: a clip carried over from
    /// another platform, or a model since uninstalled. Carrying that into a new
    /// draft pins it to something that cannot run, and the failure arrives at
    /// Generate rather than here.
    fn usable_model(&self, carried: Option<&str>) -> Option<String> {
        carried
            .filter(|id| self.models.iter().any(|m| m.id == *id))
            .map(str::to_owned)
            .or_else(|| self.selected_model.clone())
    }

    /// Start a clip. The voice and model carry over from what was last used,
    /// because that is nearly always what the next clip wants too.
    pub(crate) fn new_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.next_draft += 1;
        let draft = Draft::new(
            self.next_draft,
            self.clip_voice().map(str::to_owned),
            self.usable_model(self.clip_model()),
        );
        self.selected = Selected::Draft(draft.id.clone());
        self.drafts.insert(0, draft);
        self.text.update(cx, |state, cx| state.set_value("", window, cx));
        self.renaming = None;
        cx.notify();
    }

    /// Point the workspace at a draft, keeping the text that belongs to it.
    pub(crate) fn select_draft(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.save_open_text(cx);
        let text = self
            .drafts
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.text.clone())
            .unwrap_or_default();
        self.selected = Selected::Draft(id);
        self.text.update(cx, |state, cx| state.set_value(text, window, cx));
        self.renaming = None;
        cx.notify();
    }

    /// Point the workspace at a finished clip and load its audio, without
    /// starting it — the design shows a player, not a surprise.
    pub(crate) fn select_clip(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(latest) = self
            .clips
            .iter()
            .find(|c| c.id == id)
            .and_then(|c| c.latest())
            .map(|t| t.id.clone())
        else {
            return;
        };
        self.select_take(id, latest, window, cx);
    }

    /// Point the workspace at one reading of a clip and load its audio, without
    /// starting it — the design shows a player, not a surprise.
    pub(crate) fn select_take(
        &mut self,
        clip_id: String,
        take_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.save_open_text(cx);
        let Some(clip) = self.clips.iter().find(|c| c.id == clip_id).cloned() else { return };
        let Some(take) = clip.take(&take_id).or_else(|| clip.latest()).cloned() else { return };
        self.selected = Selected::Clip(clip_id, take.id);
        self.text.update(cx, |state, cx| state.set_value(clip.text.clone(), window, cx));
        self.renaming = None;
        self.load_clip(&take.path, take.audio_s, cx);
        cx.notify();
    }

    /// Whatever is in the composer belongs to the draft it was typed into.
    /// Typing into the opening draft is what starts it.
    pub(crate) fn save_open_text(&mut self, cx: &mut Context<Self>) {
        let text = self.text.read(cx).value().to_string();
        if let Some(draft) = self.draft_mut() {
            draft.started |= !text.trim().is_empty();
            draft.text = text;
        }
    }

    /// Editing a finished clip's words makes another take rather than altering
    /// the one you have: a new draft, seeded from it, with the clip left alone.
    pub(crate) fn edit_clip_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(clip) = self.clip().cloned() else { return };
        self.next_draft += 1;
        let mut draft = Draft::new(
            self.next_draft,
            clip.voice_id.clone(),
            // The new take would rather use the model that made the original,
            // but only if this machine has it.
            self.usable_model(Some(&clip.model)),
        );
        draft.text = clip.text.clone();
        draft.name = Some(clip.name.clone());
        draft.seed = self.take().and_then(|t| t.seed);
        self.selected = Selected::Draft(draft.id.clone());
        self.drafts.insert(0, draft);
        self.text.update(cx, |state, cx| state.set_value(clip.text, window, cx));
        cx.notify();
    }

    /// Open the name for editing, seeded with what it is called now.
    pub(crate) fn begin_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = match &self.selected {
            Selected::Draft(_) => self.draft().map(Draft::title),
            Selected::Clip(..) => self.clip().map(|c| c.name.clone()),
        };
        let Some(current) = current else { return };
        self.clip_name.update(cx, |state, cx| state.set_value(current, window, cx));
        self.renaming = Some(self.selected.clone());
        self.clip_name.update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    pub(crate) fn commit_rename(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.renaming.take() else { return };
        let name = self.clip_name.read(cx).value().trim().to_string();
        match target {
            Selected::Draft(id) => {
                if let Some(draft) = self.drafts.iter_mut().find(|d| d.id == id) {
                    draft.name = (!name.is_empty()).then_some(name);
                }
                cx.notify();
            }
            Selected::Clip(id, _) => self.rename_clip(id, name, cx),
        }
    }

    pub(crate) fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        if self.renaming.take().is_some() {
            cx.notify();
        }
    }

    fn rename_clip(&mut self, id: String, name: String, cx: &mut Context<Self>) {
        let Some(engine) = self.engine.clone() else { return };
        cx.spawn(async move |this, cx| {
            let clips = cx.background_spawn(async move { engine.rename_clip(id, name) }).await;
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

    /// Bytes the clips occupy, for the sidebar's footer line.
    pub(crate) fn clips_bytes(&self) -> u64 {
        self.clips
            .iter()
            .flat_map(|c| c.takes.iter())
            .filter_map(|t| std::fs::metadata(&t.path).ok())
            .map(|m| m.len())
            .sum()
    }
}

/// A fresh name field for the header's inline rename.
pub(crate) fn name_field(window: &mut Window, cx: &mut App) -> Entity<InputState> {
    cx.new(|cx| InputState::new(window, cx))
}
