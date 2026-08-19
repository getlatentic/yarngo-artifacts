//! The workspace: voices and clips on the left, one composer on the right.
//!
//! Structure follows `Yarngo Studio.dc.html`. Two decisions from that design
//! that the previous single-column build got wrong:
//!
//! * **Model choice is status, not a control.** It lives in the title bar
//!   because it is picked rarely and wanted visible always — five chips
//!   competing with Generate made a rare choice look like a frequent one.
//! * **Where generation happens is stated, not implied.** The footer says it
//!   on every screen rather than leaving "local" to be inferred.

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{
    input::Textarea,
    ActiveTheme, StyledExt,
};
use rust_i18n::t;
use speech_engine::{Clip, Voice};

use crate::theme;
use crate::{Status, VoiceStudio};

/// Words per second used to estimate spoken length before generating, so the
/// composer can say what it will cost in time. Measured, not guessed: the
/// Nigerian sanity run averaged about 190 wpm.
const WORDS_PER_SECOND: f32 = 3.2;

/// Space the macOS traffic lights occupy at the left of the title bar. Zed uses
/// 78 on the macOS 26 SDK and 71 before it; this app targets the newer SDK.
const TRAFFIC_LIGHT_PADDING: f32 = 78.0;
/// Right padding of the bar, and the gear that sits inside it.
const BAR_RIGHT_PADDING: f32 = 16.0;
const GEAR_SIZE: f32 = 20.0;
/// Space between the model pill and the gear.
const PILL_GEAR_GAP: f32 = 10.0;
/// Tall enough that the lights sit centred rather than crowding the top edge.
pub(crate) const TITLE_BAR_HEIGHT: f32 = 40.0;

fn initial(label: &str) -> String {
    label.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default()
}

fn clock(created: &str) -> String {
    // "2026-08-18T15:39:00" -> "15:39"
    created.split('T').nth(1).map(|t| t[..5].to_string()).unwrap_or_default()
}

/// Generation speed as a multiple of realtime. The engine reports seconds of
/// inference per second of audio, so the multiple is its inverse: above 1 is
/// faster than the clip plays, below 1 is slower than it. Printing the engine's
/// figure directly says "2.1× realtime" about a machine running at half speed.
pub(crate) fn realtime(rtf: f32) -> String {
    format!("{:.1}", if rtf > 0.0 { 1.0 / rtf } else { 0.0 })
}

pub(crate) fn duration(seconds: f32) -> String {
    // Truncated, not rounded, so a 17.6 second take reads 0:17 here and 0:17
    // in the transport rather than disagreeing with itself by a second.
    let total = seconds as u32;
    format!("{}:{:02}", total / 60, total % 60)
}

impl VoiceStudio {
    /// Title bar: product name on the left, the model as live status on the
    /// right. Status rather than a picker, per the design.
    pub(crate) fn title_bar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self
            .models
            .iter()
            .find(|m| Some(m.id.as_str()) == self.selected_model.as_deref());

        // A download in progress is the model's state, and the only one with a
        // number worth putting in the title bar.
        let downloading = self
            .selected_model
            .as_deref()
            .and_then(|id| self.installs.get(id))
            .filter(|s| s.is_downloading());
        let state = match (&self.status, downloading) {
            (_, Some(status)) => format!("{:.0}%", status.fraction() * 100.0),
            (Status::Generating, _) => t!("workspace.generating").to_string(),
            _ if self.engine.is_some() => t!("workspace.loaded").to_string(),
            _ => t!("workspace.starting").to_string(),
        };

