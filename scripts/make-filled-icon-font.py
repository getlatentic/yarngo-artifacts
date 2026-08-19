#!/usr/bin/env python3
"""Give the bundled icon font a second address for its filled glyphs.

Material Symbols carries filled variants two ways: a `FILL` variable-font axis,
and explicit `<name>.fill` glyphs. gpui puts OpenType *features* on a font but
not variable-font *axes*, so the axis is unreachable from the call site — and a
separate filled family, tried first, does not resolve at all under gpui's font
loading (it renders as tofu).

So this reaches the `.fill` glyphs directly instead: it adds cmap entries in a
private-use range pointing at them, inside the same file. One family, already
resolving, and the filled glyph is one codepoint away from the outline one.

Run with fonttools available:

    uv run --with fonttools scripts/make-filled-icon-font.py
"""
from fontTools import ttLib

FONT = "crates/app/fonts/MaterialSymbolsRounded.ttf"
# Only the glyphs the design actually fills.
ICONS = [
    "check_circle",
    "play_circle",
    "pause_circle",
    "play_arrow",
    "pause",
    "stop_circle",

]
# Supplementary private use area A, well clear of the font's own PUA block.
BASE = 0xF0000

font = ttLib.TTFont(FONT)
glyphs = set(font.getGlyphOrder())
missing = [i for i in ICONS if f"{i}.fill" not in glyphs]
if missing:
    raise SystemExit(f"no .fill glyph for: {missing}")

added = {}
for index, icon in enumerate(ICONS):
    codepoint = BASE + index
    added[icon] = codepoint
    for table in font["cmap"].tables:
        # Only the tables that can hold a supplementary-plane codepoint.
        if table.format in (4,) and codepoint > 0xFFFF:
            continue
        table.cmap[codepoint] = f"{icon}.fill"

font.save(FONT)

check = ttLib.TTFont(FONT).getBestCmap()
print("filled codepoints, for icon.rs:")
for icon, codepoint in added.items():
    ok = check.get(codepoint) == f"{icon}.fill"
    print(f"    name::{icon.upper()} => '\\u{{{codepoint:x}}}',  # {'ok' if ok else 'MISSING'}")
