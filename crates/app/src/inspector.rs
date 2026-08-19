//! The right panel: everything about one clip or one voice.
//!
//! The sidebar rows are deliberately thin — a title and a duration — because a
//! list is for finding things, not for reading them. What a clip was actually
//! made from (which model, which seed, how long it took) and what a voice was
//! learned from (the script, the recording, the permission that was given) has
//! to live somewhere, and a modal is the wrong somewhere: you look at these
//! while you work, not instead of working.
//!
//! It is closable and it follows the selection. Clicking a row in the sidebar
//! opens it on that row; the × puts it away.

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::StyledExt;
use rust_i18n::t;
use speech_engine::{Clip, Voice};

use crate::workspace::duration;
use crate::{icon, theme, ui, VoiceStudio};

/// Matches the 280px right column the setup screen uses, so the two panels in
/// the app are the same width.
const PANEL_WIDTH: f32 = 280.0;

/// What the panel is looking at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inspect {
    Clip(String),
    Voice(String),
}

impl VoiceStudio {
    /// One `LABEL / value` pair. The label is the mono section face, so a
    /// column of these reads as a record rather than as prose.
    fn field(label: String, value: String) -> Div {
        div()
            .v_flex()
            .w_full()
            .gap(px(3.0))
            .child(ui::section_label(label.to_uppercase()))
            .child(
                div()
                    .text_size(px(12.5))
                    .line_height(px(19.0))
                    .text_color(theme::hex(0x171717))
                    .child(value),
            )
    }