        div()
            .h_flex()
            .items_center()
            .justify_between()
            // macOS draws the traffic lights over the top-left of the window in
            // windowed mode, so the bar has to start clear of them. In
            // fullscreen they are hidden and the space is reclaimed; when the
            // user reveals them the system overlays its own bar, so the layout
            // does not need to move for that second state.
            .pl(px(if window.is_fullscreen() {
                16.0
            } else {
                TRAFFIC_LIGHT_PADDING
            }))
            .pr(px(BAR_RIGHT_PADDING))
            .h(px(TITLE_BAR_HEIGHT))
            .bg(theme::bg_subtle(false))
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .text_size(px(12.5))
                    .font_semibold()
                    .font_family(theme::FONT_DISPLAY)
                    .child(t!("app.name").to_string()),
            )
            // Recording takes the title bar over: the model is not the state
            // that matters while the microphone is live, and a red dot is
            // visible from across the room.
            .when(matches!(self.enrolment, crate::Enrolment::Recording), |d| {
                d.child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap(px(6.0))
                        .text_size(px(11.5))
                        .font_medium()
                        .text_color(theme::hex(0xC7362B))
                        .child(div().size(px(8.0)).rounded_full().bg(theme::hex(0xC7362B)))
                        .child(t!("enrol.recording_now").to_string()),
                )
            })
            .when(!matches!(self.enrolment, crate::Enrolment::Recording), |this| {
            this.child(
                div()
                    .h_flex()
                    .items_center()
                    .gap(px(PILL_GEAR_GAP))
                    .child(
                div()
                    .h_flex()
                    .gap(px(7.0))
                    .items_center()
                    .h(px(24.0))
                    .px(px(10.0))
                    .rounded(px(999.0))
                    .bg(theme::surface(false))
                    .border_1()
                    .border_color(cx.theme().border)
                    // A live dot: green while the engine holds a model, amber
                    // while it is working, so state is visible without reading.
                    .child(
                        div()
                            .size(px(7.0))
                            .rounded_full()
                            .bg(if matches!(self.status, Status::Generating) {
                                cx.theme().warning
                            } else if self.engine.is_some() {
                                cx.theme().success
                            } else {
                                theme::non_text(false)
                            }),
                    )
                    .text_size(px(11.5))
                    .font_medium()
                    .text_color(theme::hex(0x5F594F))
                    .child(match model {
                        Some(m) => format!("{} · {state}", m.label),
                        None => state,
                    })
                    // The chevron points the way the panel will move, so the
                    // pill reads as open rather than merely highlighted.
                    .child(crate::icon::icon(
                        if self.model_menu {
                            crate::icon::name::EXPAND_LESS
                        } else {
                            crate::icon::name::EXPAND_MORE
                        },
                        15.0,
                        theme::non_text(false),
                    ))
                    .id("model-chip")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.model_menu = !this.model_menu;
                        if this.model_menu {
                            // Residency and load times move with every
                            // generation, so the menu reads them on open
                            // rather than trusting the startup catalogue.
                            this.refresh_models(cx);
                            this.refresh_model_sizes(cx);
                        }
                        cx.notify();
                    })),
                    )
                    .child(
                        div()
                            .size(px(GEAR_SIZE))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(crate::icon::icon(
                                crate::icon::name::SETTINGS,
                                GEAR_SIZE,
                                theme::non_text(false),
                            ))
                            .id("settings")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.settings_open = true;
                                this.settings_pane = crate::settings::Pane::Models;
                                this.model_menu = false;
                                this.refresh_models(cx);
                                this.refresh_model_sizes(cx);
                                this.refresh_system(cx);
                                cx.notify();
                            })),
                    ),
            )
            })
    }

    /// The bundled sample: the model speaking as itself. Listed so the app can
    /// be heard before anything is recorded, and labelled `bundled · for trying`
    /// wherever it appears so a clip made with it is never mistaken for yours.
    fn built_in_row(&self, cx: &mut Context<Self>) -> AnyElement {
        // Highlighted only once there is something else it could have been.
        // With no voice enrolled, the sample is what is left rather than what
        // was picked, and a highlight would claim a choice nobody made.
        let chosen = self.speaking_voice().is_none() && !self.voices.is_empty();
        div()
            .h_flex()
            .gap(px(10.0))
            .items_center()
            .p(px(8.0))
            .rounded(px(8.0))
            .when(chosen, |d| d.bg(theme::hex(0xFFF3E6)))
            .child(
                div()
                    .size(px(26.0))
                    .flex_none()
                    .rounded_full()
                    .bg(cx.theme().secondary)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(crate::icon::icon(
                        crate::icon::name::VOLUME_UP,
                        16.0,
                        cx.theme().muted_foreground,
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .v_flex()
                    .child(
                        div()
                            .text_size(px(12.5))
                            .font_medium()
                            .child(t!("voice.sample").to_string()),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .mt(px(1.0))
                            .text_color(theme::hex(0x857D72))
                            .child(t!("voice.sample_detail").to_string()),
                    ),
            )
            .id("v-built-in")
            .on_click(cx.listener(|this, _, _, cx| {
                this.selected_voice = None;
                cx.notify();
            }))
            .into_any_element()
    }

    fn voice_rows(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let selected = self.speaking_voice().map(str::to_owned);
        let usable = self.model_can_clone();
        self.voices
            .iter()
            .map(|voice: &Voice| {
                let id = voice.voice_id.clone();
                let remove = voice.voice_id.clone();
                let is_selected = selected.as_deref() == Some(id.as_str());
                div()
                    .h_flex()
                    .gap(px(10.0))
                    .items_center()
                    .p(px(8.0))
                    .rounded(px(8.0))
                    .when(is_selected, |d| d.bg(theme::hex(0xFFF3E6)))
                    // Dimmed rather than hidden when the model cannot clone:
                    // the voice still exists, and hiding it would read as loss.
                    .when(!usable, |d| d.opacity(0.45))
                    .child(
                        // Avatar: the initial, in a circle. No photo exists, and
                        // a generic person glyph would say less than the letter.
                        div()
                            .size(px(26.0))
                            .flex_none()
                            .rounded_full()
                            .when(is_selected, |d| {
                                d.bg(theme::hex(0xFF8A1F)).text_color(theme::hex(0x171717))
                            })
                            .when(!is_selected, |d| {
                                d.bg(theme::hex(0xE4DCD0)).text_color(theme::hex(0x5F594F))
                            })
                            .flex()
                            .items_center()
                            .justify_center()
                            .font_family(theme::FONT_DISPLAY)
                            .text_size(px(11.0))
                            .font_semibold()
                            .child(initial(&voice.label)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .v_flex()
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .when(is_selected, |d| {
                                        d.font_semibold().text_color(theme::hex(0x8F4406))
                                    })
                                    .when(!is_selected, |d| {
                                        d.font_medium().text_color(theme::hex(0x171717))
                                    })
                                    .child(voice.label.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .mt(px(1.0))
                                    .when(is_selected, |d| {
                                        d.text_color(theme::hex(0x8F4406)).opacity(0.75)
                                    })
                                    .when(!is_selected, |d| d.text_color(theme::hex(0x857D72)))
                                    .child(if voice.seconds > 0.0 {
                                        t!("workspace.reference_of", time = duration(voice.seconds))
                                            .to_string()
                                    } else {
                                        t!("workspace.reference_len").to_string()
                                    }),
                            )
                    )
                    .child({
                        // First click asks, second confirms. Erasing a voice
                        // costs twenty seconds of reading and forty of
                        // preparation to undo, and the × is a thumb's width
                        // from the row you click to select it.
                        let asking = self.confirming_voice.as_deref() == Some(remove.as_str());
                        div()
                            .flex_none()
                            .when(asking, |d| {
                                d.text_size(px(11.0))
                                    .font_semibold()
                                    .text_color(theme::hex(0xC7362B))
                                    .child(t!("voice.confirm_delete").to_string())
                            })
                            .when(!asking, |d| {
                                d.child(crate::icon::icon(
                                    crate::icon::name::CLOSE,
                                    16.0,
                                    theme::non_text(false),
                                ))
                            })
                            .id(SharedString::from(format!("vdel-{remove}")))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.confirming_voice.as_deref() == Some(remove.as_str()) {
                                    this.delete_voice(remove.clone(), cx);
                                } else {
                                    this.confirming_voice = Some(remove.clone());
                                    cx.notify();
                                }
                            }))
                    })
                    .id(SharedString::from(format!("v-{id}")))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.model_can_clone() {
                            return;
                        }
                        this.selected_voice = Some(id.clone());
                        // Warm on selection, not on Generate: the wait belongs
                        // to the moment of choosing, not to the first clip.
                        this.warm_selected_voice(cx);
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect()
    }

    fn clip_rows(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        self.clips
            .iter()
            .map(|clip: &Clip| {
                let path = clip.path.clone();
                let audio_s = clip.audio_s;
                let remove = clip.id.clone();
                // The clip in the player is lifted out of the list: it is the
                // one the transport below belongs to, and without a mark the
                // controls appear to belong to whichever row was clicked last.
                let active = self.playing_clip() == Some(clip.path.as_path());
                let sounding = active && self.is_playing();
                div()
                    .h_flex()
                    .gap(px(10.0))
                    .items_center()
                    .p(px(8.0))
                    .rounded(px(8.0))
                    .when(active, |d| {
                        d.bg(theme::surface(false)).border_1().border_color(theme::hex(0xEBE4D9))
                    })
                    .child(
                        div().flex_none().child(crate::icon::icon(
                            if sounding {
                                crate::icon::name::PAUSE_CIRCLE
                            } else {
                                crate::icon::name::PLAY_CIRCLE
                            },
                            17.0,
                            if active { theme::hex(0x8F4406) } else { theme::hex(0x857D72) },
                        )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .v_flex()
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .when(active, |d| d.font_semibold())
                                    .when(!active, |d| d.font_medium())
                                    .truncate()
                                    .child(clip.title.clone()),
                            )
                            .child(crate::ui::mono(
                                format!("{} · {}", duration(clip.audio_s), clock(&clip.created)),
                                11.0,
                                theme::hex(0x857D72),
                            )),
                    )
                    .child(
                        div()
                            .flex_none()
                            .child(crate::icon::icon(
                                crate::icon::name::CLOSE,
                                16.0,
                                theme::non_text(false),
                            ))
                            .id(SharedString::from(format!("cdel-{remove}")))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.delete_clip(remove.clone(), cx)
                            })),
                    )
                    .id(SharedString::from(format!("c-{}", clip.id)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.load_clip(&path, audio_s, cx);
                    }))
                    .into_any_element()
            })
            .collect()
    }

    pub(crate) fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let has_clips = !self.clips.is_empty();
        let can_clone = self.model_can_clone();
        div()
            .v_flex()
            .w(px(236.0))
            .flex_none()
            .h_full()
            // Warm, so the white working pane beside it is the bright surface.
            .bg(theme::bg_subtle(false))
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                // The list is what gives way when clips accumulate: without a
                // bound it grew past the column and pushed the footer over the
                // status line below the workspace.
                div()
                    .v_flex()
                    .flex_1()
                    .min_h(px(0.0))
                    .id("sidebar-scroll")
                    .overflow_y_scroll()
                    // Labels are inset to the column's own margin, rows to a
                    // narrower one, so a row's rounded highlight sits inside
                    // the label above it rather than lining up with it.
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .justify_between()
                            .pt(px(16.0))
                            .px(px(14.0))
                            .pb(px(8.0))
                            .child(crate::ui::section_label(t!("workspace.voices").to_string().to_uppercase()))
                            .when(can_clone, |d| {
                                d.child(
                                    div()
                                        .flex_none()
                                        .child(crate::icon::icon(
                                            crate::icon::name::ADD,
                                            18.0,
                                            theme::hex(0x5F594F),
                                        ))
                                        .id("add-voice-side")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.begin_enrolment(window, cx)
                                        })),
                                )
                            }),
                    )
                    .child(
                        div()
                            .v_flex()
                            .w_full()
                            .px(px(8.0))
                            .gap(px(2.0))
                    .children(self.voice_rows(cx))
                    .when(can_clone && self.voices.len() < 3, |d| {
                        let empty = self.voices.is_empty();
                        d.child(
                            div()
                                .h_flex()
                                .w_full()
                                .gap(px(10.0))
                                .items_center()
                                .p(px(8.0))
                                .rounded(px(8.0))
                                // With no voices yet this is the only thing to
                                // do, so it reads as a slot waiting to be
                                // filled rather than as one more row.
                                .when(empty, |d| {
                                    d.border_1().border_dashed().border_color(theme::hex(0xD8D0C4))
                                })
                                .child(
                                    div()
                                        .size(px(26.0))
                                        .flex_none()
                                        .rounded_full()
                                        .bg(theme::hex(0xFFF3E6))
                                        .border_1()
                                        .border_color(theme::hex(0xFFCB93))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(crate::icon::icon(
                                            crate::icon::name::MIC,
                                            15.0,
                                            theme::hex(0x8F4406),
                                        )),
                                )
                                .child(
                                    div()
                                        .v_flex()
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .child(
                                            div()
                                                .text_size(px(12.5))
                                                .font_medium()
                                                .when(empty, |d| d.font_semibold())
                                                .child(t!("voice.add").to_string()),
                                        )
                                        .when(empty, |d| {
                                            d.child(crate::ui::mono(
                                                t!("voice.add_takes").to_string(),
                                                11.0,
                                                theme::hex(0x857D72),
                                            ))
                                        }),
                                )
                                .id("add-voice-row")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.begin_enrolment(window, cx)
                                })),
                        )
                    })
                    .child(self.built_in_row(cx))
                    // Said once, where the voices are, rather than discovered
                    // at Generate after a recording has already been made.
                    .when(!can_clone, |d| {
                        d.child(
                            div()
                                .px(px(8.0))
                                .pt(px(4.0))
                                .text_size(px(11.5))
                                .line_height(px(17.0))
                                .text_color(theme::hex(0x857D72))
                                .child(t!("voice.model_cannot_clone").to_string()),
                        )
                    }),
                    )
                    .child(
                        div()
                            .pt(px(20.0))
                            .px(px(14.0))
                            .pb(px(8.0))
                            .child(crate::ui::section_label(
                                t!("workspace.clips").to_string().to_uppercase(),
                            )),
                    )
                    .child(
                        div()
                            .v_flex()
                            .w_full()
                            .px(px(8.0))
                            .gap(px(2.0))
                    .when(matches!(self.status, Status::Generating), |d| {
                        d.child(
                            div()
                                .h_flex()
                                .gap(px(10.0))
                                .items_center()
                                .p(px(8.0))
                                .rounded(px(8.0))
                                .bg(theme::surface(false))
                                .border_1()
                                .border_color(theme::hex(0xEBE4D9))
                                .child(crate::icon::icon(
                                    crate::icon::name::GRAPHIC_EQ,
                                    17.0,
                                    theme::hex(0xFF8A1F),
                                ))
                                .child(
                                    div()
                                        .v_flex()
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .child(
                                            div()
                                                .text_size(px(12.5))
                                                .child(t!("workspace.generating_clip").to_string()),
                                        )
                                        .child(crate::ui::mono(
                                            t!("workspace.just_now").to_string(),
                                            11.0,
                                            theme::hex(0x857D72),
                                        )),
                                ),
                        )
                    })
                    .when(!has_clips, |d| {
                        d.child(
                            div()
                                .p(px(12.0))
                                .rounded(px(12.0))
                                .border_1()
                                .border_dashed()
                                .border_color(cx.theme().border)
                                .text_size(px(12.0))
                                .line_height(px(19.0))
                                .text_color(theme::hex(0x6B645A))
                                .child(t!("workspace.clips_empty").to_string()),
                        )
                    })
                    .children(self.clip_rows(cx)),
                    ),
            )
            // Footer states what is on disk, so "where did the space go" has an
            // answer without opening Settings.
            .child(
                div()
                    .v_flex()
                    .px(px(14.0))
                    .py(px(12.0))
                    .gap(px(7.0))
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_size(px(11.5))
                    .text_color(theme::hex(0x5F594F))
                    .child(
                        div()
                            .h_flex()
                            .gap(px(8.0))
                            .items_center()
                            .child(div().size(px(7.0)).rounded_full().bg(cx.theme().success))
                            // Named and numbered: "running" alone cannot answer
                            // which runtime, and the version is what a bug
                            // report needs.
                            .child(format!(
                                "{} · {}",
                                t!("workspace.runtime_running"),
                                speech_engine::runtime::VERSION
                            )),
                    )
                    .child(
                        div()
                            .h_flex()
                            .gap(px(8.0))
                            .items_center()
                            .child(crate::icon::icon(
                                crate::icon::name::HARD_DRIVE,
                                15.0,
                                theme::hex(0x857D72),
                            ))
                            .child({
                                // English needs the plural; a bare count reads
                                // as a typo.
                                let n = self.installed_models();
                                let key = if n == 1 {
                                    "workspace.model_on_disk"
                                } else {
                                    "workspace.models_on_disk"
                                };
                                format!("{} · {:.1} GB", t!(key, count = n), self.installed_gb())
                            })
                            .id("disk-row")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.choosing_model = true;
                                this.refresh_model_sizes(cx);
                                cx.notify();
                            })),
                    ),
            )
    }

    pub(crate) fn composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let text = self.text.read(cx).value().to_string();
        let words = text.split_whitespace().count();
        let spoken = words as f32 / WORDS_PER_SECOND;
        let voice = self
            .voices
            .iter()
            .find(|v| Some(v.voice_id.as_str()) == self.selected_voice.as_deref());
        let model_label = self
            .models
            .iter()
            .find(|m| Some(m.id.as_str()) == self.selected_model.as_deref())
            .map(|m| m.label.clone());
        let has_voice = voice.is_some();

        div()
            .v_flex()
            .flex_1()
            .h_full()
            .gap(px(14.0))
            // Bounded so a tall composer cannot push the status row out from
            // under it and over the sidebar's footer.
            .overflow_hidden()
            .px(px(26.0))
            .py(px(22.0))
            .child(
                div()
                    .h_flex()
                    .w_full()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                div()
                    .v_flex()
                    .child(
                        div()
                            .text_size(px(19.0))
                            .font_semibold()
                            .font_family(theme::FONT_DISPLAY)
                            .child(t!("workspace.new_clip").to_string()),
                    )
                    .child(
                        div()
                            .text_size(px(12.5))
                            .mt(px(3.0))
                            .text_color(theme::hex(0x6B645A))
                            .child({
                                let who = voice.map(|v| v.label.clone()).unwrap_or_else(|| {
                                    if self.voices.is_empty() {
                                        t!("voice.none_selected").to_string()
                                    } else {
                                        t!("voice.sample").to_string()
                                    }
                                });
                                match model_label {
                                    Some(m) => format!("{who} · {m}"),
                                    None => who,
                                }
                            }),
                    ),
                    )
                    .child(div().flex_1())
                    .when_some(self.last.as_ref().and_then(|l| l.seed), |this, seed| {
                        // The one generation parameter the engine actually
                        // takes. Pinned, "generate again" gives the same
                        // reading of whatever the text now says; unpinned,
                        // every press draws a fresh one.
                        let pinned = self.pinned_seed == Some(seed);
                        this.child(
                            div()
                                .h_flex()
                                .h(px(32.0))
                                .px(px(12.0))
                                .gap(px(7.0))
                                .flex_none()
                                .items_center()
                                .rounded(px(8.0))
                                .bg(theme::surface(false))
                                .border_1()
                                .border_color(if pinned {
                                    theme::hex(0x171717)
                                } else {
                                    theme::hex(0xD8D0C4)
                                })
                                .text_size(px(12.0))
                                .font_semibold()
                                .text_color(theme::hex(0x171717))
                                .child(crate::icon::icon(
                                    crate::icon::name::TUNE,
                                    16.0,
                                    theme::hex(0x5F594F),
                                ))
                                .child(if pinned {
                                    t!("workspace.seed_pinned", seed = seed).to_string()
                                } else {
                                    t!("workspace.seed_fresh").to_string()
                                })
                                .id("seed-chip")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.pinned_seed =
                                        (this.pinned_seed != Some(seed)).then_some(seed);
                                    cx.notify();
                                })),
                        )
                    }),
            )
            // The voice card: whose voice, and the words it was learned from.
            .when_some(voice.cloned(), |this, voice| {
                this.child(
                    div()
                        .h_flex()
                        .gap(px(12.0))
                        .items_center()
                        .px(px(15.0))
                        .py(px(13.0))
                        .rounded(px(12.0))
                        .bg(theme::surface(false))
                        .border_1()
                        .border_color(theme::hex(0xEBE4D9))
                        .shadow_sm()
                        .child(
                            div()
                                .size(px(30.0))
                                .flex_none()
                                .rounded_full()
                                .bg(theme::hex(0xFF8A1F))
                                .flex()
                                .items_center()
                                .justify_center()
                                .font_family(theme::FONT_DISPLAY)
                                .text_size(px(12.0))
                                .font_semibold()
                                .text_color(theme::hex(0x171717))
                                .child(initial(&voice.label)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .v_flex()
                                .child(
                                    div()
                                        .text_size(px(13.0))
                                        .font_semibold()
                                        .child(voice.label.clone()),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.5))
                                        .mt(px(2.0))
                                        .truncate()
                                        .text_color(theme::hex(0x6B645A))
                                        .child(t!(
                                            "workspace.reference_quote",
                                            text = crate::truncate(&voice.reference_text, 62)
                                        )
                                        .to_string()),
                                ),
                        )
                        .child(
                            crate::ui::secondary_button(
                                None,
                                t!("workspace.change").to_string(),
                            )
                            .h(px(30.0))
                            .px(px(11.0))
                            .rounded(px(7.0))
                            .text_size(px(12.0))
                            .id("change-voice")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.begin_enrolment(window, cx)
                            })),
                        )
                        .child({
                            let reference = voice.reference_audio.clone();
                            crate::ui::secondary_button(
                                Some((crate::icon::name::PLAY_ARROW, 0x5F594F)),
                                t!("workspace.hear_reference").to_string(),
                            )
                            .h(px(30.0))
                            .px(px(11.0))
                            .gap(px(6.0))
                            .rounded(px(7.0))
                            .text_size(px(12.0))
                            .id("hear-reference")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                    // Duration is unknown for a reference clip;
                                    // the player reads it from the file.
                                    this.load_clip(&reference, 0.0, cx);
                                    if let Some(player) = this.player.as_ref() {
                                        player.play();
                                    }
                                    cx.notify();
                                }))
                        }),
                )
            })
            // A model is the part that actually speaks. Said plainly, with the
            // download's own progress, rather than leaving Generate inert.
            .when(!self.model_ready(), |this| {
                let downloading = self
                    .selected_model
                    .as_deref()
                    .and_then(|id| self.installs.get(id))
                    .filter(|s| s.is_downloading())
                    .cloned();
                let label = self
                    .models
                    .iter()
                    .find(|m| Some(m.id.as_str()) == self.selected_model.as_deref())
                    .map(|m| m.label.clone())
                    .unwrap_or_default();
                this.child(
                    div()
                        .v_flex()
                        .w_full()
                        .gap(px(11.0))
                        .px(px(18.0))
                        .py(px(15.0))
                        .rounded(px(12.0))
                        .bg(theme::surface(false))
                        // A heavier rule than the other cards: this is the one
                        // thing standing between the text and a clip.
                        .border_2()
                        .border_color(theme::hex(0x171717))
                        .child(
                            div()
                                .h_flex()
                                .w_full()
                                .items_center()
                                .gap(px(14.0))
                                .child(
                                    div()
                                        .size(px(34.0))
                                        .flex_none()
                                        .rounded(px(9.0))
                                        .bg(theme::hex(0xFFF3E6))
                                        .border_1()
                                        .border_color(theme::hex(0xFFCB93))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(crate::icon::icon(
                                            crate::icon::name::DOWNLOAD,
                                            19.0,
                                            theme::hex(0x8F4406),
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
                                                .text_size(px(14.0))
                                                .font_semibold()
                                                .child(match &downloading {
                                                    Some(_) => t!(
                                                        "compose.downloading_model",
                                                        model = label.clone()
                                                    )
                                                    .to_string(),
                                                    None => t!(
                                                        "compose.model_missing",
                                                        model = label.clone()
                                                    )
                                                    .to_string(),
                                                }),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(12.5))
                                                .line_height(px(19.0))
                                                .text_color(theme::hex(0x5F594F))
                                                .mt(px(3.0))
                                                .child(t!("compose.model_explains").to_string()),
                                        ),
                                )
                                .child(
                                    crate::ui::secondary_button(
                                        None,
                                        t!("compose.choose_another").to_string(),
                                    )
                                    .id("choose-another")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.model_menu = true;
                                        this.refresh_models(cx);
                                        cx.notify();
                                    })),
                                ),
                        )
                        .when_some(downloading, |d, status| {
                            d.child(
                                div().w_full().h(px(6.0)).rounded_full().bg(theme::hex(0xEBE4D9)).child(
                                    div()
                                        .h_full()
                                        .rounded_full()
                                        .bg(theme::hex(0xFF8A1F))
                                        .w(relative(status.fraction())),
                                ),
                            )
                        }),
                )
            })
            // The sample, when it is what is selected: named, and one button
            // from the voice that would replace it.
            .when(voice.is_none() && !self.voices.is_empty() && self.model_ready(), |this| {
                let can_clone = self.model_can_clone();
                this.child(
                    div()
                        .h_flex()
                        .gap(px(14.0))
                        .items_center()
                        .p(px(12.0))
                        .rounded(px(12.0))
                        .bg(theme::surface(false))
                        .border_1()
                        .border_color(theme::hex(0xEBE4D9))
                        .child(
                            div()
                                .size(px(38.0))
                                .flex_none()
                                .rounded_full()
                                .bg(cx.theme().secondary)
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(crate::icon::icon(
                                    crate::icon::name::VOLUME_UP,
                                    20.0,
                                    cx.theme().muted_foreground,
                                )),
                        )
                        .child(
                            div()
                                .v_flex()
                                .flex_1()
                                .min_w(px(0.0))
                                .gap(px(3.0))
                                .child(
                                    div()
                                        .font_family(theme::FONT_DISPLAY)
                                        .text_size(px(14.0))
                                        .font_semibold()
                                        .child(t!("voice.sample").to_string()),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.5))
                                        .text_color(theme::hex(0x5F594F))
                                        .child(t!("voice.sample_only_listening").to_string()),
                                ),
                        )
                        .when(can_clone, |d| {
                            d.child(
                                crate::ui::secondary_button(
                                    None,
                                    t!("voice.use_mine").to_string(),
                                )
                                .id("use-my-voice")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.selected_voice =
                                        this.voices.first().map(|v| v.voice_id.clone());
                                    this.warm_selected_voice(cx);
                                    cx.notify();
                                })),
                            )
                        }),
                )
            })
            .when(voice.is_none() && self.voices.is_empty() && self.model_ready(), |this| {
                let can_clone = self.model_can_clone();
                this.child(
                    div()
                        .h_flex()
                        .gap(px(14.0))
                        .items_center()
                        .p(px(12.0))
                        .rounded(px(12.0))
                        .bg(theme::surface(false))
                        .border_1()
                        .border_color(theme::hex(0xEBE4D9))
                        .child(
                            div()
                                .size(px(38.0))
                                .flex_none()
                                .rounded_full()
                                .bg(theme::hex(0xFFF3E6))
                                .border_1()
                                .border_color(theme::hex(0xFFCB93))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(crate::icon::icon(
                                    crate::icon::name::MIC,
                                    20.0,
                                    theme::hex(0x8F4406),
                                )),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .v_flex()
                                .gap(px(3.0))
                                .child(
                                    div()
                                        .font_family(theme::FONT_DISPLAY)
                                        .text_size(px(14.0))
                                        .font_semibold()
                                        .child(t!("voice.none_title").to_string()),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.5))
                                        .line_height(px(19.0))
                                        .text_color(theme::hex(0x5F594F))
                                        .child(t!("voice.none_detail").to_string()),
                                ),
                        )
                        .when(can_clone, |d| {
                            d.child(
                                div()
                                    .h(px(36.0))
                                    .px(px(16.0))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .rounded(px(8.0))
                                    .bg(theme::hex(0xFF6E08))
                                    .text_size(px(13.0))
                                    .font_semibold()
                                    .text_color(theme::hex(0xFFFEFD))
                                    .id("enrol-from-composer")
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.begin_enrolment(window, cx)),
                                    )
                                    .child(t!("voice.add").to_string()),
                            )
                        }),
                )
            })
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_h(px(0.0))
                    .gap(px(10.0))
                    .px(px(18.0))
                    .py(px(16.0))
                    .rounded(px(12.0))
                    .bg(theme::surface(false))
                    .border_1()
                    .border_color(theme::hex(0xEBE4D9))
                    .shadow_sm()
                    // The card *is* the field. Left to itself the textarea
                    // draws a second border and a focus ring inside the first.
                    .child(
                        div()
                            .flex_1()
                            .min_h(px(0.0))
                            .text_size(px(15.0))
                            .line_height(px(25.0))
                            .child(Textarea::new(&self.text).appearance(false).h_full()),
                    )
                    .when(words > 0, |d| {
                        d.child(
                            div()
                                .h_flex()
                                .gap(px(16.0))
                                .flex_none()
                                .text_size(px(11.5))
                                .text_color(theme::hex(0x857D72))
                                .child(t!("workspace.chars", chars = text.chars().count()).to_string())
                                .child(
                                    t!("workspace.spoken", seconds = format!("{spoken:.0}"))
                                        .to_string(),
                                ),
                        )
                    }),
            )
            // What the machine is doing, while it does it. A local wait can say
            // how much audio exists so far; a remote one could only spin.
            .when(matches!(self.status, Status::Generating), |this| {
                let written = self.progress.as_ref().map(|p| p.written_s).unwrap_or(0.0);
                let elapsed = self.progress.as_ref().map(|p| p.elapsed_s).unwrap_or(0.0);
                let model = self
                    .models
                    .iter()
                    .find(|m| Some(m.id.as_str()) == self.selected_model.as_deref())
                    .map(|m| m.label.clone())
                    .unwrap_or_default();
                this.child(
                    div()
                        .v_flex()
                        .w_full()
                        .gap(px(11.0))
                        .px(px(18.0))
                        .py(px(15.0))
                        .rounded(px(12.0))
                        .bg(theme::surface(false))
                        .border_1()
                        .border_color(theme::hex(0xEBE4D9))
                        .child(
                            div()
                                .h_flex()
                                .w_full()
                                .items_center()
                                .gap(px(14.0))
                                .child(crate::icon::icon(
                                    crate::icon::name::GRAPHIC_EQ,
                                    19.0,
                                    theme::hex(0x8F4406),
                                ))
                                .child(
                                    div()
                                        .v_flex()
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .child(
                                            div()
                                                .text_size(px(13.0))
                                                .font_semibold()
                                                .child(
                                                    t!("compose.generating_with", model = model)
                                                        .to_string(),
                                                ),
                                        )
                                        .child(crate::ui::mono(
                                            t!(
                                                "compose.written",
                                                written = format!("{written:.1}"),
                                                total = format!("{:.1}", self.expected_s),
                                                elapsed = format!("{elapsed:.1}")
                                            )
                                            .to_string(),
                                            11.5,
                                            theme::hex(0x6B645A),
                                        )),
                                )
                                .child(
                                    crate::ui::secondary_button(
                                        None,
                                        t!("enrol.cancel").to_string(),
                                    )
                                    .id("cancel-generation")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.cancel_generation(cx)
                                    })),
                                ),
                        )
                        .child(
                            div().w_full().h(px(6.0)).rounded_full().bg(theme::hex(0xEBE4D9)).child(
                                div()
                                    .h_full()
                                    .rounded_full()
                                    .bg(theme::hex(0xFF8A1F))
                                    .w(relative(if self.expected_s > 0.0 {
                                        (written / self.expected_s).clamp(0.0, 1.0)
                                    } else {
                                        0.0
                                    })),
                            ),
                        )
                        .child(
                            div()
                                .text_size(px(11.5))
                                .line_height(px(17.0))
                                .text_color(theme::hex(0x6B645A))
                                .child(t!("compose.while_generating").to_string()),
                        ),
                )
            })
            .child(self.player_card(cx))
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .h_flex()
                            .w_full()
                            .gap(px(14.0))
                            .items_center()
                            .child({
                                // A voice of your own is the requirement. The
                                // sample can be heard, not shipped.
                                let waiting = !self.model_ready();
                                let ready = !self.busy()
                                    && words > 0
                                    && has_voice
                                    && (self.model_ready() || self.queued.is_none());
                                div()
                                    .h(px(38.0))
                                    .px(px(20.0))
                                    .flex()
                                    .items_center()
                                    .rounded(px(8.0))
                                    .text_size(px(13.5))
                                    .font_semibold()
                                    .when(ready, |d| {
                                        d.bg(theme::hex(0xFF6E08)).text_color(theme::hex(0xFFFEFD))
                                    })
                                    .when(!ready, |d| {
                                        d.bg(theme::hex(0xF1EBE1)).text_color(theme::hex(0xB0A79B))
                                    })
                                    .gap(px(7.0))
                                    .id("generate")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.generate(None, cx)
                                    }))
                                    // A glyph while it works, so a button that
                                    // has stopped responding to clicks looks
                                    // busy rather than broken.
                                    .when(self.busy(), |d| {
                                        d.child(crate::icon::icon(
                                            crate::icon::name::HOURGLASS,
                                            18.0,
                                            theme::hex(0xB0A79B),
                                        ))
                                    })
                                    .child(if self.busy() {
                                        t!("compose.working").to_string()
                                    } else if waiting {
                                        t!("compose.generate_when_ready").to_string()
                                    } else {
                                        t!("compose.generate").to_string()
                                    })
                            })
                            .when(words == 0 && has_voice, |this| {
                                this.child(
                                    div()
                                        .text_size(px(12.5))
                                        .text_color(theme::hex(0x6B645A))
                                        .child(t!("workspace.type_something").to_string()),
                                )
                            })
                            // Said next to the button that does it: pressing
                            // Generate again after an edit keeps the seed, so
                            // the difference you hear is the words alone.
                            .when(
                                words > 0 && has_voice && self.last.is_some() && !self.busy(),
                                |this| {
                                    this.child(
                                        div()
                                            .text_size(px(12.5))
                                            .text_color(theme::hex(0x6B645A))
                                            .child(t!("workspace.edit_hint").to_string()),
                                    )
                                },
                            )
                            .when(self.queued.is_some(), |this| {
                                this.child(
                                    div()
                                        .text_size(px(12.5))
                                        .text_color(theme::hex(0x6B645A))
                                        .child(t!("compose.queued").to_string()),
                                )
                            })
                            .when(!has_voice, |this| {
                                this.child(
                                    crate::ui::secondary_button(
                                        Some((crate::icon::name::VOLUME_UP, 0x5F594F)),
                                        t!("voice.hear_sample").to_string(),
                                    )
                                    .h(px(38.0))
                                    .id("hear-sample")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.generate(None, cx)
                                    })),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.5))
                                        .text_color(theme::hex(0x6B645A))
                                        .child(t!("voice.add_to_generate").to_string()),
                                )
                            })
                            .child(div().flex_1())
                            .child(
                                div()
                                    .h_flex()
                                    .gap(px(7.0))
                                    .items_center()
                                    .text_size(px(11.5))
                                    .text_color(theme::hex(0x857D72))
                                    .child(crate::icon::icon(
                                        crate::icon::name::WIFI_OFF,
                                        16.0,
                                        theme::hex(0x857D72),
                                    ))
                                    .child(t!("workspace.offline").to_string()),
                            ),
                    ),
            )
    }


}
