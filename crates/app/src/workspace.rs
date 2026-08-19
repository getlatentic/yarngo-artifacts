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
    input::{Input, Textarea},
    ActiveTheme, Sizable, StyledExt,
};
use rust_i18n::t;

use crate::theme;
use crate::{Status, VoiceStudio};

/// Words per second used to estimate spoken length before generating, so the
/// composer can say what it will cost in time. Measured, not guessed: the
/// Nigerian sanity run averaged about 190 wpm.
pub(crate) const WORDS_PER_SECOND: f32 = 3.2;

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
                    // The panel is a toggle, not something that opens itself:
                    // it holds settings you change now and then, and the header
                    // states them meanwhile.
                    .child(
                        div()
                            .w(px(26.0))
                            .h(px(24.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.0))
                            .border_1()
                            .border_color(if self.inspector {
                                theme::hex(0x8F4406)
                            } else {
                                theme::hex(0xE4DCD0)
                            })
                            .child(crate::icon::icon(
                                if self.inspector {
                                    crate::icon::name::PANEL_CLOSE
                                } else {
                                    crate::icon::name::PANEL_OPEN
                                },
                                18.0,
                                theme::hex(0x857D72),
                            ))
                            .id("toggle-inspector")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_inspector(cx))),
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



    /// One row in the clips list, at the fixed 52px the design gives it. Fixed
    /// so a clip that starts generating shows its progress inside its own row
    /// rather than growing and pushing the list down.
    fn clip_row(
        glyph: &'static str,
        glyph_colour: u32,
        title: String,
        detail: String,
        current: bool,
        accent: bool,
    ) -> Div {
        div()
            .relative()
            .h(px(52.0))
            .flex_none()
            .h_flex()
            .items_start()
            .gap(px(10.0))
            .p(px(9.0))
            .rounded(px(8.0))
            .when(current, |d| d.bg(theme::hex(0xFFF3E6)))
            .child(div().flex_none().child(crate::icon::icon(glyph, 17.0, theme::hex(glyph_colour))))
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_w(px(0.0))
                    .child(
                        div()
                            .text_size(px(12.5))
                            .truncate()
                            .when(accent, |d| d.font_semibold().text_color(theme::hex(0x8F4406)))
                            .when(!accent, |d| d.font_medium().text_color(theme::hex(0x171717)))
                            .child(title),
                    )
                    .child(
                        crate::ui::mono(
                            detail,
                            11.0,
                            if accent { theme::hex(0x8F4406) } else { theme::hex(0x857D72) },
                        )
                        .mt(px(2.0))
                        .truncate(),
                    ),
            )
    }

    /// Drafts first, then what has been generated. A draft that is running
    /// carries its own progress line along the bottom of the row.
    fn clip_rows(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut rows: Vec<AnyElement> = Vec::new();

        for draft in self.drafts.iter() {
            let id = draft.id.clone();
            let current = self.selected == crate::clips::Selected::Draft(id.clone());
            let running = draft.generating;
            let detail = if running {
                t!("clip.generating_now").to_string()
            } else {
                t!("clip.draft").to_string()
            };
            let fraction = self
                .progress
                .as_ref()
                .filter(|_| running)
                .map(|p| if p.chunks > 0 { p.chunks_done as f32 / p.chunks as f32 } else { 0.0 })
                .unwrap_or(0.0);
            rows.push(
                Self::clip_row(
                    if running {
                        crate::icon::name::PROGRESS
                    } else {
                        crate::icon::name::EDIT_NOTE
                    },
                    0x8F4406,
                    draft.title(),
                    detail,
                    current || running,
                    true,
                )
                .when(running, |d| {
                    d.child(
                        div()
                            .absolute()
                            .left(px(9.0))
                            .right(px(9.0))
                            .bottom(px(5.0))
                            .h(px(3.0))
                            .rounded_full()
                            .bg(theme::hex(0xFFE0C2))
                            .child(
                                div()
                                    .h_full()
                                    .rounded_full()
                                    .bg(theme::hex(0xFF8A1F))
                                    .w(relative(fraction)),
                            ),
                    )
                })
                .id(SharedString::from(format!("d-{id}")))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_draft(id.clone(), window, cx)
                }))
                .into_any_element(),
            );
        }

        for clip in self.clips.iter() {
            let id = clip.id.clone();
            let current = self.selected == crate::clips::Selected::Clip(id.clone());
            let sounding = self.playing_clip() == Some(clip.path.as_path()) && self.is_playing();
            rows.push(
                Self::clip_row(
                    if sounding {
                        crate::icon::name::PAUSE_CIRCLE
                    } else {
                        crate::icon::name::PLAY_CIRCLE
                    },
                    if current { 0x8F4406 } else { 0x857D72 },
                    clip.name.clone(),
                    format!(
                        "{} · {}",
                        duration(clip.audio_s),
                        self.voice_name(clip.voice_id.as_deref())
                    ),
                    current,
                    current,
                )
                .id(SharedString::from(format!("c-{id}")))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_clip(id.clone(), window, cx)
                }))
                .into_any_element(),
            );
        }

        rows
    }

    /// The sidebar lists clips and nothing else. A voice belongs to the clip
    /// being made, not beside the work, so it lives in the inspector.
    pub(crate) fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = self.clips.is_empty() && self.drafts.iter().all(|d| d.text.trim().is_empty());
        let count = self.clips.len();

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
                div()
                    .flex_none()
                    .pt(px(12.0))
                    .px(px(12.0))
                    .pb(px(8.0))
                    .child(
                        div()
                            .h_flex()
                            .w_full()
                            .h(px(34.0))
                            .items_center()
                            .justify_center()
                            .gap(px(8.0))
                            .rounded(px(8.0))
                            .bg(theme::hex(0x1F1C19))
                            .text_size(px(12.5))
                            .font_semibold()
                            .text_color(theme::hex(0xFFF9F2))
                            .child(crate::icon::icon(
                                crate::icon::name::ADD,
                                17.0,
                                theme::hex(0xFFF9F2),
                            ))
                            .child(t!("clip.new").to_string())
                            .id("new-clip")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.new_draft(window, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .h(px(26.0))
                    .flex_none()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(14.0))
                    .child(crate::ui::section_label(
                        t!("workspace.clips").to_string().to_uppercase(),
                    ))
                    .child(div().flex_1())
                    .child(crate::ui::mono(count.to_string(), 10.5, theme::hex(0xB0A79B))),
            )
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_h(px(0.0))
                    .gap(px(2.0))
                    .pt(px(6.0))
                    .px(px(8.0))
                    .id("sidebar-scroll")
                    .overflow_y_scroll()
                    .when(empty, |d| {
                        d.child(
                            div()
                                .m(px(6.0))
                                .p(px(14.0))
                                .rounded(px(10.0))
                                .border_1()
                                .border_dashed()
                                .border_color(theme::hex(0xD8D0C4))
                                .text_size(px(12.0))
                                .line_height(px(19.0))
                                .text_color(theme::hex(0x6B645A))
                                .child(t!("workspace.clips_empty").to_string()),
                        )
                    })
                    .children(self.clip_rows(cx)),
            )
            // Footer states what is on disk, so "where did the space go" has an
            // answer without opening Settings.
            .child(
                div()
                    .v_flex()
                    .flex_none()
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
                                let n = self.clips.len();
                                let key = if n == 1 { "clip.one_on_disk" } else { "clip.n_on_disk" };
                                format!(
                                    "{} · {:.0} MB",
                                    t!(key, count = n),
                                    self.clips_bytes() as f32 / 1e6
                                )
                            })
                            .id("disk-row")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.settings_open = true;
                                this.settings_pane = crate::settings::Pane::Storage;
                                cx.notify();
                            })),
                    ),
            )
    }

    /// The clip's name, and what it is set up with. A fixed 46px so nothing
    /// below it moves when the name is being edited.
    fn composer_header(&self, cx: &mut Context<Self>) -> Div {
        let renaming = self.renaming.as_ref() == Some(&self.selected);
        let title = match &self.selected {
            crate::clips::Selected::Draft(_) => {
                self.draft().map(|d| d.title()).unwrap_or_default()
            }
            crate::clips::Selected::Clip(_) => {
                self.clip().map(|c| c.name.clone()).unwrap_or_default()
            }
        };

        div()
            .h_flex()
            .h(px(46.0))
            .flex_none()
            .w_full()
            .items_start()
            .gap(px(12.0))
            .child(
                div()
                    .v_flex()
                    .min_w(px(0.0))
                    .child(
                        div()
                            .h_flex()
                            .h(px(26.0))
                            .items_center()
                            .gap(px(8.0))
                            .when(renaming, |d| {
                                d.child(
                                    div()
                                        .w(px(300.0))
                                        .child(Input::new(&self.clip_name).small()),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.5))
                                        .text_color(theme::hex(0x6B645A))
                                        .child(t!("clip.rename_keys").to_string()),
                                )
                            })
                            .when(!renaming, |d| {
                                d.child(
                                    div()
                                        .font_family(theme::FONT_DISPLAY)
                                        .text_size(px(19.0))
                                        .font_semibold()
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .child(crate::icon::icon(
                                            crate::icon::name::EDIT,
                                            17.0,
                                            theme::hex(0xB0A79B),
                                        ))
                                        .id("rename-clip")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.begin_rename(window, cx)
                                        })),
                                )
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(theme::hex(0x6B645A))
                            .child(self.composer_subtitle()),
                    ),
            )
            .child(div().flex_1())
            // With the inspector closed, the two settings that belong to this
            // clip are still stated, and either one opens it.
            .when(!self.inspector, |d| {
                d.child(
                    div()
                        .h_flex()
                        .flex_none()
                        .gap(px(8.0))
                        .child(
                            Self::header_chip(
                                crate::icon::name::RECORD_VOICE,
                                self.voice_name(self.clip_voice()),
                                true,
                            )
                            .id("chip-voice")
                            .on_click(cx.listener(|this, _, _, cx| this.open_inspector(cx))),
                        )
                        .child(
                            Self::header_chip(
                                crate::icon::name::TUNE,
                                self.model_label(),
                                false,
                            )
                            .id("chip-model")
                            .on_click(cx.listener(|this, _, _, cx| this.open_inspector(cx))),
                        ),
                )
            })
    }

    fn header_chip(glyph: &'static str, label: String, chevron: bool) -> Div {
        div()
            .h_flex()
            .h(px(30.0))
            .px(px(10.0))
            .gap(px(7.0))
            .flex_none()
            .items_center()
            .rounded(px(8.0))
            .bg(theme::surface(false))
            .border_1()
            .border_color(theme::hex(0xD8D0C4))
            .text_size(px(12.0))
            .font_medium()
            .text_color(theme::hex(0x171717))
            .child(crate::icon::icon(glyph, 16.0, theme::hex(0x5F594F)))
            .child(label)
            .when(chevron, |d| {
                d.child(crate::icon::icon(
                    crate::icon::name::EXPAND_MORE,
                    15.0,
                    theme::hex(0x857D72),
                ))
            })
    }

    pub(crate) fn model_label(&self) -> String {
        self.models
            .iter()
            .find(|m| Some(m.id.as_str()) == self.clip_model())
            .map(|m| m.label.clone())
            .unwrap_or_default()
    }

    /// One line under the name saying what this clip is set up with, or what it
    /// was made with once it exists.
    fn composer_subtitle(&self) -> String {
        let voice = self.voice_name(self.clip_voice());
        let model = self.model_label();
        match self.clip() {
            Some(clip) => t!(
                "clip.made_line",
                length = duration(clip.audio_s),
                at = clock(&clip.created),
                voice = voice,
                model = model
            )
            .to_string(),
            None => format!("{voice} · {model}"),
        }
    }

    /// The strip along the top of the card. Always there, so the card is the
    /// same height whether or not there is audio yet.
    fn card_strip(&self, cx: &mut Context<Self>) -> Div {
        let strip = div()
            .h(px(76.0))
            .flex_none()
            .w_full()
            .px(px(16.0))
            .border_b_1()
            .border_color(theme::hex(0xF1EBE1));

        if self.busy() {
            return strip.v_flex().justify_center().gap(px(9.0)).child(self.generating_row(cx)).child(
                div()
                    .w_full()
                    .h(px(4.0))
                    .rounded_full()
                    .bg(theme::hex(0xFFE0C2))
                    .child(
                        div()
                            .h_full()
                            .rounded_full()
                            .bg(theme::hex(0xFF8A1F))
                            .w(relative(self.generation_fraction())),
                    ),
            );
        }

        match self.clip() {
            Some(clip) => {
                let (playing, progress) = match self.player_state() {
                    Some(state) => state,
                    None => (false, 0.0),
                };
                let position = crate::format_time(self.player_position());
                strip
                    .h_flex()
                    .items_center()
                    .gap(px(14.0))
                    .child(
                        crate::ui::play_button(playing, true)
                            .id("play")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_playback(cx))),
                    )
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_w(px(0.0))
                            .child(crate::ui::waveform_wide(
                                &self.clip_levels,
                                progress,
                                44.0,
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
                                this.seek_from(event.position().x, cx)
                            })),
                    )
                    .child(
                        crate::ui::mono(
                            format!("{position} / {}", duration(clip.audio_s)),
                            12.0,
                            theme::hex(0x6B645A),
                        )
                        .flex_none(),
                    )
            }
            // Nothing generated yet: the control is there, greyed, so the card
            // does not change shape when it becomes real.
            None => strip
                .h_flex()
                .items_center()
                .gap(px(14.0))
                .child(
                    div()
                        .size(px(40.0))
                        .flex_none()
                        .rounded_full()
                        .bg(theme::hex(0xFFFDFA))
                        .border_1()
                        .border_color(theme::hex(0xEBE4D9))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(crate::icon::icon(
                            crate::icon::name::PLAY_ARROW,
                            22.0,
                            theme::hex(0xC4BBAE),
                        )),
                )
                .child(
                    div()
                        .h_flex()
                        .flex_1()
                        .min_w(px(0.0))
                        .items_center()
                        .gap(px(12.0))
                        .child(
                            div()
                                .text_size(px(12.5))
                                .text_color(theme::hex(0xB0A79B))
                                .child(t!("clip.audio_appears").to_string()),
                        )
                        .child(div().flex_1().h(px(1.0)).bg(theme::hex(0xF1EBE1))),
                )
                .child(crate::ui::mono("0:00", 12.0, theme::hex(0xC4BBAE)).flex_none()),
        }
    }

    /// The head of the progress strip: what is running, how far in, and out.
    fn generating_row(&self, cx: &mut Context<Self>) -> Div {
        let written = self.progress.as_ref().map(|p| p.written_s).unwrap_or(0.0);
        let elapsed = self.progress.as_ref().map(|p| p.elapsed_s).unwrap_or(0.0);
        let rtf = (written > 0.0 && elapsed > 0.0).then(|| elapsed / written);

        div()
            .h_flex()
            .w_full()
            .items_center()
            .gap(px(14.0))
            .child(
                div()
                    .size(px(36.0))
                    .flex_none()
                    .rounded_full()
                    .bg(theme::hex(0xFFF3E6))
                    .border_1()
                    .border_color(theme::hex(0xFFCB93))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(crate::icon::icon(
                        crate::icon::name::GRAPHIC_EQ,
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
                            .text_size(px(13.5))
                            .font_semibold()
                            .child(match rtf {
                                Some(rtf) => t!(
                                    "compose.generating_left",
                                    seconds =
                                        format!("{:.0}", (self.expected_s - written).max(0.0) * rtf)
                                )
                                .to_string(),
                                None => t!("compose.generating").to_string(),
                            }),
                    )
                    .child(
                        crate::ui::mono(
                            match rtf {
                                Some(rtf) => t!(
                                    "compose.written_of",
                                    written = format!("{written:.0}"),
                                    total = format!("{:.0}", self.expected_s),
                                    rtf = realtime(rtf)
                                )
                                .to_string(),
                                None => t!("compose.starting").to_string(),
                            },
                            11.5,
                            theme::hex(0x6B645A),
                        )
                        .mt(px(2.0))
                        .truncate(),
                    ),
            )
            .child(
                crate::ui::secondary_button(None, t!("enrol.cancel").to_string())
                    .h(px(32.0))
                    .px(px(12.0))
                    .text_size(px(12.0))
                    .id("cancel-generation")
                    .on_click(cx.listener(|this, _, _, cx| this.cancel_generation(cx))),
            )
    }

    /// The words: a field while writing, the record of what was said once the
    /// clip exists, and locked while it runs.
    fn card_body(&self, cx: &mut Context<Self>) -> Div {
        let body = div()
            .v_flex()
            .flex_1()
            .min_h(px(0.0))
            .w_full()
            .px(px(18.0))
            .py(px(16.0))
            .gap(px(11.0));

        if self.busy() {
            return body
                .child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(crate::icon::icon(
                            crate::icon::name::LOCK,
                            15.0,
                            theme::hex(0x857D72),
                        ))
                        .child(crate::ui::section_label(
                            t!("clip.text_locked").to_string().to_uppercase(),
                        )),
                )
                .child(
                    div()
                        .text_size(px(15.0))
                        .line_height(px(25.0))
                        .text_color(theme::hex(0x5F594F))
                        .child(self.running_text()),
                );
        }

        match self.clip() {
            Some(clip) => body
                .child(
                    div()
                        .h_flex()
                        .h(px(20.0))
                        .items_center()
                        .gap(px(8.0))
                        .child(crate::icon::icon(
                            crate::icon::name::DESCRIPTION,
                            15.0,
                            theme::hex(0x857D72),
                        ))
                        .child(crate::ui::section_label(
                            t!("clip.text_from").to_string().to_uppercase(),
                        ))
                        .child(div().flex_1())
                        .child(
                            crate::ui::secondary_button(
                                Some((crate::icon::name::EDIT, 0x5F594F)),
                                t!("clip.edit_text").to_string(),
                            )
                            .h(px(26.0))
                            .px(px(10.0))
                            .gap(px(6.0))
                            .rounded(px(7.0))
                            .text_size(px(11.5))
                            .id("edit-text")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.edit_clip_text(window, cx)
                            })),
                        ),
                )
                .child(
                    div()
                        .text_size(px(15.0))
                        .line_height(px(25.0))
                        .text_color(theme::hex(0x171717))
                        .child(clip.text.clone()),
                ),
            None => body.child(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .text_size(px(15.0))
                    .line_height(px(25.0))
                    .child(Textarea::new(&self.text).appearance(false).h_full()),
            ),
        }
    }

    /// The row under the words: what they cost, or what editing them means.
    fn card_footer(&self, cx: &mut Context<Self>) -> Div {
        let text = self.text.read(cx).value().to_string();
        let words = text.split_whitespace().count();
        let spoken = words as f32 / WORDS_PER_SECOND;

        div()
            .h_flex()
            .h(px(34.0))
            .flex_none()
            .w_full()
            .items_center()
            .gap(px(16.0))
            .px(px(18.0))
            .border_t_1()
            .border_color(theme::hex(0xF1EBE1))
            .text_size(px(11.5))
            .text_color(theme::hex(0x857D72))
            .when(self.clip().is_some(), |d| {
                d.child(t!("clip.editing_makes_take").to_string())
            })
            .when(self.clip().is_none(), |d| {
                d.child(t!("workspace.chars", chars = text.chars().count()).to_string())
                    .when(words > 0, |d| {
                        d.child(
                            t!("workspace.spoken", seconds = format!("{spoken:.0}")).to_string(),
                        )
                    })
            })
    }

    /// The one row of actions. Which buttons it holds changes with the state;
    /// where it sits never does.
    fn card_actions(&self, cx: &mut Context<Self>) -> Div {
        let row = div().h_flex().h(px(38.0)).flex_none().w_full().items_center().gap(px(14.0));

        let row = if self.busy() {
            row.child(
                crate::ui::secondary_button(
                    Some((crate::icon::name::ADD, 0x5F594F)),
                    t!("clip.start_another").to_string(),
                )
                .h(px(38.0))
                .px(px(16.0))
                .text_size(px(13.0))
                .id("start-another")
                .on_click(cx.listener(|this, _, window, cx| this.new_draft(window, cx))),
            )
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(theme::hex(0x6B645A))
                    .child(t!("clip.keeps_running").to_string()),
            )
        } else if self.clip().is_some() {
            row.child(self.primary_button(
                Some(crate::icon::name::REFRESH),
                t!("clip.again").to_string(),
                true,
                "generate-again",
                cx,
            ))
            .child(
                crate::ui::secondary_button(
                    Some((crate::icon::name::DOWNLOAD, 0x5F594F)),
                    t!("clip.save_as").to_string(),
                )
                .h(px(38.0))
                .px(px(16.0))
                .text_size(px(13.0))
                .id("save-as")
                .on_click(cx.listener(|this, _, window, cx| this.save_selected_clip(window, cx))),
            )
            .child(
                crate::ui::secondary_button(
                    Some((crate::icon::name::CONTENT_COPY, 0x5F594F)),
                    t!("clip.copy").to_string(),
                )
                .h(px(38.0))
                .px(px(16.0))
                .text_size(px(13.0))
                .id("copy-audio")
                .on_click(cx.listener(|this, _, _, cx| this.copy_selected_clip(cx))),
            )
        } else {
            let ready = self.can_generate(cx);
            row.child(self.primary_button(
                None,
                t!("compose.generate").to_string(),
                ready,
                "generate",
                cx,
            ))
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(theme::hex(0x6B645A))
                    .child(self.generate_hint(cx)),
            )
        };

        row.child(div().flex_1()).child(
            div()
                .h_flex()
                .flex_none()
                .gap(px(7.0))
                .items_center()
                .text_size(px(11.5))
                .text_color(theme::hex(0x857D72))
                .child(crate::icon::icon(
                    crate::icon::name::WIFI_OFF,
                    16.0,
                    theme::hex(0x857D72),
                ))
                .child(t!("workspace.on_this_machine").to_string()),
        )
    }

    fn primary_button(
        &self,
        glyph: Option<&'static str>,
        label: String,
        enabled: bool,
        id: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .h_flex()
            .h(px(38.0))
            .px(px(if glyph.is_some() { 20.0 } else { 16.0 }))
            .gap(px(8.0))
            .flex_none()
            .items_center()
            .rounded(px(8.0))
            .text_size(px(13.0))
            .font_semibold()
            .when(enabled, |d| {
                d.bg(theme::hex(0xFF6E08)).text_color(theme::hex(0xFFFEFD))
            })
            .when(!enabled, |d| {
                d.bg(theme::hex(0xF1EBE1)).text_color(theme::hex(0xB0A79B))
            })
            .when_some(glyph, |d, glyph| {
                d.child(crate::icon::icon(
                    glyph,
                    17.0,
                    if enabled { theme::hex(0xFFFEFD) } else { theme::hex(0xB0A79B) },
                ))
            })
            .child(label)
            .id(SharedString::from(id))
            .on_click(cx.listener(|this, _, _, cx| this.generate(None, cx)))
            .into_any_element()
    }

    pub(crate) fn composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .v_flex()
            .flex_1()
            .min_w(px(0.0))
            .h_full()
            .gap(px(14.0))
            .overflow_hidden()
            .px(px(24.0))
            .py(px(22.0))
            .child(self.composer_header(cx))
            // One card, whatever the state. What changes happens inside it, so
            // the buttons underneath never move.
            .child(
                crate::ui::card()
                    .flex_1()
                    .min_h(px(0.0))
                    .w_full()
                    .child(self.card_strip(cx))
                    .child(self.card_body(cx))
                    .child(self.card_footer(cx)),
            )
            .child(self.card_actions(cx))
    }


}
