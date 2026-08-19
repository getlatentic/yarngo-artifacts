//! Choosing a model, as screen 1b of the design.
//!
//! One model per row, with the licence and the download size where a marketing
//! blurb would normally go. Both are commitments the user is making — gigabytes
//! of disk, and terms they inherit for anything they generate — so both are read
//! before the download starts rather than found afterwards.
//!
//! The same screen is the model switcher later, reached from the title bar, so
//! there is one place that answers "what is on this machine and what would
//! another one cost".

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{ActiveTheme, StyledExt};
use rust_i18n::t;
use speech_engine::ModelSpec;

use crate::theme;
use crate::{icon, ui, VoiceStudio};

/// Matches the enrolment column, so the two setup steps line up.
const CONTENT_WIDTH: f32 = 860.0;

/// Gigabytes, or nothing at all. A model whose size has never been looked up
/// says so rather than showing a plausible number.
fn gigabytes(bytes: u64) -> Option<String> {
    (bytes > 0).then(|| format!("{:.1} GB", bytes as f32 / 1e9))
}

/// A figure in the unit that fits it. Half a gigabyte reads as `493 MB`; only
/// past a gigabyte is `1.2 GB` the shorter truth.
fn size_figure(bytes: u64) -> String {
    if bytes < 1_000_000_000 {
        format!("{:.0} MB", bytes as f32 / 1e6)
    } else {
        format!("{:.1} GB", bytes as f32 / 1e9)
    }
}

impl VoiceStudio {
    /// One fact about a model, in the small mono face the design uses for
    /// figures: size, licence, precision, measured speed.
    fn meta(&self, text: String) -> AnyElement {
        ui::mono(text, 11.5, theme::hex(0x6B645A)).into_any_element()
    }

