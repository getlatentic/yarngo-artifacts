//! What the app has put on this machine, and where.
//!
//! The app writes gigabytes — a Python runtime, model weights, reference
//! recordings, and every clip ever generated. Scattered across the interface
//! that is four different numbers in four different places; here it is one
//! account, in the order things get large.
//!
//! Sizes are measured, not estimated, and measured off the UI thread: the
//! runtime alone is tens of thousands of files.

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::StyledExt;
use rust_i18n::t;

use crate::{icon, theme, ui, VoiceStudio};

/// What each part of the data directory occupies. `None` until it has been
/// walked, so the pane can say it is counting rather than claim zero.
#[derive(Clone, Copy, Debug, Default)]
pub struct Usage {
    pub runtime: u64,
    pub models: u64,
    pub voices: u64,
    pub clips: u64,
    pub free: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.runtime + self.models + self.voices + self.clips
    }
}

/// Bytes under a directory, following what is actually there rather than what
/// a manifest claims. Symlinks are not followed: the runtime is full of them,
/// and counting their targets would report the same bytes several times over.
fn directory_bytes(path: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else { return 0 };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => directory_bytes(&entry.path()),
            Ok(kind) if kind.is_symlink() => 0,
            _ => entry.metadata().map(|m| m.len()).unwrap_or(0),
        })
        .sum()
}

/// Gigabytes past a gigabyte, megabytes below it. A model is 3.4 GB and a clip
/// is 300 KB; one unit for both makes one of them unreadable.
pub fn size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1e9)
    } else if bytes >= 1_000_000 {
        format!("{:.0} MB", bytes as f64 / 1e6)
    } else {
        format!("{:.0} KB", bytes as f64 / 1e3)
    }
}

impl VoiceStudio {
    /// Walk the data directory. Deliberate rather than continuous: this is a
    /// pane you open, and counting on every render would be a tax on the app
    /// for a number nobody is watching change.
    pub(crate) fn refresh_storage(&mut self, cx: &mut Context<Self>) {
        let free = self.system.as_ref().map(|s| s.free_bytes).unwrap_or(0);
        // Model weights live in the Hugging Face cache, wherever that is on
        // this machine, so the catalogue is the one thing that knows their
        // size — the sidecar measures it there and reports it per model.
        let models = self.models.iter().filter(|m| m.installed).map(|m| m.size_bytes).sum();
        cx.spawn(async move |this, cx| {
            let usage = cx
                .background_spawn(async move {
                    let data = speech_engine::paths::data_dir();
                    Usage {
                        runtime: directory_bytes(&speech_engine::paths::runtime_dir()),
                        models,
                        voices: directory_bytes(&data.join("voices")),
                        clips: directory_bytes(&data.join("clips")),
                        free,
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                this.storage = Some(usage);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// One line of the account: what it is, how much, and what removing it
    /// would cost you.
    fn usage_row(label: String, detail: String, bytes: u64, total: u64) -> Div {
        let share = if total > 0 { bytes as f32 / total as f32 } else { 0.0 };
        div()
            .v_flex()
            .w_full()
            .gap(px(6.0))
            .child(
                div()
                    .h_flex()
                    .w_full()
                    .items_baseline()
                    .gap(px(10.0))
                    .child(div().text_size(px(12.5)).font_medium().child(label))
                    .child(div().flex_1())
                    .child(ui::mono(size(bytes), 12.0, theme::hex(0x171717))),
            )
            .child(
                div()
                    .w_full()
                    .h(px(4.0))
                    .rounded_full()
                    .bg(theme::hex(0xEBE4D9))
                    .child(
                        div()
                            .h_full()
                            .rounded_full()
                            .bg(theme::hex(0xFF8A1F))
                            .w(relative(share)),
                    ),
            )
            .child(
                div()
                    .text_size(px(11.5))
                    .line_height(px(17.0))
                    .text_color(theme::hex(0x6B645A))
                    .child(detail),
            )
    }

    pub(crate) fn storage_pane(&self, cx: &mut Context<Self>) -> AnyElement {
        let usage = self.storage;
        let total = usage.map(|u| u.total()).unwrap_or(0);

        div()
            .v_flex()
            .flex_1()
            .h_full()
            .min_w(px(0.0))
            .gap(px(14.0))
            .px(px(22.0))
            .py(px(20.0))
            .child(
                div()
                    .v_flex()
                    .flex_none()
                    .child(
                        div()
                            .font_family(theme::FONT_DISPLAY)
                            .text_size(px(17.0))
                            .font_semibold()
                            .child(t!("settings.storage").to_string()),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme::hex(0x6B645A))
                            .mt(px(3.0))
                            .child(match usage {
                                Some(u) => t!(
                                    "storage.line",
                                    used = size(u.total()),
                                    free = size(u.free)
                                )
                                .to_string(),
                                None => t!("storage.counting").to_string(),
                            }),
                    ),
            )
            .when_some(usage, |d, usage| {
                d.child(
                    div()
                        .v_flex()
                        .w_full()
                        .flex_1()
                        .min_h(px(0.0))
                        .gap(px(16.0))
                        .id("storage-list")
                        .overflow_y_scroll()
                        .child(Self::usage_row(
                            t!("settings.models").to_string(),
                            t!("storage.models_detail").to_string(),
                            usage.models,
                            total,
                        ))
                        // Zero here is a real answer, not a missing one: a
                        // checkout running against a developer environment has
                        // never installed a runtime of its own.
                        .child(Self::usage_row(
                            t!("settings.runtime").to_string(),
                            if usage.runtime > 0 {
                                t!("storage.runtime_detail").to_string()
                            } else {
                                t!("storage.runtime_elsewhere").to_string()
                            },
                            usage.runtime,
                            total,
                        ))
                        .child(Self::usage_row(
                            t!("workspace.clips").to_string(),
                            t!("storage.clips_detail").to_string(),
                            usage.clips,
                            total,
                        ))
                        .child(Self::usage_row(
                            t!("settings.voices").to_string(),
                            t!("storage.voices_detail").to_string(),
                            usage.voices,
                            total,
                        )),
                )
            })
            // Everything above lives in one folder, and saying which one is the
            // difference between "the app took my disk" and a thing you can act
            // on.
            .child(
                div()
                    .v_flex()
                    .w_full()
                    .flex_none()
                    .gap(px(6.0))
                    .pt(px(14.0))
                    .border_t_1()
                    .border_color(theme::hex(0xEBE4D9))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .line_height(px(18.0))
                            .text_color(theme::hex(0x5F594F))
                            .child(t!("storage.where").to_string()),
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
                                    .id("reveal-storage")
                                    .on_click(|_, _, _| {
                                        let _ = std::process::Command::new("open")
                                            .arg(speech_engine::paths::data_dir())
                                            .spawn();
                                    }),
                            ),
                    )
                    .child(
                        ui::secondary_button(None, t!("storage.recount").to_string())
                            .h(px(30.0))
                            .px(px(11.0))
                            .rounded(px(7.0))
                            .text_size(px(12.0))
                            .id("recount-storage")
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_storage(cx))),
                    ),
            )
            .into_any_element()
    }
}
