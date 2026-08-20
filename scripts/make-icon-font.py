#!/usr/bin/env python3
"""Cut the icon font down to the icons this app actually draws.

Material Symbols ships 6,597 glyphs. The design uses forty. Shipped whole it is
14 MB compiled into every copy of the binary — more than the application's own
machine code — so this subsets it and prints the Rust module that addresses the
result.

Two things make the subset possible at all:

*Codepoints instead of ligatures.* Material Symbols is normally used as a
ligature font: you write `play_arrow` and the font substitutes the symbol. That
defeats subsetting, because our forty names between them use most of the
alphabet, so every other icon spellable from those letters survives too. But
each glyph also carries its own codepoint (`check` is U+E668), so addressing
them that way lets the letters and the whole `liga` table go.

*Filled variants by private-use codepoint.* Material Symbols keeps filled cuts
both on a `FILL` variable axis and as explicit `<name>.fill` glyphs. gpui puts
OpenType features on a font but not variable-font axes, so the axis is
unreachable, and a second family does not resolve under gpui's font loading.
Pointing a private-use codepoint at each `.fill` glyph inside the one file works,
and survives subsetting because the mapping is rebuilt here afterwards.

    uv run --with fonttools scripts/make-icon-font.py

**This script works and its output is not yet shipped.** The font it writes is
valid by every measure available outside the application — CoreText loads it and
reports both U+E668 and the private-use codepoints present, Pillow rasterises
U+E668 to a glyph pixel-identical to the original, `add_fonts` returns Ok, and
`all_font_names()` lists the family. Inside gpui every icon still draws as
tofu, so the full font stays bundled until that is understood.

Ruled out, each by running it: the hand-built cmap (replaced with the
subsetter's own, in all four subtables, coverage verified per subtable);
variable versus static instancing (both fail); a `STAT` table orphaned by
instancing; the family name and a poisoned macOS font cache (a name never
registered before still fails, and is listed by gpui); the font being invalid
(it renders elsewhere); a system-installed Material Symbols shadowing ours
(none is installed); and glyph-ID renumbering (`retain_gids` fails too).

What is *not* ruled out is gpui's own glyph lookup on a subset font — the
shaper resolves a codepoint the font demonstrably contains to .notdef. The next
step is to instrument gpui's text system directly rather than the font, since
every property of the font now checks out.
"""
from pathlib import Path

from fontTools import subset, ttLib
from fontTools.varLib import instancer

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "crates/app/fonts/MaterialSymbolsRounded.ttf"
# The family the subset registers itself as; icon.rs must agree.
FAMILY = "Yarngo Icons"

# Every icon the design draws. Adding one here and re-running is the whole
# process for introducing an icon; the printed module goes into icon.rs.
ICONS = [
    ("CHECK", "check"),
    ("PLAY_ARROW", "play_arrow"),
    ("PLAY_CIRCLE", "play_circle"),
    ("PAUSE", "pause"),
    ("PAUSE_CIRCLE", "pause_circle"),
    ("MIC", "mic"),
    ("ADD", "add"),
    ("SETTINGS", "settings"),
    ("EXPAND_MORE", "expand_more"),
    ("EXPAND_LESS", "expand_less"),
    ("CHECK_CIRCLE", "check_circle"),
    ("HARD_DRIVE", "hard_drive"),
    ("TUNE", "tune"),
    ("WIFI_OFF", "wifi_off"),
    ("DOWNLOAD", "download"),
    ("DOWNLOADING", "downloading"),
    ("UPLOAD_FILE", "upload_file"),
    ("WARNING", "warning"),
    ("INFO", "info"),
    ("GRAPHIC_EQ", "graphic_eq"),
    ("LOCK", "lock"),
    ("CLOSE", "close"),
    ("STOP_CIRCLE", "stop_circle"),
    ("FOLDER_OPEN", "folder_open"),
    ("BLOCK", "block"),
    ("DELETE", "delete"),
    ("OPEN_IN_NEW", "open_in_new"),
    ("HEADPHONES", "headphones"),
    ("REFRESH", "refresh"),
    ("CONTENT_COPY", "content_copy"),
    ("RADIO_UNCHECKED", "radio_button_unchecked"),
    ("MEMORY", "memory"),
    ("EDIT_NOTE", "edit_note"),
    ("MORE_HORIZ", "more_horiz"),
    ("EDIT", "edit"),
    ("PROGRESS", "progress_activity"),
    ("RECORD_VOICE", "record_voice_over"),
    ("PANEL_OPEN", "right_panel_open"),
    ("PANEL_CLOSE", "right_panel_close"),
    ("CASINO", "casino"),
    ("DESCRIPTION", "description"),
]