    fn model_row(&self, model: &ModelSpec, cx: &mut Context<Self>) -> AnyElement {
        let id = model.id.clone();
        let selected = self.selected_model.as_deref() == Some(id.as_str());
        let downloading = self.installs.get(&id).filter(|s| s.is_downloading()).cloned();

        let mut facts = vec![
            self.meta(model.licence.clone()),
            self.meta(model.precision.clone()),
        ];
        // Installed models report what they occupy; the rest, what they cost.
        if let Some(size) = gigabytes(if model.installed {
            model.size_bytes
        } else {
            model.download_bytes
        }) {
            facts.insert(0, self.meta(size));
        }
        if let Some(rtf) = model.measured_rtf {
            facts.push(self.meta(
                t!("model.measured", rtf = crate::workspace::realtime(rtf)).to_string(),
            ));
        }
        // The capability that decides whether enrolment is worth the user's
        // time. Stated on the row, because discovering it at Generate would be
        // after they had already recorded themselves.
        facts.push(self.meta(
            if model.supports_cloning {
                t!("model.can_clone")
            } else {
                t!("model.own_voice_only")
            }
            .to_string(),
        ));

        ui::card()
            .w_full()
            .when(selected, |d| d.border_2().border_color(theme::hex(0x171717)))
            .child(
                div()
                    .h_flex()
                    .w_full()
                    .items_start()
                    .gap(px(14.0))
                    .px(px(16.0))
                    .py(px(12.0))
                    .child(
                        div()
                            .v_flex()
                            .flex_1()
                            .min_w(px(0.0))
                            .gap(px(7.0))
                            .child(
                                div()
                                    .h_flex()
                                    .items_center()
                                    .gap(px(9.0))
                                    .child(
                                        div()
                                            .font_family(theme::FONT_DISPLAY)
                                            .text_size(px(15.0))
                                            .font_semibold()
                                            .child(crate::workspace::model_name(model)),
                                    )
                                    // What this app recommends it for, beside
                                    // what it is — a characterisation, and it
                                    // reads as one.
                                    .child(
                                        div()
                                            .px(px(9.0))
                                            .py(px(2.0))
                                            .rounded(px(999.0))
                                            .bg(theme::bg_subtle(false))
                                            .border_1()
                                            .border_color(theme::hex(0xE4DCD0))
                                            .text_size(px(10.5))
                                            .font_medium()
                                            .text_color(theme::hex(0x5F594F))
                                            .child(model.label.clone()),
                                    )
                                    .when(model.default, |d| {
                                        d.child(
                                            div()
                                                .px(px(9.0))
                                                .py(px(2.0))
                                                .rounded(px(999.0))
                                                .bg(theme::hex(0xFFF3E6))
                                                .border_1()
                                                .border_color(theme::hex(0xFFCB93))
                                                .text_size(px(10.5))
                                                .font_semibold()
                                                .text_color(theme::hex(0x8F4406))
                                                .child(t!("model.recommended").to_string()),
                                        )
                                    })
                                    .when(model.installed, |d| {
                                        d.child(icon::icon(
                                            icon::name::CHECK,
                                            15.0,
                                            cx.theme().success,
                                        ))
                                    }),
                            )
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .line_height(px(19.0))
                                    .max_w(px(630.0))
                                    .text_color(theme::hex(0x5F594F))
                                    .child(model.notes.clone()),
                            )
                            .child(div().h_flex().flex_wrap().gap(px(14.0)).children(facts)),
                    )
                    .child(match (&downloading, model.installed) {
                        // A download owns the row while it runs: progress
                        // matters more than a button that would do nothing.
                        (Some(status), _) => {
                            div()
                                .v_flex()
                                .w(px(196.0))
                                .flex_none()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .w_full()
                                        .h(px(6.0))
                                        .rounded_full()
                                        .bg(theme::hex(0xEBE4D9))
                                        .child(
                                            div()
                                                .h_full()
                                                .rounded_full()
                                                .bg(theme::hex(0xFF8A1F))
                                                .w(relative(status.fraction())),
                                        ),
                                )
                                .child(ui::mono(
                                    format!(
                                        "{} / {} · {:.0}%",
                                        size_figure(status.downloaded_bytes),
                                        size_figure(status.total_bytes),
                                        status.fraction() * 100.0
                                    ),
                                    11.5,
                                    theme::hex(0x6B645A),
                                ))
                                .into_any_element()
                        }
                        (None, true) => ui::secondary_button(
                            None,
                            if selected {
                                t!("model.in_use").to_string()
                            } else {
                                t!("model.use").to_string()
                            },
                        )
                        .flex_none()
                        .id(SharedString::from(format!("use-{id}")))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.select_model(id.clone(), cx)
                        }))
                        .into_any_element(),
                        (None, false) => ui::secondary_button(
                            Some((icon::name::DOWNLOAD, 0x5F594F)),
                            t!("model.download").to_string(),
                        )
                        .flex_none()
                        .id(SharedString::from(format!("get-{id}")))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.install_model(id.clone(), cx)
                        }))
                        .into_any_element(),
                    }),
            )
            .into_any_element()
    }

    /// Ask the hub what the downloads cost. Deliberate and one-shot: the
    /// catalogue is readable offline without it, just without sizes.
    pub(crate) fn refresh_model_sizes(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.engine.clone() else { return };
        if self.models.iter().all(|m| m.download_bytes > 0 || m.installed) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let models = cx.background_spawn(async move { engine.models_with_sizes() }).await;
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

    pub(crate) fn models_screen(
        &mut self,
        _window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let rows: Vec<AnyElement> =
            self.models.clone().iter().map(|m| self.model_row(m, cx)).collect();
        let installed = self.installed_models();
        let free = self.system.as_ref().map(|s| s.free_bytes as f32 / 1e9);

        div()
            .v_flex()
            .flex_1()
            .min_h(px(0.0))
            .bg(cx.theme().background)
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_h(px(0.0))
                    .items_center()
                    .id("models-scroll")
                    .overflow_y_scroll()
                    .pt(px(22.0))
                    .px(px(40.0))
                    .child(
                        div()
                            .v_flex()
                            .w_full()
                            .max_w(px(CONTENT_WIDTH))
                            .gap(px(14.0))
                            .pb(px(24.0))
                            .child(self.stepper(2, cx))
                            .child(
                                div()
                                    .v_flex()
                                    .gap(px(4.0))
                                    .child(
                                        div()
                                            .font_family(theme::FONT_DISPLAY)
                                            .text_size(px(20.0))
                                            .font_semibold()
                                            .child(t!("model.title").to_string()),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(12.5))
                                            .line_height(px(19.0))
                                            .max_w(px(720.0))
                                            .text_color(theme::hex(0x5F594F))
                                            .child(t!("model.subtitle").to_string()),
                                    ),
                            )
                            .child(div().v_flex().w_full().gap(px(8.0)).children(rows)),
                    ),
            )
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
                            .child(match free {
                                Some(gb) => {
                                    t!("model.free_after", gb = format!("{gb:.0}")).to_string()
                                }
                                None => t!("model.background").to_string(),
                            }),
                    )
                    .child(div().flex_1())
                    .child(
                        // Nothing to continue to until something can speak, so
                        // the step gates on disk rather than on a click.
                        div()
                            .h(px(36.0))
                            .px(px(18.0))
                            .flex()
                            .items_center()
                            .rounded(px(8.0))
                            .when(installed > 0, |d| {
                                d.bg(theme::hex(0xFF6E08)).text_color(theme::hex(0xFFFEFD))
                            })
                            .when(installed == 0, |d| {
                                d.bg(theme::hex(0xF1EBE1)).text_color(theme::hex(0xB0A79B))
                            })
                            .text_size(px(13.0))
                            .font_semibold()
                            .id("models-continue")
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.installed_models() == 0 {
                                    return;
                                }
                                this.choosing_model = false;
                                // First run continues into enrolment; a later
                                // visit returns to the workspace it came from.
                                if this.voices.is_empty() && this.last.is_none() {
                                    this.begin_enrolment(window, cx);
                                    this.in_setup = true;
                                }
                                cx.notify();
                            }))
                            .child(t!("model.continue").to_string()),
                    ),
            )
    }
}
