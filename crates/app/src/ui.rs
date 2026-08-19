//! Primitives measured from `Yarngo Studio.dc.html` rather than approximated.
//!
//! Every number here was read out of the design markup. They are collected in
//! one place so a card in enrolment and a card in the workspace cannot drift
//! apart, and so correcting one value corrects it everywhere.

use gpui::prelude::*;
use gpui::*;
use gpui_component::StyledExt;

use crate::icon;
use crate::theme;

/// Card: white, hairline border, 12px radius, and the warm level-1 shadow.
/// `overflow_hidden` matters — without it the header divider draws past the
/// rounded corners.
pub fn card() -> Div {
    div()
        .v_flex()
        .bg(theme::surface(false))
        .border_1()
        .border_color(theme::hex(0xEBE4D9))
        .rounded(px(12.0))
        .shadow_sm()
        .overflow_hidden()
}

/// Label style: mono, 10.5px, 8% tracking, in the non-text grey. The design
/// sets these in `Noto Sans Mono`, not the interface face.
pub fn section_label(text: impl Into<SharedString>) -> Div {
    div()
        .font_family(theme::FONT_MONO)
        .text_size(px(10.5))
        .font_semibold()
        // The design tracks this at .08em; gpui has no letter-spacing control,
        // so the mono face and caps carry the label instead.
        .text_color(theme::hex(0x857D72))
        .child(text.into())
}

/// Secondary button: 34px tall, white, strong hairline border, 8px radius, with
/// an optional leading glyph. Used for "Record again", "Use a file instead".
pub fn secondary_button(
    glyph: Option<(&'static str, u32)>,
    label: impl Into<SharedString>,
) -> Div {
    div()
        .h_flex()
        .h(px(34.0))
        .px(px(13.0))
        .gap(px(8.0))
        .flex_none()
        .items_center()
        .rounded(px(8.0))
        .bg(theme::surface(false))
        .border_1()
        .border_color(theme::hex(0xD8D0C4))
        .text_size(px(12.5))
        .font_semibold()
        .text_color(theme::hex(0x171717))
        .when_some(glyph, |this, (name, colour)| {
            this.child(icon::icon(name, 17.0, theme::hex(colour)))
        })
        .child(label.into())
}

/// The round play control, 40px. `accent` fills it solid orange for the
/// workspace player; otherwise it is the pale accent surface with an orange
/// rim that enrolment review uses.
pub fn play_button(playing: bool, accent: bool) -> Div {
    div()
        .size(px(40.0))
        .flex_none()
        .rounded_full()
        .when(accent, |d| d.bg(theme::hex(0xFF8A1F)))
        .when(!accent, |d| {
            d.bg(theme::hex(0xFFF3E6)).border_1().border_color(theme::hex(0xFFCB93))
        })
        .flex()
        .items_center()
        .justify_center()
        .child(icon::filled(
            if playing { icon::name::PAUSE } else { icon::name::PLAY_ARROW },
            if accent { 22.0 } else { 21.0 },
            if accent { theme::hex(0x171717) } else { theme::hex(0x8F4406) },
        ))
}

/// Waveform: 3px bars, 2px apart, 2px radius, inside a row of `height` px.
/// `progress` splits the bars already played from those still to come, so one
/// renderer serves both the finished take in enrolment and the clip playing in
/// the workspace.
pub fn waveform(levels: &[f32], progress: f32, height: f32, played: Hsla, rest: Hsla) -> Div {
    waveform_bars(levels, progress, height, played, rest, false)
}

/// The same waveform with bars that share the width instead of taking a fixed
/// 3px each, which is how the workspace player fills its row.
pub fn waveform_wide(
    levels: &[f32],
    progress: f32,
    height: f32,
    played: Hsla,
    rest: Hsla,
) -> Div {
    waveform_bars(levels, progress, height, played, rest, true)
}

fn waveform_bars(
    levels: &[f32],
    progress: f32,
    height: f32,
    played: Hsla,
    rest: Hsla,
    fill: bool,
) -> Div {
    let edge = (progress.clamp(0.0, 1.0) * levels.len() as f32).round() as usize;
    div()
        .h_flex()
        .flex_1()
        .min_w(px(0.0))
        .h(px(height))
        .when(fill, |d| d.items_end().gap(px(3.0)))
        .when(!fill, |d| d.items_center().gap(px(2.0)))
        .children(levels.iter().enumerate().map(|(i, level)| {
            let bar = div()
                .h(px(10.0 + level.clamp(0.0, 1.0) * (height - 10.0)))
                .rounded(px(2.0))
                .bg(if i < edge { played } else { rest });
            if fill {
                bar.flex_1()
            } else {
                bar.w(px(3.0)).flex_none()
            }
        }))
}

/// Monospace figures — durations and counts are set in mono in the design.
pub fn mono(text: impl Into<SharedString>, size: f32, colour: Hsla) -> Div {
    div().font_family(theme::FONT_MONO).text_size(px(size)).text_color(colour).child(text.into())
}
