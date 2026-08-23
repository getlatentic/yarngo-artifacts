//! The model switcher that drops out of the title-bar pill, as screen 3c.
//!
//! Two things the design insists on, both about cost:
//!
//! * **Downloaded and not-downloaded are separate lists.** Picking something
//!   already on disk and committing several gigabytes are different decisions,
//!   so they do not sit in one undifferentiated column.
//! * **Switching states its price.** A resident model reads "in memory"; one on
//!   disk reads how long it took to load here last time. The footnote says what
//!   a switch does and does not touch, because "will this re-cut my clips?" is
//!   the question a switcher raises and rarely answers.
//!
//! Hand-positioned rather than built on the component library's `Popover`: that
//! one builds its content under `PopoverState`, so every row would reach this
//! view through a weak handle, and the panel's geometry here is measured from
//! the design rather than inherited.

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::StyledExt;
use rust_i18n::t;
use speech_engine::ModelSpec;

use crate::theme;
use crate::{icon, ui, VoiceStudio};

/// Panel geometry, read off the design: 351 wide, hanging 8px under the bar and
/// right-aligned with the pill rather than with the window.
const PANEL_WIDTH: f32 = 352.0;
/// The panel hangs 6px under a 40px bar, landing its top edge at 46.
const PANEL_GAP: f32 = 6.0;
/// Distance from the window's right edge to the pill's right edge: the bar's
/// own padding, plus the gear button and the space before it.
pub(crate) const PILL_RIGHT_INSET: f32 = 44.0;

fn gigabytes(bytes: u64) -> Option<String> {
    (bytes > 0).then(|| format!("{:.1} GB", bytes as f32 / 1e9))
}

impl VoiceStudio {
    /// The line under a model's name: what it occupies, and how fast it runs
    /// here. Only measured figures appear — a model never used on this machine
    /// shows its size alone rather than a borrowed benchmark.
    fn model_facts(&self, model: &ModelSpec) -> String {
        let size = gigabytes(if model.installed { model.size_bytes } else { model.download_bytes });
        let speed = model
            .measured_rtf
            .map(|rtf| {
                t!("model.measured", rtf = crate::workspace::realtime(rtf)).to_string()
            });
        match (size, speed) {
            (Some(size), Some(speed)) => format!("{size} · {speed}"),
            (Some(size), None) => size,
            (None, Some(speed)) => speed,
            (None, None) => String::new(),
        }
    }

    /// What choosing this model costs right now.
    fn switch_cost(&self, model: &ModelSpec) -> String {
        if model.resident {
            t!("switch.in_memory").to_string()
        } else {
            match model.load_s {
                Some(seconds) => t!("switch.loads_in", seconds = format!("{seconds:.0}")).to_string(),
                None => t!("switch.on_disk").to_string(),
            }
        }
    }

