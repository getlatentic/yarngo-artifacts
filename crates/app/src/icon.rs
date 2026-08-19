//! Material Symbols Rounded, the icon set the design draws with.
//!
//! It is a ligature font: the glyph is selected by writing its *name* as text —
//! `check`, `play_arrow` — and the font substitutes the symbol. So an icon here
//! is a text run in a particular family, not an SVG or a codepoint, which is
//! also why the family name has to match exactly or you get the literal word.

use gpui::*;

/// The family as the bundled file registers itself.
pub const FAMILY: &str = "Material Symbols Rounded";

/// Names used by the design. Kept as constants so a typo fails to compile
/// rather than rendering the word "chekc" in the middle of the interface.
pub mod name {
    pub const CHECK: &str = "check";
    pub const PLAY_ARROW: &str = "play_arrow";
    pub const PLAY_CIRCLE: &str = "play_circle";
    pub const PAUSE: &str = "pause";
    pub const PAUSE_CIRCLE: &str = "pause_circle";
    pub const MIC: &str = "mic";
    pub const ADD: &str = "add";
    pub const SETTINGS: &str = "settings";
    pub const EXPAND_MORE: &str = "expand_more";
    pub const EXPAND_LESS: &str = "expand_less";
    pub const CHECK_CIRCLE: &str = "check_circle";
    pub const HARD_DRIVE: &str = "hard_drive";
    pub const TUNE: &str = "tune";
    pub const WIFI_OFF: &str = "wifi_off";
    pub const DOWNLOAD: &str = "download";
    pub const DOWNLOADING: &str = "downloading";
    pub const UPLOAD_FILE: &str = "upload_file";
    pub const ERROR_OUTLINE: &str = "error_outline";
    pub const GRAPHIC_EQ: &str = "graphic_eq";
    pub const LOCK: &str = "lock";
    pub const CLOSE: &str = "close";
    pub const STOP_CIRCLE: &str = "stop_circle";
    pub const FOLDER_OPEN: &str = "folder_open";
    pub const BLOCK: &str = "block";
    pub const DELETE: &str = "delete";
    pub const OPEN_IN_NEW: &str = "open_in_new";
    pub const HEADPHONES: &str = "headphones";
    pub const REFRESH: &str = "refresh";
    pub const CONTENT_COPY: &str = "content_copy";
    pub const RADIO_UNCHECKED: &str = "radio_button_unchecked";
    pub const MEMORY: &str = "memory";
    pub const EDIT_NOTE: &str = "edit_note";
    pub const EDIT: &str = "edit";
    pub const PROGRESS: &str = "progress_activity";
    pub const RECORD_VOICE: &str = "record_voice_over";
    pub const PANEL_OPEN: &str = "right_panel_open";
    pub const PANEL_CLOSE: &str = "right_panel_close";
    pub const CASINO: &str = "casino";
    pub const DESCRIPTION: &str = "description";
}

/// One icon at a given size and colour.
pub fn icon(glyph: &'static str, size: f32, colour: Hsla) -> impl IntoElement {
    face(FAMILY, glyph.to_string(), size, colour)
}

/// The solid version, for the glyphs the design draws filled: the chosen voice,
/// the clip that is playing, the transport itself.
///
/// Material Symbols keeps its filled variants both on a `FILL` variable-font
/// axis and as explicit `<name>.fill` glyphs. gpui puts OpenType features on a
/// font but not variable-font axes, so the axis cannot be reached from here —
/// and a separate filled family does not resolve under gpui's font loading at
/// all. `scripts/make-filled-icon-font.py` therefore points a private-use
/// codepoint at each `.fill` glyph inside the one bundled file, and this
/// addresses them by that codepoint. A name with no filled cut falls back to
/// the outline.
pub fn filled(glyph: &'static str, size: f32, colour: Hsla) -> impl IntoElement {
    face(FAMILY, filled_char(glyph).unwrap_or_else(|| glyph.to_string()), size, colour)
}

/// Printed by `scripts/make-filled-icon-font.py` when it writes the font, so
/// the two can be checked against each other rather than trusted.
fn filled_char(glyph: &str) -> Option<String> {
    let c = match glyph {
        name::CHECK_CIRCLE => '\u{f0000}',
        name::PLAY_CIRCLE => '\u{f0001}',
        name::PAUSE_CIRCLE => '\u{f0002}',
        name::PLAY_ARROW => '\u{f0003}',
        name::PAUSE => '\u{f0004}',
        name::STOP_CIRCLE => '\u{f0005}',
        _ => return None,
    };
    Some(c.to_string())
}

fn face(family: &'static str, glyph: String, size: f32, colour: Hsla) -> impl IntoElement {
    div()
        .font_family(family)
        .text_size(px(size))
        .text_color(colour)
        .line_height(px(size))
        .child(glyph)
}
