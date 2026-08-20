//! Settings, as screen 3d of the design.
//!
//! Everything slower than switching a model lives here. The split is by
//! frequency: the title-bar pill is the switcher for a choice made often, and
//! this window is where models are added and removed, disk is accounted for,
//! and the runtime is named.
//!
//! Deleting says what it costs before it happens — the bytes freed, whether
//! anything still depends on it, and what getting it back would take. A number
//! is only shown when it was measured; nothing here is estimated.

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{ActiveTheme, StyledExt};
use rust_i18n::t;
use speech_engine::{runtime, ModelSpec};

use crate::theme;
use crate::{icon, ui, VoiceStudio};

/// Window geometry from 3d: 900x600 over a lightened workspace.
const WINDOW_WIDTH: f32 = 900.0;
const WINDOW_HEIGHT: f32 = 600.0;
const NAV_WIDTH: f32 = 186.0;

/// Which pane of settings is showing. Only Models is built; the rest are named
/// so the shape of the window is honest about what will live here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    General,
    Models,
    Voices,
    Audio,
    Storage,
    Runtime,
    About,
}

impl Pane {
    fn glyph(self) -> &'static str {
        match self {
            Pane::General => icon::name::TUNE,
            Pane::Models => icon::name::GRAPHIC_EQ,
            Pane::Voices => icon::name::MIC,
            Pane::Audio => icon::name::HEADPHONES,
            Pane::Storage => icon::name::HARD_DRIVE,
            Pane::Runtime => icon::name::MEMORY,
            Pane::About => icon::name::ERROR_OUTLINE,
        }
    }

    fn label(self) -> String {
        match self {
            Pane::General => t!("settings.general"),
            Pane::Models => t!("settings.models"),
            Pane::Voices => t!("settings.voices"),
            Pane::Audio => t!("settings.audio"),
            Pane::Storage => t!("settings.storage"),
            Pane::Runtime => t!("settings.runtime"),
            Pane::About => t!("settings.about"),
        }
        .to_string()
    }

    const ALL: [Pane; 7] = [
        Pane::General,
        Pane::Models,
        Pane::Voices,
        Pane::Audio,
        Pane::Storage,
        Pane::Runtime,
        Pane::About,
    ];
}

fn gigabytes(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f32 / 1e9)
}