# The ones the design draws solid. Each gets a private-use codepoint, assigned
# in this order, so the numbers are stable as long as the list only grows.
FILLED = [
    "check_circle",
    "play_circle",
    "pause_circle",
    "play_arrow",
    "pause",
    "stop_circle",
]

PRIVATE_USE_BASE = 0xF0000


def main() -> None:
    font = ttLib.TTFont(SOURCE)
    before = SOURCE.stat().st_size
    by_glyph = {glyph: code for code, glyph in font.getBestCmap().items()}
    present = set(font.getGlyphOrder())

    missing = [name for _, name in ICONS if name not in by_glyph]
    if missing:
        raise SystemExit(f"not in the font: {missing}")
    # A name with no filled cut would silently render as its outline, which is
    # a design bug that should stop the build instead.
    missing_fills = [f"{name}.fill" for name in FILLED if f"{name}.fill" not in present]
    if missing_fills:
        raise SystemExit(f"no filled cut for: {missing_fills}")

    # Flatten to a static font first. Material Symbols ships variable — FILL,
    # wght, GRAD, opsz — and gpui can reach none of those axes, so the
    # variation tables are weight without benefit, and a subset that keeps
    # gvar while dropping most glyphs leaves it describing deltas for glyphs
    # that no longer exist.
    if "fvar" in font:
        axes = {a.axisTag: a.defaultValue for a in font["fvar"].axes}
        instancer.instantiateVariableFont(font, axes, inplace=True, updateFontNames=False)

    # Add the private-use entries *before* subsetting, then subset by codepoint,
    # so fontTools builds the character map itself in every subtable format the
    # font carries. An earlier version replaced the cmap tables by hand
    # afterwards: CoreText accepted the result and reported the glyphs present,
    # and gpui still drew tofu. Byte-level table surgery belongs to the library
    # that knows the format.
    fills = {name: PRIVATE_USE_BASE + i for i, name in enumerate(FILLED)}
    for table in font["cmap"].tables:
        # Only the 32-bit subtables reach past the basic multilingual plane.
        if table.format != 4:
            for name, code in fills.items():
                table.cmap[code] = f"{name}.fill"

    options = subset.Options()
    # Addressing by codepoint means the ligature table, and the alphabet it
    # needed, can both go — which is where most of the 15 MB lived.
    options.layout_features = []
    options.glyph_names = True
    options.notdef_outline = True
    # Keep original glyph IDs. Subsetting renumbers them by default, and a
    # consumer that resolved a codepoint through one mapping while reading
    # outlines through another lands on .notdef for every icon.
    options.retain_gids = True
    # STAT describes variable axes that instancing has already removed.
    options.drop_tables += ["DSIG", "STAT"]

    subsetter = subset.Subsetter(options=options)
    subsetter.populate(unicodes={by_glyph[n] for _, n in ICONS} | set(fills.values()))
    subsetter.subset(font)

    for record in font["name"].names:
        if record.nameID in (1, 16):
            record.string = FAMILY
        elif record.nameID == 4:
            record.string = f"{FAMILY} Regular"
        elif record.nameID == 6:
            record.string = FAMILY.replace(" ", "")

    font.save(SOURCE)
    after = SOURCE.stat().st_size
    print(f"{SOURCE.name}: {before/1e6:.1f} MB -> {after/1e6:.3f} MB")
    print(f"kept {len(keep)} glyphs of 6597\n")

    print("// Generated by scripts/make-icon-font.py — regenerate rather than edit.")
    print("pub mod name {")
    for const, glyph in ICONS:
        print(f'    pub const {const}: &str = "\\u{{{by_glyph[glyph]:x}}}"; // {glyph}')
    print("}")
    print()
    print("// Filled cuts, addressed by private-use codepoint.")
    for name, code in fills.items():
        print(f"//   {name} -> \\u{{{code:x}}}")


if __name__ == "__main__":
    main()
