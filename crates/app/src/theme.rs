//! Yarngo brand tokens applied to the gpui-component theme.
//!
//! Values come from the Yarngo Brand Guidelines v1.0 semantic token table,
//! which is the authority — the Swift/Kotlin token files are exports of it.
//! Product code references semantic names, never raw hex, so the names below
//! match the guideline rather than being reinterpreted.
//!
//! Two contrast rules the guideline is emphatic about, both easy to get wrong:
//!
//! * **Text on a brand orange field is Ink** (7.6:1). White on `#FF8A1F` is
//!   2.4:1 and is listed as "never".
//! * **Buttons are a different orange.** The action fill is `#FF6E08` with
//!   near-white `#FFFEFD` on top — an explicit, documented exception for
//!   orange surfaces carrying text.
//!
//! And one composition rule: *one accent per screen. If two things are orange,
//! one of them is wrong.*

use gpui::{hsla, App, Hsla};
use gpui_component::{Theme, ThemeMode};
use gpui_component::theme::ThemeTokens;

/// Semantic tokens, light and dark. Dark is a token swap, not a redesign.
/// The palette as the brand guidelines set it down, transcribed whole. Tokens
/// no screen happens to use yet are kept so the table stays the guidelines
/// rather than a subset of them.
#[allow(dead_code)]
pub mod tokens {
    pub mod light {
        pub const BG: u32 = 0xFFF9F2;
        pub const SURFACE: u32 = 0xFFFFFF;
        pub const SURFACE_SUNKEN: u32 = 0xF1EBE1;
        pub const BORDER: u32 = 0xEBE4D9;
        pub const TEXT: u32 = 0x171717;
        pub const TEXT_MUTED: u32 = 0x5F594F;
        pub const ACCENT: u32 = 0xFF8A1F;
        pub const ACCENT_ACTION: u32 = 0xFF6E08;
        pub const ON_ACTION: u32 = 0xFFFEFD;
        pub const ACCENT_TEXT: u32 = 0x8F4406;
        pub const ACCENT_SURFACE: u32 = 0xFFF3E6;
        pub const ON_ACCENT: u32 = 0x171717;
        pub const BG_SUBTLE: u32 = 0xF7F1E8;
        pub const BORDER_STRONG: u32 = 0xD8D0C4;
        /// Icons, dividers and disabled marks — never a text string.
        pub const NON_TEXT: u32 = 0x857D72;
        pub const SUCCESS_SURFACE: u32 = 0xE9F5EF;
        pub const DANGER_SURFACE: u32 = 0xFBEBE8;
        /// Focus is Signal Blue, not the accent — it must not read as a brand mark.
        pub const FOCUS: u32 = 0x596DF8;
    }

    pub mod dark {
        pub const BG: u32 = 0x121110;
        pub const SURFACE: u32 = 0x1F1C19;
        pub const SURFACE_SUNKEN: u32 = 0x171513;
        pub const BORDER: u32 = 0x35302A;
        pub const TEXT: u32 = 0xFFF9F2;
        pub const TEXT_MUTED: u32 = 0xB0A79B;
        pub const ACCENT: u32 = 0xFF9C3D;
        pub const ACCENT_ACTION: u32 = 0xFF9C3D;
        pub const ON_ACTION: u32 = 0x171717;
        pub const ACCENT_TEXT: u32 = 0xFFB264;
        pub const ACCENT_SURFACE: u32 = 0x2E1F10;
        pub const ON_ACCENT: u32 = 0x171717;
        pub const BG_SUBTLE: u32 = 0x1A1816;
        pub const BORDER_STRONG: u32 = 0x4A443C;
        pub const NON_TEXT: u32 = 0x857D72;
        pub const SUCCESS_SURFACE: u32 = 0x10291F;
        pub const DANGER_SURFACE: u32 = 0x2E1512;
        pub const FOCUS: u32 = 0x9AA6FB;
    }

    pub const SUCCESS: u32 = 0x287A57; // Leaf
    pub const WARNING: u32 = 0xC97A00; // Ochre
    pub const DANGER: u32 = 0xC7362B;
    pub const INFO: u32 = 0x596DF8; // Signal Blue — API surfaces only
}

/// Convert a `0xRRGGBB` token into the `Hsla` gpui works in.
fn rgb(hex: u32) -> Hsla {
    let r = ((hex >> 16) & 0xFF) as f32 / 255.0;
    let g = ((hex >> 8) & 0xFF) as f32 / 255.0;
    let b = (hex & 0xFF) as f32 / 255.0;

    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let lightness = (max + min) / 2.0;
    let delta = max - min;

    let saturation = if delta == 0.0 {
        0.0
    } else {
        delta / (1.0 - (2.0 * lightness - 1.0).abs())
    };

    let hue = if delta == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / delta) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };

    hsla(hue.rem_euclid(360.0) / 360.0, saturation.clamp(0.0, 1.0), lightness, 1.0)
}

