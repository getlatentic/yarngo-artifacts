#!/usr/bin/env python3
"""Cut the macOS app icon from the brand mark.

The brand guidelines are specific about this one: "The aperture fills the tile.
The wordmark never appears in an app icon", and "Orange fields belong to brand
moments — covers, campaigns, the app icon". So the tile is Action Orange with
the aperture in Warm White filling it, not the in-product arrangement of an
orange mark on a warm surface.

Geometry follows Apple's macOS template: a 1024 canvas with the rounded square
occupying 824 of it, centred, corner radius 185.4. The system draws its own
shadow, so none is baked in.

Run with pillow available:

    uv run --with pillow scripts/make-app-icon.py
"""
import subprocess
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
MARK = ROOT / "design/html-and-assets/project/brand/yarngo-icon-warmwhite.png"
ICONSET = ROOT / "packaging/YarngoStudio.iconset"
ICNS = ROOT / "packaging/YarngoStudio.icns"

CANVAS = 1024
TILE = 824
RADIUS = 185
ORANGE = (255, 110, 8, 255)  # Action Orange, #FF6E08
# The aperture fills the tile: a small breathing margin, not a logo floating in
# the middle of a field.
INSET = 0.80


def build() -> Image.Image:
    icon = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))

    tile = Image.new("RGBA", (TILE, TILE), (0, 0, 0, 0))
    ImageDraw.Draw(tile).rounded_rectangle([0, 0, TILE - 1, TILE - 1], RADIUS, fill=ORANGE)

    mark = Image.open(MARK).convert("RGBA")
    # Trim to the mark's own ink before scaling, so the margin is the margin
    # around the aperture rather than around whatever canvas it was exported on.
    bbox = mark.split()[3].point(lambda a: 255 if a > 8 else 0).getbbox()
    mark = mark.crop(bbox)

    room = int(TILE * INSET)
    scale = min(room / mark.width, room / mark.height)
    mark = mark.resize((round(mark.width * scale), round(mark.height * scale)), Image.LANCZOS)

    tile.alpha_composite(mark, ((TILE - mark.width) // 2, (TILE - mark.height) // 2))
    icon.alpha_composite(tile, ((CANVAS - TILE) // 2, (CANVAS - TILE) // 2))
    return icon


def main() -> None:
    icon = build()
    ICONSET.mkdir(parents=True, exist_ok=True)
    for size in (16, 32, 128, 256, 512):
        icon.resize((size, size), Image.LANCZOS).save(ICONSET / f"icon_{size}x{size}.png")
        icon.resize((size * 2, size * 2), Image.LANCZOS).save(
            ICONSET / f"icon_{size}x{size}@2x.png"
        )

    subprocess.run(["iconutil", "-c", "icns", str(ICONSET), "-o", str(ICNS)], check=True)
    # The packager wants a PNG too, for the platforms that do not read .icns.
    icon.save(ROOT / "packaging/icon-1024.png")
    print(f"wrote {ICNS.relative_to(ROOT)} ({ICNS.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