    fn switch_row(&self, model: &ModelSpec, cx: &mut Context<Self>) -> AnyElement {
        let id = model.id.clone();
        let chosen = self.selected_model.as_deref() == Some(id.as_str());
        let installed = model.installed;
        let downloading = self.installs.get(&id).is_some_and(|s| s.is_downloading());

        div()
            .h_flex()
            .w_full()
            .items_start()
            .gap(px(10.0))
            .px(px(8.0))
            .py(px(9.0))
            .rounded(px(8.0))
            // The current model is the only filled row, so the eye finds it
            // before reading any of the names.
            .when(chosen, |d| d.bg(theme::hex(0xFFF3E6)))
            .child(
                div()
                    .size(px(18.0))
                    .flex_none()
                    .mt(px(1.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(match (installed, chosen, downloading) {
                        // Downloading reads as motion, not as a choice.
                        (_, _, true) => {
                            icon::icon(icon::name::DOWNLOADING, 18.0, theme::hex(0x8F4406))
                                .into_any_element()
                        }
                        (true, true, _) => {
                            icon::icon(icon::name::CHECK_CIRCLE, 18.0, theme::hex(0x8F4406))
                                .into_any_element()
                        }
                        (true, false, _) => icon::icon(
                            icon::name::RADIO_UNCHECKED,
                            18.0,
                            theme::hex(0xB0A79B),
                        )
                        .into_any_element(),
                        (false, _, _) => {
                            icon::icon(icon::name::DOWNLOAD, 18.0, theme::hex(0xB0A79B))
                                .into_any_element()
                        }
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
                            .gap(px(7.0))
                            .child(
                                div()
                                    .when(installed, |d| d.text_size(px(13.0)))
                                    .when(!installed, |d| {
                                        d.text_size(px(12.5)).text_color(theme::hex(0x5F594F))
                                    })
                                    .when(chosen, |d| d.font_semibold())
                                    .when(!chosen, |d| d.font_medium())
                                    .child(crate::workspace::model_name(model)),
                            )
                            .when(installed, |d| {
                                d.child(
                                    div()
                                        .text_size(px(10.5))
                                        .when(chosen, |d| d.text_color(theme::hex(0x8F4406)))
                                        .when(!chosen, |d| d.text_color(theme::hex(0x857D72)))
                                        .child(self.switch_cost(model)),
                                )
                            }),
                    )
                    .child(
                        ui::mono(
                            self.model_facts(model),
                            if installed { 11.5 } else { 11.0 },
                            if installed { theme::hex(0x6B645A) } else { theme::hex(0x857D72) },
                        )
                        .mt(px(2.0)),
                    ),
            )
            .id(SharedString::from(format!("switch-{id}")))
            .on_click(cx.listener(move |this, _, _, cx| {
                if installed {
                    this.select_model(id.clone(), cx);
                    this.model_menu = false;
                } else {
                    // Downloading keeps the menu open: the row it started is
                    // where the progress appears.
                    this.install_model(id.clone(), cx);
                }
                cx.notify();
            }))
            .into_any_element()
    }

    fn switch_section(
        &self,
        label: String,
        models: Vec<ModelSpec>,
        ruled: bool,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if models.is_empty() {
            return None;
        }
        Some(
            div()
                .v_flex()
                .w_full()
                .child(
                    div()
                        .px(px(14.0))
                        .py(px(9.0))
                        .when(ruled, |d| {
                            d.border_t_1().border_color(theme::hex(0xF1EBE1))
                        })
                        .child(ui::section_label(label.to_uppercase())),
                )
                .children(models.iter().map(|m| self.switch_row(m, cx)))
                .into_any_element(),
        )
    }

    /// The panel itself, positioned under the pill. Rendered by the root view
    /// over everything else, so it is not clipped by the title bar's height.
    pub(crate) fn model_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        if !self.model_menu {
            return div().into_any_element();
        }
        let models = self.models.clone();
        let (on_disk, absent): (Vec<_>, Vec<_>) =
            models.into_iter().partition(|m| m.installed);

        let panel = div()
            .absolute()
            .top(px(crate::workspace::TITLE_BAR_HEIGHT + PANEL_GAP))
            .right(px(PILL_RIGHT_INSET))
            .w(px(PANEL_WIDTH))
            .v_flex()
            .py(px(4.0))
            .px(px(8.0))
            .bg(theme::hex(0xFFFDFA))
            .border_1()
            .border_color(theme::hex(0xE4DCD0))
            .rounded(px(12.0))
            .shadow_lg()
            .children(self.switch_section(t!("switch.on_machine").to_string(), on_disk, false, cx))
            .children(self.switch_section(
                t!("switch.not_downloaded").to_string(),
                absent,
                true,
                cx,
            ))
            // The question a switcher provokes, answered before it is asked.
            .child(
                div()
                    .px(px(14.0))
                    .pt(px(11.0))
                    .border_t_1()
                    .border_color(theme::hex(0xF1EBE1))
                    .text_size(px(11.5))
                    .line_height(px(17.0))
                    .text_color(theme::hex(0x6B645A))
                    .child(t!("switch.explain").to_string()),
            )
            .child(
                div()
                    .h_flex()
                    .w_full()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(14.0))
                    .py(px(11.0))
                    .rounded(px(8.0))
                    .child(icon::icon(icon::name::SETTINGS, 17.0, theme::hex(0x5F594F)))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .font_semibold()
                            .child(t!("switch.manage").to_string()),
                    )
                    .id("manage-models")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.model_menu = false;
                        this.settings_open = true;
                        this.look_for_runtime_update(cx);
                        this.settings_pane = crate::settings::Pane::Models;
                        this.refresh_model_sizes(cx);
                        cx.notify();
                    })),
            );

        div()
            .absolute()
            .inset_0()
            // A transparent catcher, so clicking anywhere else dismisses the
            // menu the way every other dropdown on this platform does.
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .id("model-menu-scrim")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.model_menu = false;
                        cx.notify();
                    })),
            )
            .child(panel)
            .into_any_element()
    }
}
