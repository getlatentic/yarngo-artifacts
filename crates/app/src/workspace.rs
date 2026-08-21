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
    ActiveTheme, StyledExt,
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
const BAR_RIGHT_PADDING: f32 = 14.0;
const GEAR_SIZE: f32 = 19.0;
/// Space between the model pill and the gear.
const PILL_GEAR_GAP: f32 = 12.0;
/// Tall enough that the lights sit centred rather than crowding the top edge.
pub(crate) const TITLE_BAR_HEIGHT: f32 = 40.0;

pub(crate) fn clock(created: &str) -> String {
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

/// The model's own name, falling back to this app's label for a catalogue
/// entry that has not been given one.
pub(crate) fn model_name(model: &speech_engine::ModelSpec) -> String {
    if model.name.is_empty() { model.label.clone() } else { model.name.clone() }
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
        let setup = !matches!(self.screen(), crate::Screen::Workspace);
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
            // Setup has no model to report, nothing to inspect and no settings
            // worth opening — the catalogue behind them is empty. It says which
            // run this is instead.
            .when(setup, |d| {
                d.child(
                    div()
                        .text_size(px(11.5))
                        .text_color(theme::hex(0x857D72))
                        .child(t!("setup.first_run").to_string()),
                )
            })
            // Recording takes the title bar over: the model is not the state
            // that matters while the microphone is live, and a red dot is
            // visible from across the room.
            .when(!setup && matches!(self.enrolment, crate::Enrolment::Recording), |d| {
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
            .when(!setup && !matches!(self.enrolment, crate::Enrolment::Recording), |this| {
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
                    .border_color(theme::hex(theme::RULE))
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
                        Some(m) => format!("{} · {state}", model_name(m)),
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
                            .when(self.inspector, |d| {
                                d.bg(theme::hex(0xFFF3E6)).border_color(theme::hex(0xFFCB93))
                            })
                            .when(!self.inspector, |d| d.border_color(theme::hex(0xE4DCD0)))
                            .child(crate::icon::icon(
                                if self.inspector {
                                    crate::icon::name::PANEL_CLOSE
                                } else {
                                    crate::icon::name::PANEL_OPEN
                                },
                                18.0,
                                if self.inspector {
                                    theme::hex(0x8F4406)
                                } else {
                                    theme::hex(0x857D72)
                                },
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
                                theme::hex(0x5F594F),
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
        filled: bool,
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
            .child(div().flex_none().child(if filled {
                crate::icon::filled(glyph, 17.0, theme::hex(glyph_colour)).into_any_element()
            } else {
                crate::icon::icon(glyph, 17.0, theme::hex(glyph_colour)).into_any_element()
            }))
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
                        .when(accent, |d| d.opacity(0.8))
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
            // The draft the app opens on is not a clip yet — the list shows its
            // own empty state instead. Every other draft is one from the moment
            // it is made, which is what gives New clip something to point at.
            if !draft.started && !draft.generating {
                continue;
            }
            let id = draft.id.clone();
            let current = self.selected == crate::clips::Selected::Draft(id.clone());
            let running = draft.generating;
            let detail = match (running, self.seconds_left()) {
                (true, Some(left)) => {
                    t!("clip.generating_left", seconds = format!("{left:.0}")).to_string()
                }
                (true, None) => t!("clip.generating_now").to_string(),
                (false, _) => t!("clip.draft").to_string(),
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
                    false,
                )
                // A running row is taller by the height of its own progress
                // bar. At the fixed 52px the bar sat on top of the line that
                // says "generating", clipping it.
                .when(running, |d| d.h(px(62.0)))
                .when(running, |d| {
                    d.child(
                        div()
                            .absolute()
                            .left(px(9.0))
                            .right(px(9.0))
                            .bottom(px(8.0))
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
            let current = matches!(
                &self.selected,
                crate::clips::Selected::Clip(selected, _) if selected == &id
            );
            let sounding = clip
                .takes
                .iter()
                .any(|t| self.playing_clip() == Some(t.path.as_path()))
                && self.is_playing();
            let length = clip.latest().map(|t| t.audio_s).unwrap_or(0.0);
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
                        duration(length),
                        self.voice_name(clip.voice_id.as_deref())
                    ),
                    current,
                    current,
                    current,
                )
                .when(current, |d| {
                    let menu_id = id.clone();
                    d.child(
                        div()
                            .flex_none()
                            .child(crate::icon::icon(
                                crate::icon::name::MORE_HORIZ,
                                17.0,
                                theme::hex(0x8F4406),
                            ))
                            .id(SharedString::from(format!("menu-{menu_id}")))
                            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                                this.clip_menu = Some((menu_id.clone(), event.position()));
                                cx.notify();
                            })),
                    )
                })
                .id(SharedString::from(format!("c-{id}")))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_clip(id.clone(), window, cx)
                }))
                .into_any_element(),
            );
        }

        rows
    }

    /// Rename, duplicate and delete, on the row rather than in the panel — a
    /// clip you want to act on is one you can see in the list, not necessarily
    /// the one open in the composer.
    pub(crate) fn clip_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some((id, at)) = self.clip_menu.clone() else {
            return div().into_any_element();
        };

        fn item(label: String, danger: bool) -> Div {
            div()
                .h_flex()
                .w_full()
                .h(px(30.0))
                .items_center()
                .px(px(10.0))
                .rounded(px(6.0))
                .text_size(px(12.5))
                .text_color(if danger { theme::hex(0xC7362B) } else { theme::hex(0x171717) })
                .child(label)
        }

        div()
            .absolute()
            .inset_0()
            // A click anywhere else puts the menu away, which is what makes it
            // safe to open one from a row without committing to anything.
            .id("clip-menu-scrim")
            .on_click(cx.listener(|this, _, _, cx| {
                this.clip_menu = None;
                cx.notify();
            }))
            .child(
                deferred(
                    anchored().position(at).child(
                        div()
                            .v_flex()
                            .w(px(180.0))
                            .gap(px(2.0))
                            .p(px(6.0))
                            .rounded(px(10.0))
                            .bg(theme::hex(0xFFFDFA))
                            .border_1()
                            .border_color(theme::hex(0xE4DCD0))
                            .shadow_lg()
                            .occlude()
                            .child(
                                item(t!("clip.rename").to_string(), false)
                                    .id("menu-rename")
                                    .on_click(cx.listener({
                                        let id = id.clone();
                                        move |this, _, window, cx| {
                                            this.clip_menu = None;
                                            this.select_clip(id.clone(), window, cx);
                                            this.begin_rename(window, cx);
                                        }
                                    })),
                            )
                            .child(
                                item(t!("clip.duplicate").to_string(), false)
                                    .id("menu-duplicate")
                                    .on_click(cx.listener({
                                        let id = id.clone();
                                        move |this, _, _, cx| {
                                            this.clip_menu = None;
                                            this.duplicate_clip(id.clone(), cx);
                                        }
                                    })),
                            )
                            .child(div().h(px(1.0)).my(px(3.0)).bg(theme::hex(0xEBE4D9)))
                            .child(
                                item(t!("inspect.delete_clip").to_string(), true)
                                    .id("menu-delete")
                                    .on_click(cx.listener({
                                        let id = id.clone();
                                        move |this, _, window, cx| {
                                            this.clip_menu = None;
                                            this.delete_selected_clip(id.clone(), window, cx);
                                        }
                                    })),
                            ),
                    ),
                )
                .with_priority(1),
            )
            .into_any_element()
    }

    /// The sidebar lists clips and nothing else. A voice belongs to the clip
    /// being made, not beside the work, so it lives in the inspector.
    pub(crate) fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // The empty state belongs to a list with nothing in it — including the
        // draft rows, which now appear as soon as a clip is started.
        let empty = self.clips.is_empty() && !self.drafts.iter().any(|d| d.started || d.generating);
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
                                .my(px(6.0))
                                .mx(px(4.0))
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
                            // While something is running that is the fact worth
                            // stating; the rest of the time it is the disk.
                            .when(self.busy(), |d| {
                                d.child(crate::icon::icon(
                                    crate::icon::name::GRAPHIC_EQ,
                                    15.0,
                                    theme::hex(0xFF8A1F),
                                ))
                                .child(t!("clip.one_generating").to_string())
                                .text_color(theme::hex(0x8F4406))
                            })
                            .when(!self.busy(), |d| {
                                d.child(crate::icon::icon(
                                    crate::icon::name::HARD_DRIVE,
                                    15.0,
                                    theme::hex(0x857D72),
                                ))
                                .child({
                                    let n = self.clips.len();
                                    let key =
                                        if n == 1 { "clip.one_on_disk" } else { "clip.n_on_disk" };
                                    format!(
                                        "{} · {:.0} MB",
                                        t!(key, count = n),
                                        self.clips_bytes() as f32 / 1e6
                                    )
                                })
                                .text_color(theme::hex(0x6B645A))
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
        // A draft with nothing in it is not yet a clip to name — the design
        // calls it "New clip" and offers no pencil until there is something.
        // Only the opening composer is "New clip"; a clip that has been started
        // is "Untitled clip" until it is named, and can be renamed.
        let fresh = self.draft().is_some_and(|d| !d.started && !d.generating);
        let title = match &self.selected {
            crate::clips::Selected::Draft(_) if fresh => t!("clip.fresh_title").to_string(),
            crate::clips::Selected::Draft(_) => {
                self.draft().map(|d| d.title()).unwrap_or_default()
            }
            crate::clips::Selected::Clip(..) => {
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
                                        .h_flex()
                                        .h(px(26.0))
                                        .min_w(px(300.0))
                                        .items_center()
                                        .px(px(9.0))
                                        .rounded(px(7.0))
                                        .bg(theme::surface(false))
                                        .border_2()
                                        .border_color(theme::hex(0x171717))
                                        .font_family(theme::FONT_DISPLAY)
                                        .text_size(px(15.0))
                                        .font_semibold()
                                        // Its own frame, so the component's
                                        // border and focus ring do not draw a
                                        // second box inside this one.
                                        .key_context(crate::RENAME_CONTEXT)
                                        .child(Input::new(&self.clip_name).appearance(false)),
                                )
                                .child(
                                    div()
                                        .h_flex()
                                        .flex_none()
                                        .items_baseline()
                                        .gap(px(4.0))
                                        .text_size(px(11.5))
                                        .text_color(theme::hex(0x6B645A))
                                        .child(Self::key_cap("Enter"))
                                        .child(t!("clip.rename_save").to_string())
                                        .child(Self::key_cap("Esc"))
                                        .child(t!("clip.rename_cancel").to_string()),
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
                                .when(!fresh, |d| {
                                    d.child(
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
                                })
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

    /// A key name inside a sentence, in the mono face the design sets it in.
    fn key_cap(key: &'static str) -> Div {
        div()
            .font_family(theme::FONT_MONO)
            .text_size(px(11.0))
            .font_semibold()
            .text_color(theme::hex(0x171717))
            .child(key)
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

    /// What the model is called. `label` is what this app recommends it for,
    /// which is the right thing to read while choosing one and the wrong thing
    /// to read afterwards — a clip was made by dots.tts MF, not by "Fast".
    pub(crate) fn model_label(&self) -> String {
        // No model chosen yet is a draft, and a draft has nothing to name.
        let Some(id) = self.clip_model() else { return String::new() };
        self.models
            .iter()
            .find(|m| m.id == id)
            .map(model_name)
            // A clip records what actually made it and that record is never
            // rewritten, so an id with no catalogue entry is a real state: the
            // model was removed, or the clip came from a machine running a
            // different backend. The id is the truest thing left to show, and
            // showing nothing — which is what this did — reads as a bug.
            .unwrap_or_else(|| id.to_string())
    }

    /// Whether the model a clip names is one this machine can offer. False for
    /// a model since uninstalled, and for every clip carried over from a
    /// platform whose backend has a different catalogue.
    pub(crate) fn model_available(&self) -> bool {
        self.clip_model().is_none_or(|id| self.models.iter().any(|m| m.id == id))
    }

    /// One line under the name saying what this clip is set up with, or what it
    /// was made with once it exists.
    fn composer_subtitle(&self) -> String {
        let voice = self.voice_name(self.clip_voice());
        let model = self.model_label();
        match (self.clip(), self.take()) {
            (Some(_), Some(take)) => t!(
                "clip.made_line",
                length = duration(take.audio_s),
                at = clock(&take.created),
                voice = voice,
                model = model
            )
            .to_string(),
            _ => format!("{voice} · {model}"),
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

        if self.showing_generation() {
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

        match self.take().cloned() {
            Some(take) => {
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
                            format!("{position} / {}", duration(take.audio_s)),
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
                            .child(match self.seconds_left() {
                                Some(left) => t!(
                                    "compose.generating_left",
                                    seconds = format!("{left:.0}")
                                )
                                .to_string(),
                                None => t!("compose.generating").to_string(),
                            }),
                    )
                    .child(
                        crate::ui::mono(
                            match rtf {
                                // Chunks done, and seconds actually written.
                                // Both are counted. The old line divided by a
                                // word-count estimate and so could read "163
                                // of 149 seconds".
                                Some(rtf) => {
                                    let p = self.progress.as_ref();
                                    t!(
                                        "compose.written_of",
                                        done = p.map(|p| p.chunks_done).unwrap_or(0).to_string(),
                                        chunks = p.map(|p| p.chunks).unwrap_or(0).to_string(),
                                        written = format!("{written:.0}"),
                                        rtf = realtime(rtf)
                                    )
                                    .to_string()
                                }
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

        if self.showing_generation() {
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
                        .line_height(px(25.5))
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
                            crate::ui::secondary_button_sized(
                                Some((crate::icon::name::EDIT, 0x5F594F)),
                                t!("clip.edit_text").to_string(),
                                15.0,
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
                    // Bounded and scrollable. Unbounded, a long clip's words ran
                    // past the card and painted over the line beneath it — the
                    // composer had the same fault and this branch was missed,
                    // because only the editable one was fixed.
                    //
                    // A scroll viewport cuts wherever it happens to land, and a
                    // line of text sliced through the middle reads as broken
                    // rather than as "there is more". So the last few pixels
                    // fade into the card instead: the cut stops being an edge
                    // and becomes the usual signal that the text continues.
                    div()
                        .relative()
                        .flex_1()
                        .min_h(px(0.0))
                        .child(
                            div()
                                .id("clip-text")
                                .size_full()
                                .overflow_y_scroll()
                                // Room at the end, so scrolling to the bottom
                                // finishes on whitespace, under the fade.
                                .pb(px(20.0))
                                .text_size(px(15.0))
                                .line_height(px(25.5))
                                .text_color(theme::hex(0x171717))
                                .child(clip.text.clone()),
                        )
                        .child(
                            // No id and no occlude, so it never takes the
                            // scroll it is drawn over.
                            div()
                                .absolute()
                                .bottom_0()
                                .left_0()
                                .right_0()
                                .h(px(28.0))
                                .bg(gpui::linear_gradient(
                                    180.0,
                                    gpui::linear_color_stop(
                                        theme::surface(false).opacity(0.0),
                                        0.0,
                                    ),
                                    gpui::linear_color_stop(theme::surface(false), 1.0),
                                )),
                        ),
                ),
            None => body.child(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    // Without this the field paints over the count and the
                    // border beneath it once the text is longer than the box:
                    // gpui does not clip to the box on its own.
                    .overflow_hidden()
                    .pb(px(12.0))
                    .text_size(px(15.0))
                    .line_height(px(25.5))
                    .child(Textarea::new(&self.text).appearance(false).h_full()),
            ),
        }
    }

    /// The row under the words: what they cost, or what editing them means.
    fn card_footer(&self, cx: &mut Context<Self>) -> Div {
        let text = self.text.read(cx).value().to_string();
        let words = text.split_whitespace().count();
        let spoken = words as f32 / WORDS_PER_SECOND;
        // Whether *this* clip is the one running, not whether anything is.
        let running_here = self.showing_generation();

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
            // Renaming touches nothing but the label, and the design says so
            // here rather than leaving 4d's warning in place.
            .when(self.renaming.is_some(), |d| {
                d.child(t!("clip.rename_note").to_string())
            })
            .when(self.renaming.is_none() && self.clip().is_some() && !running_here, |d| {
                d.child(t!("clip.editing_makes_take").to_string())
            })
            .when(self.renaming.is_none() && (self.clip().is_none() || running_here), |d| {
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

        let row = if self.showing_generation() {
            // Stopping is the action that belongs to a running clip. Starting
            // another was offered here as the primary control, which reads as
            // the thing to press and is not — the run is already listed in the
            // sidebar, and you can leave it by clicking any other row.
            row.child(
                crate::ui::secondary_button(
                    Some((crate::icon::name::CLOSE, 0x5F594F)),
                    t!("clip.cancel").to_string(),
                )
                .h(px(38.0))
                .px(px(16.0))
                .text_size(px(13.0))
                .id("cancel-generation")
                .on_click(cx.listener(|this, _, _, cx| this.cancel_generation(cx))),
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

        // Stated where there is room for it and nothing more urgent to say:
        // fully on the empty screen, briefly while writing, not at all once the
        // card is generating or holding a finished clip.
        let empty = self.text.read(cx).value().trim().is_empty();
        let note = (!self.busy() && self.clip().is_none()).then(|| {
            if empty {
                t!("workspace.offline").to_string()
            } else {
                t!("workspace.on_this_machine").to_string()
            }
        });

        row.child(div().flex_1()).when_some(note, |row, note| {
            row.child(
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
                    .child(note),
            )
        })
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
            .px(px(if glyph.is_some() || enabled { 20.0 } else { 16.0 }))
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