/// Family names as the bundled faces register themselves.
pub const FONT_TEXT: &str = "Noto Sans";
pub const FONT_DISPLAY: &str = "Sora";
pub const FONT_MONO: &str = "Noto Sans Mono";

/// The hairline between sections. Lighter than a card border, which is what
/// keeps a rule from reading as an edge.
pub const RULE: u32 = 0xE4DCD0;

/// Convert a `0xRRGGBB` design token into the `Hsla` gpui works in.
pub fn hex(value: u32) -> Hsla {
    rgb(value)
}

pub fn apply(mode: ThemeMode, cx: &mut App) {
    Theme::change(mode, None, cx);
    let theme = Theme::global_mut(cx);

    // Loading a font does not select it — the family has to be named here or
    // every string renders in the platform default.
    theme.font_family = FONT_TEXT.into();
    // The design sets interface chrome at 13-14px; the stock 16 made every
    // label look like body copy.
    theme.font_size = gpui::px(14.0);

    macro_rules! set {
        ($set:ident) => {{
            use tokens::$set as t;
            theme.background = rgb(t::BG);
            theme.foreground = rgb(t::TEXT);
            theme.secondary = rgb(t::SURFACE_SUNKEN);
            theme.secondary_foreground = rgb(t::TEXT);
            theme.border = rgb(t::BORDER);
            theme.muted = rgb(t::SURFACE_SUNKEN);
            theme.muted_foreground = rgb(t::TEXT_MUTED);
            theme.popover = rgb(t::SURFACE);
            theme.popover_foreground = rgb(t::TEXT);
            theme.input = rgb(t::BORDER);
            theme.title_bar = rgb(t::BG);
            theme.sidebar = rgb(t::SURFACE);

            // Buttons use the action orange with near-white on top — the
            // documented exception. The brand-field pairing (ink on #FF8A1F)
            // is what `accent` carries, below.
            theme.primary = rgb(t::ACCENT_ACTION);
            theme.primary_hover = rgb(t::ACCENT);
            theme.primary_active = rgb(t::ACCENT_ACTION);
            theme.primary_foreground = rgb(t::ON_ACTION);

            // Buttons read `theme.tokens.button_*`, not `theme.primary`, so the
            // button colours have to be set on their own fields — setting only
            // `primary` leaves every button rendering the stock palette.
            theme.button_primary = rgb(t::ACCENT_ACTION);
            theme.button_primary_hover = rgb(t::ACCENT);
            theme.button_primary_active = rgb(t::ACCENT_ACTION);
            theme.button_primary_foreground = rgb(t::ON_ACTION);

            theme.button = rgb(t::SURFACE);
            theme.button_foreground = rgb(t::TEXT);
            theme.button_hover = rgb(t::SURFACE_SUNKEN);
            theme.button_active = rgb(t::BORDER);

            theme.button_secondary = rgb(t::SURFACE_SUNKEN);
            theme.button_secondary_foreground = rgb(t::TEXT);
            theme.button_secondary_hover = rgb(t::BORDER);
            theme.button_secondary_active = rgb(t::BORDER_STRONG);

            theme.accent = rgb(t::ACCENT_SURFACE);
            theme.accent_foreground = rgb(t::ON_ACCENT);
            // Focus is Signal Blue per the tokens: a focus ring is not a brand moment.
            theme.ring = rgb(t::FOCUS);
            theme.selection = rgb(t::ACCENT_SURFACE);
            theme.tab_active = rgb(t::ACCENT);
        }};
    }

    match mode {
        ThemeMode::Dark => set!(dark),
        _ => set!(light),
    }

    theme.success = rgb(tokens::SUCCESS);
    theme.warning = rgb(tokens::WARNING);
    theme.danger = rgb(tokens::DANGER);
    theme.danger_foreground = rgb(tokens::light::BG);
    theme.button_danger = rgb(tokens::DANGER);
    theme.button_danger_foreground = rgb(tokens::light::BG);
    theme.button_success = rgb(tokens::SUCCESS);
    theme.button_warning = rgb(tokens::WARNING);

    // `tokens` is derived from `colors` when the theme is built, so it must be
    // rebuilt after editing colours or the components keep the old palette.
    theme.tokens = ThemeTokens::from(theme.colors);
}

pub fn surface(dark: bool) -> Hsla {
    rgb(if dark { tokens::dark::SURFACE } else { tokens::light::SURFACE })
}

/// The warm tone behind navigation. Sampled from the design: the sidebar is
/// `#F7F1E8` and the working pane is white — not the other way round.
pub fn bg_subtle(dark: bool) -> Hsla {
    rgb(if dark { tokens::dark::BG_SUBTLE } else { tokens::light::BG_SUBTLE })
}

/// For icons, dividers and disabled marks. Never for a text string.
pub fn non_text(dark: bool) -> Hsla {
    rgb(if dark { tokens::dark::NON_TEXT } else { tokens::light::NON_TEXT })
}

