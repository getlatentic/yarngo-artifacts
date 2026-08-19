//! What this is, what it is made of, and what it promises.
//!
//! An app that clones a voice should be able to answer three questions without
//! a support page: what version am I running, what am I allowed to do with what
//! it produces, and where does my voice go. So this pane is the build, the
//! privacy position stated plainly rather than linked to, the consent record,
//! and the things the app is actually made of.
//!
//! Models are not among them. The app does not ship any weights — each model is
//! downloaded by the person using it, under terms stated where they choose it,
//! which is the only moment those terms can still change the decision. Repeating
//! them here would attribute a licence to nothing and bury it where nobody is
//! deciding anything.

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::StyledExt;
use rust_i18n::t;
use speech_engine::runtime;

use crate::{icon, theme, ui, VoiceStudio};

/// One credited component: what it is, and the terms it comes under.
struct Credit {
    what: &'static str,
    terms: &'static str,
}

/// The things the app is built out of that are not its own: bundled, or
/// installed by it. Models are neither — see the note at the top.
const CREDITS: [Credit; 5] = [
    Credit { what: "Sora", terms: "SIL Open Font Licence 1.1" },
    Credit { what: "Noto Sans, Noto Sans Mono", terms: "SIL Open Font Licence 1.1" },
    Credit { what: "Material Symbols Rounded", terms: "Apache-2.0" },
    Credit { what: "CPython, via python-build-standalone", terms: "PSF Licence" },
    Credit { what: "MLX", terms: "MIT" },
];

impl VoiceStudio {
    fn credit_row(what: String, terms: String) -> Div {
        div()
            .h_flex()
            .w_full()
            .items_baseline()
            .gap(px(12.0))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .text_size(px(12.5))
                    .text_color(theme::hex(0x171717))
                    .child(what),
            )
            .child(ui::mono(terms, 11.5, theme::hex(0x6B645A)))
    }

    fn about_section(label: String, body: Div) -> Div {
        div()
            .v_flex()
            .w_full()
            .gap(px(8.0))
            .child(ui::section_label(label.to_uppercase()))
            .child(body)
    }

    pub(crate) fn about_pane(&self, _cx: &mut Context<Self>) -> AnyElement {
        div()
            .v_flex()
            .flex_1()
            .h_full()
            .min_w(px(0.0))
            .gap(px(16.0))
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
                            .child(t!("app.name").to_string()),
                    )
                    .child(
                        ui::mono(
                            t!("about.build", version = env!("CARGO_PKG_VERSION")).to_string(),
                            11.5,
                            theme::hex(0x6B645A),
                        )
                        .mt(px(3.0)),
                    ),
            )
            .child(
                div()
                    .v_flex()
                    .w_full()
                    .flex_1()
                    .min_h(px(0.0))
                    .gap(px(18.0))
                    .id("about-scroll")
                    .overflow_y_scroll()
                    // The promise, stated rather than linked to. It is short
                    // enough to read, and it is the reason the app is built the
                    // way it is.
                    .child(Self::about_section(
                        t!("about.privacy").to_string(),
                        div()
                            .v_flex()
                            .w_full()
                            .gap(px(8.0))
                            .px(px(14.0))
                            .py(px(12.0))
                            .rounded(px(10.0))
                            .bg(theme::surface(false))
                            .border_1()
                            .border_color(theme::hex(0xEBE4D9))
                            .text_size(px(12.5))
                            .line_height(px(19.0))
                            .text_color(theme::hex(0x5F594F))
                            .child(t!("about.privacy_body").to_string())
                            .child(t!("about.privacy_network").to_string()),
                    ))
                    // A cloned voice is a likeness. What was agreed, and where
                    // the record of it lives.
                    .child(Self::about_section(
                        t!("about.consent").to_string(),
                        div()
                            .v_flex()
                            .w_full()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .line_height(px(19.0))
                                    .text_color(theme::hex(0x5F594F))
                                    .child(t!("about.consent_body").to_string()),
                            )
                            .child(
                                div()
                                    .h_flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .child(ui::mono(
                                        speech_engine::paths::data_dir()
                                            .join("consent.log")
                                            .display()
                                            .to_string(),
                                        11.0,
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
                                            .id("reveal-consent")
                                            .on_click(|_, _, _| {
                                                let _ = std::process::Command::new("open")
                                                    .arg("-R")
                                                    .arg(
                                                        speech_engine::paths::data_dir()
                                                            .join("consent.log"),
                                                    )
                                                    .spawn();
                                            }),
                                    ),
                            ),
                    ))
                    .child(Self::about_section(
                        t!("about.built_with").to_string(),
                        div()
                            .v_flex()
                            .w_full()
                            .gap(px(7.0))
                            .children(
                                CREDITS.iter().map(|c| {
                                    Self::credit_row(c.what.to_string(), c.terms.to_string())
                                }),
                            )
                            .child(Self::credit_row(
                                format!("{} {}", runtime::NAME, runtime::VERSION),
                                t!("about.on_this_machine").to_string(),
                            ))
                            // Models are not listed here. The app does not ship
                            // them — each one is downloaded by the person using
                            // it, under terms stated at the moment they choose
                            // it, which is where those terms can still change
                            // the decision.
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .line_height(px(17.0))
                                    .text_color(theme::hex(0x6B645A))
                                    .mt(px(2.0))
                                    .child(t!("about.models_elsewhere").to_string()),
                            ),
                    )),
            )
            .into_any_element()
    }
}
