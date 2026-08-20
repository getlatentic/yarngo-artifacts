//! The right panel: the settings that belong to the clip being made.
//!
//! A voice is not something you browse next to your work — it is a property of
//! this clip, like the model and the seed, so all three live together here
//! rather than in the sidebar. Closed by default, because the composer header
//! already states the voice and the model; the chips there open it.
//!
//! Everything in this panel writes to the selected draft. A finished clip is
//! shown read-only: its settings are what produced it, and changing them would
//! be describing it wrongly rather than changing it.

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::StyledExt;
use rust_i18n::t;
use speech_engine::Voice;

use crate::clips::Selected;
use crate::workspace::duration;
use crate::{icon, theme, ui, VoiceStudio};

/// 300px, as the design draws it.
const PANEL_WIDTH: f32 = 300.0;

impl VoiceStudio {
    /// One voice, chosen or not. The bundled default and a recorded one are the
    /// same row with different words under the name.
    fn pick_voice_row(
        &self,
        id: Option<&str>,
        name: String,
        detail: String,
        chosen: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pick = id.map(str::to_owned);
        let hear = id.map(str::to_owned);
        div()
            .h_flex()
            .w_full()
            .h(px(44.0))
            .items_center()
            .gap(px(10.0))
            .p(px(8.0))
            .rounded(px(8.0))
            .when(chosen, |d| d.bg(theme::hex(0xFFF3E6)))
            .child(if chosen {
                icon::filled(icon::name::CHECK_CIRCLE, 18.0, theme::hex(0x8F4406))
                    .into_any_element()
            } else {
                icon::icon(icon::name::RADIO_UNCHECKED, 18.0, theme::hex(0xB0A79B))
                    .into_any_element()
            })
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_w(px(0.0))
                    .child(
                        div()
                            .text_size(px(12.5))
                            .truncate()
                            .when(chosen, |d| d.font_semibold())
                            .when(!chosen, |d| d.font_medium())
                            .child(name),
                    )
                    .child(
                        ui::mono(
                            detail,
                            11.0,
                            if chosen { theme::hex(0x6B645A) } else { theme::hex(0x857D72) },
                        )
                        .mt(px(1.0))
                        .truncate(),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .child(icon::icon(
                        icon::name::PLAY_CIRCLE,
                        17.0,
                        if chosen { theme::hex(0x8F4406) } else { theme::hex(0x857D72) },
                    ))
                    .id(SharedString::from(format!(
                        "hear-{}",
                        hear.clone().unwrap_or_else(|| "default".into())
                    )))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.hear_voice(hear.clone(), cx)
                    })),
            )
            .id(SharedString::from(format!(
                "pick-{}",
                pick.clone().unwrap_or_else(|| "default".into())
            )))
            .on_click(cx.listener(move |this, _, _, cx| this.choose_voice(pick.clone(), cx)))
            .into_any_element()
    }

    /// The voice section: the bundled default first, then yours, then the way
    /// to add one. Default first because it is the one that needs nothing.
    fn voice_section(&self, cx: &mut Context<Self>) -> Div {
        let chosen = self.clip_voice().map(str::to_owned);
        let voices: Vec<Voice> = self.voices.clone();
        let clone_ok = self.model_can_clone();

        div()
            .v_flex()
            .w_full()
            .gap(px(7.0))
            .child(ui::section_label(t!("voice.label").to_string().to_uppercase()))
            .child(self.pick_voice_row(
                None,
                t!("voice.default").to_string(),
                t!("voice.default_detail").to_string(),
                chosen.is_none(),
                cx,
            ))
            .when(!voices.is_empty(), |d| {
                d.child(ui::section_label(
                    t!("voice.yours", count = voices.len()).to_string().to_uppercase(),
                ))
            })
            .children(voices.iter().map(|voice| {
                self.pick_voice_row(
                    Some(&voice.voice_id),
                    voice.label.clone(),
                    if voice.seconds > 0.0 {
                        t!("workspace.reference_of", time = duration(voice.seconds)).to_string()
                    } else {
                        t!("workspace.reference_len").to_string()
                    },
                    chosen.as_deref() == Some(voice.voice_id.as_str()),
                    cx,
                )
            }))
            // Adding a voice starts from the clip that wants it, which is what
            // makes "saved and selected for this clip" the natural next step.
            .when(clone_ok, |d| {
                d.child(
                    div()
                        .h_flex()
                        .w_full()
                        .h(px(40.0))
                        .items_center()
                        .gap(px(10.0))
                        .p(px(8.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_dashed()
                        .border_color(theme::hex(0xC4BBAE))
                        .child(icon::icon(icon::name::MIC, 17.0, theme::hex(0x8F4406)))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .text_size(px(12.5))
                                .font_semibold()
                                .child(t!("voice.record_one").to_string()),
                        )
                        .child(ui::mono(
                            t!("voice.about_a_minute").to_string(),
                            11.0,
                            theme::hex(0x857D72),
                        ))
                        .id("record-voice")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.begin_enrolment(window, cx)
                        })),
                )
            })
            // One clipped line, as the design sets it — it explains the choice
            // rather than arguing for it, so it must not push MODEL down.
            .when_some(self.voice_note(chosen.as_deref(), clone_ok), |d, note| {
                d.child(
                    div()
                        .w_full()
                        .text_size(px(11.5))
                        .line_height(px(17.0))
                        .text_color(theme::hex(0x6B645A))
                        .mt(px(2.0))
                        .truncate()
                        .child(note),
                )
            })
    }

    /// What to say under the voice list, if anything. The design only explains
    /// the cases that need it.
    fn voice_note(&self, chosen: Option<&str>, clone_ok: bool) -> Option<String> {
        if !clone_ok {
            return Some(t!("voice.model_cannot_clone").to_string());
        }
        match chosen {
            None => Some(t!("voice.default_explains").to_string()),
            Some(id) => self
                .voices
                .iter()
                .find(|v| v.voice_id == id)
                .filter(|v| v.seconds > 0.0)
                .map(|v| {
                    t!("voice.reference_explains", time = duration(v.seconds)).to_string()
                }),
        }
    }

    /// The model, and the one thing about it that changes what you can do.
    fn model_section(&self, cx: &mut Context<Self>) -> Div {
        div()
            .v_flex()
            .w_full()
            .gap(px(7.0))
            .child(ui::section_label(t!("model.label").to_string().to_uppercase()))
            .child(
                div()
                    .h_flex()
                    .w_full()
                    .h(px(36.0))
                    .items_center()
                    .justify_between()
                    .pl(px(12.0))
                    .pr(px(10.0))
                    .rounded(px(8.0))
                    .bg(theme::surface(false))
                    .border_1()
                    .border_color(theme::hex(0xD8D0C4))
                    .text_size(px(12.5))
                    .font_medium()
                    .child(self.model_label())
                    .child(icon::icon(icon::name::EXPAND_MORE, 18.0, theme::hex(0x857D72)))
                    .id("inspect-model")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.model_menu = true;
                        this.refresh_models(cx);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap(px(7.0))
                    .text_size(px(11.5))
                    .text_color(theme::hex(0x5F594F))
                    .text_color(if self.model_can_clone() {
                        theme::hex(0x5F594F)
                    } else {
                        theme::hex(0x8F4406)
                    })
                    .child(icon::icon(
                        if self.model_can_clone() {
                            icon::name::CHECK
                        } else {
                            icon::name::LOCK
                        },
                        15.0,
                        if self.model_can_clone() {
                            theme::hex(0x287A57)
                        } else {
                            theme::hex(0xB98A55)
                        },
                    ))
                    .child(
                        if self.model_can_clone() {
                            t!("model.uses_recorded")
                        } else {
                            t!("model.own_voices_only")
                        }
                        .to_string(),
                    ),
            )
    }

    /// Delivery. Only the seed: the engine takes one, and it is the number that
    /// decides whether a second press repeats a take or draws a new one.
    fn delivery_section(&self, cx: &mut Context<Self>) -> Div {
        let seed = match &self.selected {
            Selected::Draft(_) => self.draft().and_then(|d| d.seed),
            Selected::Clip(..) => self.take().and_then(|t| t.seed),
        };

        div()
            .v_flex()
            .w_full()
            .gap(px(9.0))
            .child(ui::section_label(t!("clip.delivery").to_string().to_uppercase()))
            .child(
                div()
                    .h_flex()
                    .w_full()
                    .items_center()
                    .gap(px(10.0))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(12.5))
                            .text_color(theme::hex(0x5F594F))
                            .child(t!("clip.seed").to_string()),
                    )
                    .child(
                        div()
                            .h_flex()
                            .h(px(28.0))
                            .px(px(10.0))
                            .gap(px(7.0))
                            .items_center()
                            .rounded(px(7.0))
                            .bg(theme::surface(false))
                            .border_1()
                            .border_color(theme::hex(0xD8D0C4))
                            .font_family(theme::FONT_MONO)
                            .text_size(px(12.0))
                            .child(match seed {
                                Some(seed) => seed.to_string(),
                                None => t!("clip.seed_fresh").to_string(),
                            })
                            .child(icon::icon(icon::name::CASINO, 15.0, theme::hex(0x5F594F)))
                            .id("reroll-seed")
                            .on_click(cx.listener(|this, _, _, cx| this.reroll_seed(cx))),
                    ),
            )
    }

    /// A `label  value` line, which is how the record of a finished clip reads.
    fn record_line(label: String, value: String) -> Div {
        div()
            .h_flex()
            .w_full()
            .items_center()
            .gap(px(10.0))
            .p(px(2.0))
            .child(
                div()
                    .flex_1()
                    .text_size(px(12.5))
                    .text_color(theme::hex(0x5F594F))
                    .child(label),
            )
            .child(ui::mono(value, 12.0, theme::hex(0x171717)))
    }

    /// What produced this clip. Read-only: these are not settings any more,
    /// they are the description of something that already exists.
    fn made_with_section(&self, clip: &speech_engine::Clip, cx: &mut Context<Self>) -> Div {
        let voice = clip.voice_id.clone();
        let seed = self.take().and_then(|t| t.seed);

        div()
            .v_flex()
            .w_full()
            .gap(px(7.0))
            .child(ui::section_label(t!("clip.made_with").to_string().to_uppercase()))
            .child(
                div()
                    .h_flex()
                    .w_full()
                    .h(px(44.0))
                    .items_center()
                    .gap(px(10.0))
                    .p(px(8.0))
                    .rounded(px(8.0))
                    .bg(theme::surface(false))
                    .border_1()
                    .border_color(theme::hex(0xEBE4D9))
                    .child(icon::icon(icon::name::RECORD_VOICE, 18.0, theme::hex(0x5F594F)))
                    .child(
                        div()
                            .v_flex()
                            .flex_1()
                            .min_w(px(0.0))
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .font_semibold()
                                    .truncate()
                                    .child(self.voice_name(clip.voice_id.as_deref())),
                            )
                            .child(
                                ui::mono(
                                    match clip.voice_id.as_deref() {
                                        None => t!("voice.bundled").to_string(),
                                        Some(_) => t!("voice.recorded").to_string(),
                                    },
                                    11.0,
                                    theme::hex(0x6B645A),
                                )
                                .mt(px(1.0)),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .child(icon::icon(
                                icon::name::PLAY_CIRCLE,
                                17.0,
                                theme::hex(0x5F594F),
                            ))
                            .id("made-with-hear")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.hear_voice(voice.clone(), cx)
                            })),
                    ),
            )
            .child(Self::record_line(t!("model.label").to_string(), self.model_label()))
            // The clip still names the model that made it, but this machine
            // cannot offer it — uninstalled, or a catalogue from another
            // platform's backend. Say so, rather than showing a name that
            // looks selectable and is not.
            .when(!self.model_available(), |d| {
                d.child(
                    div()
                        .text_size(px(11.5))
                        .line_height(px(17.0))
                        .text_color(theme::hex(0x6B645A))
                        .child(t!("clip.model_unavailable").to_string()),
                )
            })
            .when_some(seed, |d, seed| {
                d.child(
                    Self::record_line(t!("clip.seed").to_string(), seed.to_string()).child(
                        // The one setting worth carrying forward: it makes the
                        // next clip a different reading of the same voice.
                        ui::secondary_button(None, t!("clip.reuse_seed").to_string())
                            .h(px(24.0))
                            .px(px(8.0))
                            .rounded(px(6.0))
                            .text_size(px(11.0))
                            .id("reuse-seed")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.pinned_seed = Some(seed);
                                cx.notify();
                            })),
                    ),
                )
            })
    }

    /// Every reading of this clip. Generating again adds one rather than making
    /// a second clip, so this is where they are compared and chosen between.
    fn takes_section(&self, clip: &speech_engine::Clip, cx: &mut Context<Self>) -> Div {
        let total = clip.takes.len();
        let listening = self.take().map(|t| t.id.clone());

        div()
            .v_flex()
            .w_full()
            .gap(px(7.0))
            .child(ui::section_label(t!("clip.takes").to_string().to_uppercase()))
            .children(clip.takes.iter().enumerate().map(|(index, take)| {
                // Numbered oldest-first the way a person counts them, while the
                // list stays newest-first.
                let number = total - index;
                let chosen = listening.as_deref() == Some(take.id.as_str());
                let (clip_id, take_id) = (clip.id.clone(), take.id.clone());
                let facts = match take.seed {
                    Some(seed) => format!(
                        "{} · {}",
                        duration(take.audio_s),
                        t!("clip.take_seed", seed = seed)
                    ),
                    None => duration(take.audio_s),
                };

                div()
                    .h_flex()
                    .w_full()
                    .h(px(44.0))
                    .items_center()
                    .gap(px(10.0))
                    .p(px(8.0))
                    .rounded(px(8.0))
                    .when(chosen, |d| d.bg(theme::hex(0xFFF3E6)))
                    .child(if chosen {
                        icon::filled(icon::name::CHECK_CIRCLE, 17.0, theme::hex(0x8F4406))
                            .into_any_element()
                    } else {
                        icon::icon(icon::name::RADIO_UNCHECKED, 17.0, theme::hex(0xB0A79B))
                            .into_any_element()
                    })
                    .child(
                        div()
                            .v_flex()
                            .flex_1()
                            .min_w(px(0.0))
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .when(chosen, |d| d.font_semibold())
                                    .when(!chosen, |d| d.font_medium())
                                    .truncate()
                                    .child(t!(
                                        "clip.take_line",
                                        number = number,
                                        at = crate::workspace::clock(&take.created)
                                    )
                                    .to_string()),
                            )
                            .child(
                                ui::mono(
                                    facts,
                                    11.0,
                                    if chosen {
                                        theme::hex(0x6B645A)
                                    } else {
                                        theme::hex(0x857D72)
                                    },
                                )
                                .mt(px(1.0))
                                .truncate(),
                            ),
                    )
                    .id(SharedString::from(format!("take-{take_id}")))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_take(clip_id.clone(), take_id.clone(), window, cx)
                    }))
            }))
    }

    pub(crate) fn inspector_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        if !self.inspector {
            return div().into_any_element();
        }

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
                    .h(px(44.0))
                    .flex_none()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(14.0))
                    .border_b_1()
                    .border_color(theme::hex(0xEBE4D9))
                    .child(ui::section_label(t!("clip.this_clip").to_string().to_uppercase()))
                    .child(div().flex_1())
                    .child(
                        div()
                            .flex_none()
                            .child(icon::icon(
                                icon::name::PANEL_CLOSE,
                                18.0,
                                theme::hex(0x857D72),
                            ))
                            .id("close-inspector")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.inspector = false;
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
                    .gap(px(15.0))
                    .p(px(14.0))
                    .id("inspector-scroll")
                    .overflow_y_scroll()
                    // Said plainly rather than by greying every control: the
                    // run has the settings it started with, whatever you do here.
                    .when(self.busy(), |d| {
                        d.child(
                            div()
                                .w_full()
                                .px(px(12.0))
                                .py(px(10.0))
                                .rounded(px(9.0))
                                .bg(theme::hex(0xFFF3E6))
                                .border_1()
                                .border_color(theme::hex(0xFFE0C2))
                                .text_size(px(11.5))
                                .line_height(px(17.0))
                                .text_color(theme::hex(0x8F4406))
                                .child(t!("clip.settings_fixed").to_string()),
                        )
                    })
                    // What just happened, where it happened, and only until the
                    // next thing happens.
                    .when_some(self.voice_saved.clone(), |d, saved| {
                        d.child(
                            div()
                                .h_flex()
                                .w_full()
                                .items_start()
                                .gap(px(9.0))
                                .px(px(12.0))
                                .py(px(10.0))
                                .rounded(px(9.0))
                                .bg(theme::hex(0xF1F7F3))
                                .border_1()
                                .border_color(theme::hex(0xC9E0D3))
                                .child(icon::filled(
                                    icon::name::CHECK_CIRCLE,
                                    17.0,
                                    theme::hex(0x287A57),
                                ))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .text_size(px(11.5))
                                        .line_height(px(17.0))
                                        .text_color(theme::hex(0x2C5C46))
                                        .child(
                                            t!("voice.saved_for_clip", time = saved).to_string(),
                                        ),
                                ),
                        )
                    })
                    // A finished clip is a record, not a set of controls: what
                    // made it, and every reading of it.
                    .when_some(self.clip().cloned(), |d, clip| {
                        d.child(self.made_with_section(&clip, cx))
                            .child(self.takes_section(&clip, cx))
                    })
                    .when(self.clip().is_none(), |d| {
                        d.child(self.voice_section(cx))
                            .child(self.model_section(cx))
                            .child(self.delivery_section(cx))
                    })
                    // Not in the design's action row, and it has to live
                    // somewhere: a clip is the one thing here that takes disk.
                    .when_some(self.clip().map(|c| c.id.clone()), |d, id| {
                        d.child(div().flex_1()).child(
                            ui::secondary_button(None, t!("inspect.delete_clip").to_string())
                                .w_full()
                                .h(px(34.0))
                                .justify_center()
                                .text_color(theme::hex(0xC7362B))
                                .id("delete-clip")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.delete_selected_clip(id.clone(), window, cx)
                                })),
                        )
                    }),
            )
            .into_any_element()
    }
}