impl VoiceStudio {
    fn nav_item(&self, pane: Pane, cx: &mut Context<Self>) -> AnyElement {
        let current = self.settings_pane == pane;
        div()
            .h_flex()
            .items_center()
            .gap(px(9.0))
            .px(px(10.0))
            .py(px(8.0))
            .rounded(px(8.0))
            .text_size(px(12.5))
            .when(current, |d| {
                d.bg(theme::hex(0xFFF3E6)).font_semibold().text_color(theme::hex(0x8F4406))
            })
            .when(!current, |d| d.font_medium().text_color(theme::hex(0x171717)))
            .child(icon::icon(
                pane.glyph(),
                18.0,
                if current { theme::hex(0x8F4406) } else { theme::hex(0x5F594F) },
            ))
            .child(pane.label())
            .id(SharedString::from(format!("pane-{pane:?}")))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.settings_pane = pane;
                this.confirming_voice = None;
                this.confirming_delete = None;
                if pane == Pane::Storage {
                    this.refresh_storage(cx);
                }
                cx.notify();
            }))
            .into_any_element()
    }

    /// How many clips this model made. Derived from the clips themselves, so
    /// "nothing uses it" is a fact about this machine rather than a guess.
    fn clips_from(&self, model_id: &str) -> usize {
        self.clips.iter().filter(|c| c.model == model_id).count()
    }

    /// How many distinct voices this model has actually spoken as. The
    /// conditioning a voice needs is rebuilt per process and never written to
    /// disk, so the durable relationship is the one the clips record.
    fn voices_using(&self, model_id: &str) -> usize {
        let mut seen: Vec<&str> = Vec::new();
        for clip in self.clips.iter().filter(|c| c.model == model_id) {
            if let Some(voice) = clip.voice_id.as_deref() {
                if !seen.contains(&voice) {
                    seen.push(voice);
                }
            }
        }
        seen.len()
    }

    fn installed_row(&self, model: &ModelSpec, cx: &mut Context<Self>) -> AnyElement {
        let id = model.id.clone();
        let current = self.selected_model.as_deref() == Some(id.as_str());

        let mut facts = vec![
            model.label.clone(),
            gigabytes(model.size_bytes),
            model.precision.clone(),
            model.licence.clone(),
        ];
        facts.push(match self.voices_using(&id) {
            0 => t!("settings.used_by_no_voice").to_string(),
            1 => t!("settings.used_by_one_voice").to_string(),
            n => t!("settings.used_by_voices", count = n).to_string(),
        });

        div()
            .h_flex()
            .w_full()
            .items_center()
            .gap(px(12.0))
            .px(px(14.0))
            .py(px(12.0))
            .rounded(px(10.0))
            .bg(theme::surface(false))
            .border_1()
            .border_color(theme::hex(0xEBE4D9))
            .child(
                div()
                    .size(px(20.0))
                    .flex_none()
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(current, |d| d.bg(theme::hex(0xFF6E08)))
                    .when(!current, |d| {
                        d.border_1().border_color(theme::hex(0xC4BBAE))
                    })
                    .when(current, |d| {
                        d.child(
                            div().size(px(7.0)).rounded_full().bg(theme::hex(0xFFFEFD)),
                        )
                    }),
            )
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_w(px(0.0))
                    .child(
                        div()
                            .h_flex()
                            .items_baseline()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .font_family(theme::FONT_DISPLAY)
                                    .text_size(px(13.5))
                                    .font_semibold()
                                    .child(crate::workspace::model_name(model)),
                            )
                            .when(current || model.resident, |d| {
                                d.child(
                                    div()
                                        .text_size(px(11.0))
                                        .text_color(theme::hex(0x8F4406))
                                        .child(if current && model.resident {
                                            t!("settings.default_in_memory").to_string()
                                        } else if current {
                                            t!("settings.default").to_string()
                                        } else {
                                            t!("switch.in_memory").to_string()
                                        }),
                                )
                            }),
                    )
                    .child(ui::mono(facts.join(" · "), 11.5, theme::hex(0x6B645A)).mt(px(2.0))),
            )
            .child(
                // The model in use cannot be deleted out from under the next
                // clip; switching first is the honest order.
                ui::secondary_button(None, t!("settings.delete").to_string())
                    .h(px(30.0))
                    .px(px(11.0))
                    .rounded(px(7.0))
                    .when(current, |d| d.text_color(theme::hex(0xB0A79B)))
                    .id(SharedString::from(format!("del-{id}")))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.selected_model.as_deref() == Some(id.as_str()) {
                            return;
                        }
                        this.confirming_delete = Some(id.clone());
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    /// What deleting costs, stated before it happens.
    fn delete_confirmation(&self, model: &ModelSpec, cx: &mut Context<Self>) -> AnyElement {
        let id = model.id.clone();
        let used = self.clips_from(&id);
        div()
            .h_flex()
            .w_full()
            .items_start()
            .gap(px(11.0))
            .px(px(14.0))
            .py(px(12.0))
            .rounded(px(10.0))
            .bg(theme::hex(0xFFF9F5))
            .border_1()
            .border_color(theme::hex(0xF0B7AF))
            .child(icon::icon(icon::name::DELETE, 18.0, theme::hex(0xC7362B)))
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_w(px(0.0))
                    .child(
                        div()
                            .text_size(px(12.5))
                            .font_semibold()
                            .child(
                                t!(
                                    "settings.delete_frees",
                                    model = crate::workspace::model_name(model),
                                    size = gigabytes(model.size_bytes)
                                )
                                .to_string(),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .line_height(px(18.0))
                            .text_color(theme::hex(0x5F594F))
                            .mt(px(3.0))
                            .child(if used == 0 {
                                t!(
                                    "settings.delete_detail_unused",
                                    size = gigabytes(model.size_bytes)
                                )
                                .to_string()
                            } else {
                                t!(
                                    "settings.delete_detail_used",
                                    count = used,
                                    size = gigabytes(model.size_bytes)
                                )
                                .to_string()
                            }),
                    ),
            )
            .child(
                ui::secondary_button(None, t!("enrol.cancel").to_string())
                    .h(px(30.0))
                    .px(px(11.0))
                    .rounded(px(7.0))
                    .id("cancel-delete")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.confirming_delete = None;
                this.confirming_voice = None;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .h(px(30.0))
                    .px(px(12.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .rounded(px(7.0))
                    .bg(theme::hex(0xC7362B))
                    .text_size(px(12.0))
                    .font_semibold()
                    .text_color(theme::hex(0xFFFEFD))
                    .id("confirm-delete")
                    .on_click(cx.listener(move |this, _, _, cx| this.delete_model(id.clone(), cx)))
                    .child(t!("settings.delete").to_string()),
            )
            .into_any_element()
    }

    fn available_row(&self, model: &ModelSpec, cx: &mut Context<Self>) -> AnyElement {
        let id = model.id.clone();
        let downloading = self.installs.get(&id).filter(|s| s.is_downloading()).cloned();
        div()
            .h_flex()
            .w_full()
            .items_center()
            .gap(px(12.0))
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_w(px(0.0))
                    .child(
                        div()
                            .text_size(px(13.0))
                            .font_medium()
                            .child(crate::workspace::model_name(model)),
                    )
                    // Named and licensed before it is downloaded, not after:
                    // this is the moment the terms can still change the choice.
                    .child(ui::mono(
                        [
                            Some(model.label.clone()),
                            (model.download_bytes > 0).then(|| gigabytes(model.download_bytes)),
                            Some(model.licence.clone()),
                        ]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(" · "),
                        11.5,
                        theme::hex(0x6B645A),
                    ))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(theme::hex(0x857D72))
                            .mt(px(1.0))
                            .child(model.notes.clone()),
                    ),
            )
            .child(match downloading {
                Some(status) => ui::mono(
                    format!(
                        "{:.1} / {:.1} GB",
                        status.downloaded_bytes as f32 / 1e9,
                        status.total_bytes as f32 / 1e9
                    ),
                    11.5,
                    theme::hex(0x8F4406),
                )
                .into_any_element(),
                None => ui::secondary_button(None, t!("model.download").to_string())
                    .h(px(28.0))
                    .px(px(11.0))
                    .rounded(px(7.0))
                    .id(SharedString::from(format!("get-{id}")))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.install_model(id.clone(), cx)
                    }))
                    .into_any_element(),
            })
            .into_any_element()
    }

    fn models_pane(&self, cx: &mut Context<Self>) -> AnyElement {
        let models = self.models.clone();
        let (on_disk, available): (Vec<_>, Vec<_>) =
            models.into_iter().partition(|m| m.installed);
        let free = self.system.as_ref().map(|s| s.free_bytes).unwrap_or(0);
        let confirming = self
            .confirming_delete
            .as_deref()
            .and_then(|id| on_disk.iter().find(|m| m.id == id))
            .cloned();

        div()
            .v_flex()
            .flex_1()
            .h_full()
            .min_w(px(0.0))
            .gap(px(14.0))
            .px(px(22.0))
            .py(px(20.0))
            .overflow_hidden()
            .child(
                div()
                    .h_flex()
                    .w_full()
                    .flex_none()
                    .items_end()
                    .gap(px(12.0))
                    .child(
                        div()
                            .v_flex()
                            .flex_1()
                            .min_w(px(0.0))
                            .child(
                                div()
                                    .font_family(theme::FONT_DISPLAY)
                                    .text_size(px(17.0))
                                    .font_semibold()
                                    .child(t!("settings.models").to_string()),
                            )
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(theme::hex(0x6B645A))
                                    .mt(px(3.0))
                                    .child(t!(
                                        "settings.disk_line",
                                        count = on_disk.len(),
                                        used = gigabytes(
                                            on_disk.iter().map(|m| m.size_bytes).sum::<u64>()
                                        ),
                                        free = gigabytes(free)
                                    )
                                    .to_string()),
                            ),
                    ),
            )
            .child(
                // The list is what gives way when there is not enough height:
                // the disk summary and the runtime line stay put.
                div()
                    .v_flex()
                    .w_full()
                    .flex_1()
                    .min_h(px(0.0))
                    .gap(px(8.0))
                    .id("installed-models")
                    .overflow_y_scroll()
                    .children(on_disk.iter().map(|m| self.installed_row(m, cx)))
                    .children(confirming.map(|m| self.delete_confirmation(&m, cx))),
            )
            .when(!available.is_empty(), |d| {
                d.child(
                    div()
                        .v_flex()
                        .w_full()
                        .gap(px(9.0))
                        .pt(px(14.0))
                        .border_t_1()
                        .border_color(theme::hex(0xEBE4D9))
                        .child(ui::section_label(
                            t!("settings.available").to_string().to_uppercase(),
                        ))
                        .children(available.iter().map(|m| self.available_row(m, cx))),
                )
            })
            // The runtime, named and located, so "what is installed and where"
            // has an answer that does not need a support article.
            .child(
                div()
                    .v_flex()
                    .w_full()
                    .gap(px(4.0))
                    .pt(px(14.0))
                    .border_t_1()
                    .border_color(theme::hex(0xEBE4D9))
                    .child(
                        div()
                            .text_size(px(12.5))
                            .font_medium()
                            .child(format!("{} {}", runtime::NAME, runtime::VERSION)),
                    )
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(ui::mono(
                                speech_engine::paths::data_dir().display().to_string(),
                                11.5,
                                theme::hex(0x6B645A),
                            ))
                            .child(
                                div()
                                    .flex_none()
                                    .child(icon::icon(
                                        icon::name::OPEN_IN_NEW,
                                        15.0,
                                        theme::hex(0x857D72),
                                    ))
                                    .id("reveal-data")
                                    .on_click(|_, _, _| {
                                        crate::reveal::open_folder(
                                            &speech_engine::paths::data_dir(),
                                        );
                                    }),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// One enrolled voice: what it was learned from, what it has made, and the
    /// way to remove it. The sidebar's × is for tidying while you work; this is
    /// where a voice is looked at before it is deleted.
    fn voice_row(&self, voice: &speech_engine::Voice, cx: &mut Context<Self>) -> AnyElement {
        let id = voice.voice_id.clone();
        let current = self.speaking_voice() == Some(id.as_str());
        let clips = self.clips.iter().filter(|c| c.voice_id.as_deref() == Some(id.as_str())).count();
        let asking = self.confirming_voice.as_deref() == Some(id.as_str());

        let mut facts = Vec::new();
        if voice.seconds > 0.0 {
            facts.push(
                t!("workspace.reference_of", time = crate::workspace::duration(voice.seconds))
                    .to_string(),
            );
        }
        facts.push(if clips == 0 {
            t!("settings.unused").to_string()
        } else {
            t!("settings.used_by", count = clips).to_string()
        });

        div()
            .h_flex()
            .w_full()
            .items_center()
            .gap(px(12.0))
            .px(px(14.0))
            .py(px(12.0))
            .rounded(px(10.0))
            .bg(theme::surface(false))
            .border_1()
            .border_color(theme::hex(0xEBE4D9))
            .child(
                div()
                    .size(px(28.0))
                    .flex_none()
                    .rounded_full()
                    .when(current, |d| d.bg(theme::hex(0xFF8A1F)))
                    .when(!current, |d| d.bg(theme::hex(0xE4DCD0)))
                    .flex()
                    .items_center()
                    .justify_center()
                    .font_family(theme::FONT_DISPLAY)
                    .text_size(px(12.0))
                    .font_semibold()
                    .text_color(theme::hex(0x171717))
                    .child(
                        voice
                            .label
                            .chars()
                            .next()
                            .map(|c| c.to_uppercase().to_string())
                            .unwrap_or_default(),
                    ),
            )
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_w(px(0.0))
                    .child(
                        div()
                            .h_flex()
                            .items_baseline()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .font_family(theme::FONT_DISPLAY)
                                    .text_size(px(13.5))
                                    .font_semibold()
                                    .child(voice.label.clone()),
                            )
                            .when(current, |d| {
                                d.child(
                                    div()
                                        .text_size(px(11.0))
                                        .text_color(theme::hex(0x8F4406))
                                        .child(t!("settings.in_use").to_string()),
                                )
                            }),
                    )
                    .child(ui::mono(facts.join(" · "), 11.5, theme::hex(0x6B645A)).mt(px(2.0))),
            )
            // Deleting here says what goes and what stays before it happens,
            // which the sidebar's two-tap cannot.
            .when(asking, |d| {
                d.child(
                    div()
                        .text_size(px(11.5))
                        .line_height(px(16.0))
                        .text_color(theme::hex(0x5F594F))
                        .max_w(px(230.0))
                        .child(if clips == 0 {
                            t!("settings.voice_delete_unused").to_string()
                        } else {
                            t!("settings.voice_delete_used", count = clips).to_string()
                        }),
                )
            })
            .child(
                ui::secondary_button(None, t!("settings.delete").to_string())
                    .h(px(30.0))
                    .px(px(11.0))
                    .rounded(px(7.0))
                    .when(asking, |d| {
                        d.bg(theme::hex(0xC7362B))
                            .border_color(theme::hex(0xC7362B))
                            .text_color(theme::hex(0xFFFEFD))
                    })
                    .id(SharedString::from(format!("vrow-del-{id}")))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.confirming_voice.as_deref() == Some(id.as_str()) {
                            this.delete_voice(id.clone(), cx);
                        } else {
                            this.confirming_voice = Some(id.clone());
                            cx.notify();
                        }
                    })),
            )
            .into_any_element()
    }

    fn voices_pane(&self, cx: &mut Context<Self>) -> AnyElement {
        let voices = self.voices.clone();
        let total: f32 = voices.iter().map(|v| v.seconds).sum();

        div()
            .v_flex()
            .flex_1()
            .h_full()
            .min_w(px(0.0))
            .gap(px(14.0))
            .px(px(22.0))
            .py(px(20.0))
            .overflow_hidden()
            .child(
                div()
                    .v_flex()
                    .flex_none()
                    .child(
                        div()
                            .font_family(theme::FONT_DISPLAY)
                            .text_size(px(17.0))
                            .font_semibold()
                            .child(t!("settings.voices").to_string()),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme::hex(0x6B645A))
                            .mt(px(3.0))
                            .child(t!(
                                "settings.voices_line",
                                count = voices.len(),
                                time = crate::workspace::duration(total)
                            )
                            .to_string()),
                    ),
            )
            .child(
                div()
                    .v_flex()
                    .w_full()
                    .flex_1()
                    .min_h(px(0.0))
                    .gap(px(8.0))
                    .id("voices-list")
                    .overflow_y_scroll()
                    .children(voices.iter().map(|v| self.voice_row(v, cx))),
            )
            // Where the recordings live, and what removing that folder means.
            .child(
                div()
                    .v_flex()
                    .w_full()
                    .flex_none()
                    .gap(px(4.0))
                    .pt(px(14.0))
                    .border_t_1()
                    .border_color(theme::hex(0xEBE4D9))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme::hex(0x5F594F))
                            .child(t!("settings.voices_where").to_string()),
                    )
                    .child(ui::mono(
                        speech_engine::paths::data_dir().join("voices").display().to_string(),
                        11.5,
                        theme::hex(0x6B645A),
                    )),
            )
            .into_any_element()
    }

    /// A pane that is named but not built yet. Better than hiding the row: the
    /// window says what will live here rather than pretending it does not.
    fn placeholder_pane(&self, pane: Pane) -> AnyElement {
        div()
            .v_flex()
            .flex_1()
            .h_full()
            .min_w(px(0.0))
            .gap(px(6.0))
            .px(px(22.0))
            .py(px(20.0))
            .child(
                div()
                    .font_family(theme::FONT_DISPLAY)
                    .text_size(px(17.0))
                    .font_semibold()
                    .child(pane.label()),
            )
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(theme::hex(0x6B645A))
                    .child(t!("settings.not_built").to_string()),
            )
            .into_any_element()
    }

    pub(crate) fn settings_window(&self, cx: &mut Context<Self>) -> AnyElement {
        if !self.settings_open {
            return div().into_any_element();
        }

        div()
            .absolute()
            .inset_0()
            // A light wash, not a dark one: the workspace behind stays legible
            // and the window reads as laid over it rather than as a modal that
            // has switched the app off.
            .bg(gpui::rgba(0xFFF9F28C))
            .flex()
            .items_center()
            .justify_center()
            .id("settings-scrim")
            .on_click(cx.listener(|this, _, _, cx| {
                this.settings_open = false;
                this.confirming_delete = None;
                this.confirming_voice = None;
                cx.notify();
            }))
            .child(
                div()
                    .w(px(WINDOW_WIDTH))
                    .h(px(WINDOW_HEIGHT))
                    .max_w(relative(0.95))
                    // Without this, a click on the nav or a Delete button
                    // bubbles to the scrim behind and dismisses the window —
                    // so every control inside it appeared to do nothing.
                    .occlude()
                    .v_flex()
                    .bg(cx.theme().background)
                    .border_1()
                    .border_color(theme::hex(0xD8D0C4))
                    .rounded(px(12.0))
                    .shadow_lg()
                    .overflow_hidden()
                    // Its own title bar, because this is a window in the
                    // design's terms even though it is drawn inside one.
                    .child(
                        div()
                            .h_flex()
                            .w_full()
                            .h(px(38.0))
                            .flex_none()
                            .items_center()
                            .gap(px(12.0))
                            .px(px(13.0))
                            .bg(theme::bg_subtle(false))
                            .border_b_1()
                            .border_color(theme::hex(0xEBE4D9))
                            .child(
                                div()
                                    .size(px(11.0))
                                    .rounded_full()
                                    .bg(theme::hex(0xFF5F57))
                                    .id("close-settings")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.settings_open = false;
                                        this.confirming_delete = None;
                this.confirming_voice = None;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .mr(px(44.0))
                                    .flex()
                                    .justify_center()
                                    .font_family(theme::FONT_DISPLAY)
                                    .text_size(px(12.5))
                                    .font_semibold()
                                    .child(t!("settings.title").to_string()),
                            ),
                    )
                    .child(
                        div()
                            .h_flex()
                            .flex_1()
                            .min_h(px(0.0))
                            .child(
                                div()
                                    .v_flex()
                                    .w(px(NAV_WIDTH))
                                    .h_full()
                                    .flex_none()
                                    .gap(px(2.0))
                                    .px(px(8.0))
                                    .py(px(12.0))
                                    .bg(theme::bg_subtle(false))
                                    .border_r_1()
                                    .border_color(theme::hex(0xEBE4D9))
                                    .children(
                                        Pane::ALL
                                            .iter()
                                            .take(Pane::ALL.len() - 1)
                                            .map(|p| self.nav_item(*p, cx)),
                                    )
                                    .child(div().flex_1())
                                    .child(self.nav_item(Pane::About, cx)),
                            )
                            .child(match self.settings_pane {
                                Pane::Models => self.models_pane(cx),
                                Pane::Voices => self.voices_pane(cx),
                                Pane::Storage => self.storage_pane(cx),
                                Pane::About => self.about_pane(cx),
                                other => self.placeholder_pane(other),
                            }),
                    ),
            )
            .into_any_element()
    }
}