    /// A path, in mono, with the way to open the folder it is in. Shown because
    /// "where did that file go" should not need a support article.
    fn path_field(&self, label: String, path: std::path::PathBuf) -> Div {
        let open = path.clone();
        div()
            .v_flex()
            .w_full()
            .gap(px(3.0))
            .child(ui::section_label(label.to_uppercase()))
            .child(
                div()
                    .h_flex()
                    .w_full()
                    .items_start()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .child(ui::mono(
                                path.display().to_string(),
                                11.0,
                                theme::hex(0x6B645A),
                            )),
                    )
                    .child(
                        div()
                            .flex_none()
                            .child(icon::icon(icon::name::OPEN_IN_NEW, 15.0, theme::hex(0x857D72)))
                            .id("inspect-reveal")
                            .on_click(move |_, _, _| {
                                let _ = std::process::Command::new("open")
                                    .arg("-R")
                                    .arg(&open)
                                    .spawn();
                            }),
                    ),
            )
    }

    /// What produced this clip, in the order you would ask: the words, then the
    /// voice and model, then the seed that would reproduce it.
    fn clip_body(&self, clip: &Clip, cx: &mut Context<Self>) -> Div {
        let voice = clip
            .voice_id
            .as_deref()
            .and_then(|id| self.voices.iter().find(|v| v.voice_id == id))
            .map(|v| v.label.clone())
            .unwrap_or_else(|| t!("voice.sample").to_string());
        let model = self
            .models
            .iter()
            .find(|m| m.id == clip.model)
            .map(|m| m.label.clone())
            .unwrap_or_else(|| clip.model.clone());

        let mut made = vec![
            duration(clip.audio_s),
            t!("inspect.took", seconds = format!("{:.1}", clip.gen_s)).to_string(),
        ];
        if clip.gen_s > 0.0 && clip.audio_s > 0.0 {
            made.push(
                t!(
                    "workspace.realtime",
                    rtf = crate::workspace::realtime(clip.gen_s / clip.audio_s)
                )
                .to_string(),
            );
        }

        let id = clip.id.clone();
        div()
            .v_flex()
            .w_full()
            .gap(px(16.0))
            .child(Self::field(t!("inspect.words").to_string(), clip.text.clone()))
            .child(Self::field(t!("inspect.spoken_by").to_string(), format!("{voice} · {model}")))
            .child(
                div()
                    .v_flex()
                    .w_full()
                    .gap(px(3.0))
                    .child(ui::section_label(t!("inspect.made").to_string().to_uppercase()))
                    .child(ui::mono(made.join(" · "), 11.5, theme::hex(0x6B645A))),
            )
            // The seed is the one number that makes a take repeatable, so it is
            // also the one worth pinning from here.
            .when_some(clip.seed, |this, seed| {
                let pinned = self.pinned_seed == Some(seed);
                this.child(
                    div()
                        .v_flex()
                        .w_full()
                        .gap(px(6.0))
                        .child(ui::section_label(t!("inspect.seed").to_string().to_uppercase()))
                        .child(ui::mono(seed.to_string(), 12.5, theme::hex(0x171717)))
                        .child(
                            ui::secondary_button(
                                None,
                                if pinned {
                                    t!("inspect.unpin_seed").to_string()
                                } else {
                                    t!("inspect.pin_seed").to_string()
                                },
                            )
                            .h(px(30.0))
                            .px(px(11.0))
                            .rounded(px(7.0))
                            .text_size(px(12.0))
                            .id("inspect-pin")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.pinned_seed =
                                    (this.pinned_seed != Some(seed)).then_some(seed);
                                cx.notify();
                            })),
                        ),
                )
            })
            .child(self.path_field(t!("inspect.file").to_string(), clip.path.clone()))
            .child(div().flex_1())
            .child(
                ui::secondary_button(None, t!("inspect.delete_clip").to_string())
                    .w_full()
                    .h(px(34.0))
                    .justify_center()
                    .text_color(theme::hex(0xC7362B))
                    .id("inspect-delete-clip")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.inspector = None;
                        this.delete_clip(id.clone(), cx);
                    })),
            )
    }

    /// What this voice was learned from, and what was agreed to when it was.
    fn voice_body(&self, voice: &Voice, cx: &mut Context<Self>) -> Div {
        let clips = self
            .clips
            .iter()
            .filter(|c| c.voice_id.as_deref() == Some(voice.voice_id.as_str()))
            .count();
        let reference = voice.reference_audio.clone();

        div()
            .v_flex()
            .w_full()
            .gap(px(16.0))
            .child(
                div()
                    .v_flex()
                    .w_full()
                    .gap(px(3.0))
                    .child(ui::section_label(t!("inspect.reference").to_string().to_uppercase()))
                    .child(ui::mono(
                        if voice.seconds > 0.0 {
                            format!(
                                "{} · {}",
                                duration(voice.seconds),
                                t!("inspect.made_clips", count = clips)
                            )
                        } else {
                            t!("inspect.made_clips", count = clips).to_string()
                        },
                        11.5,
                        theme::hex(0x6B645A),
                    )),
            )
            .child(
                ui::secondary_button(
                    Some((icon::name::PLAY_ARROW, 0x5F594F)),
                    t!("workspace.hear_reference").to_string(),
                )
                .w_full()
                .h(px(32.0))
                .justify_center()
                .text_size(px(12.0))
                .id("inspect-hear")
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.load_clip(&reference, 0.0, cx);
                    this.play_loaded(cx);
                })),
            )
            // The words it was learned from. A voice is the pair, and this is
            // the half that is otherwise invisible.
            .child(Self::field(
                t!("enrol.reference_text").to_string(),
                voice.reference_text.clone(),
            ))
            // Not decoration: a cloned voice is a likeness, and this is the
            // claim that was made about it, in the wording that was shown.
            .when(!voice.consent.statement.is_empty(), |this| {
                this.child(
                    div()
                        .v_flex()
                        .w_full()
                        .gap(px(5.0))
                        .px(px(12.0))
                        .py(px(11.0))
                        .rounded(px(10.0))
                        .bg(theme::surface(false))
                        .border_1()
                        .border_color(theme::hex(0xEBE4D9))
                        .child(
                            div()
                                .h_flex()
                                .items_center()
                                .gap(px(7.0))
                                .child(icon::icon(icon::name::LOCK, 14.0, theme::hex(0x857D72)))
                                .child(ui::section_label(
                                    t!("inspect.consent").to_string().to_uppercase(),
                                )),
                        )
                        .child(
                            div()
                                .text_size(px(11.5))
                                .line_height(px(17.0))
                                .text_color(theme::hex(0x5F594F))
                                .child(voice.consent.statement.clone()),
                        )
                        .child(ui::mono(
                            format!("{} · {}", voice.consent.source, voice.consent.app_version),
                            11.0,
                            theme::hex(0x857D72),
                        )),
                )
            })
            .child(self.path_field(t!("inspect.file").to_string(), voice.reference_audio.clone()))
            .child(div().flex_1())
    }

    /// The panel. Returns nothing at all when closed, so the workspace keeps
    /// the full width it is designed for.
    pub(crate) fn inspector_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(looking_at) = self.inspector.clone() else {
            return div().into_any_element();
        };

        let (title, body) = match &looking_at {
            Inspect::Clip(id) => match self.clips.iter().find(|c| &c.id == id).cloned() {
                Some(clip) => (clip.title.clone(), self.clip_body(&clip, cx)),
                None => return div().into_any_element(),
            },
            Inspect::Voice(id) => match self.voices.iter().find(|v| &v.voice_id == id).cloned() {
                Some(voice) => (voice.label.clone(), self.voice_body(&voice, cx)),
                None => return div().into_any_element(),
            },
        };

        div()
            .v_flex()
            .w(px(PANEL_WIDTH))
            .h_full()
            .flex_none()
            .bg(theme::hex(0xFFFDFA))
            .border_l_1()
            .border_color(theme::hex(0xEBE4D9))
            .child(
                div()
                    .h_flex()
                    .w_full()
                    .flex_none()
                    .items_start()
                    .gap(px(10.0))
                    .px(px(16.0))
                    .pt(px(18.0))
                    .pb(px(12.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .font_family(theme::FONT_DISPLAY)
                            .text_size(px(14.0))
                            .line_height(px(20.0))
                            .font_semibold()
                            .child(title),
                    )
                    .child(
                        div()
                            .flex_none()
                            .child(icon::icon(icon::name::CLOSE, 18.0, theme::hex(0x5F594F)))
                            .id("close-inspector")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.inspector = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_h(px(0.0))
                    .w_full()
                    .px(px(16.0))
                    .pb(px(18.0))
                    .id("inspector-scroll")
                    .overflow_y_scroll()
                    .child(body),
            )
            .into_any_element()
    }
}
