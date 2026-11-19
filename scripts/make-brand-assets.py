#!/usr/bin/env python3
# Standby pictures for the tray are rendered by scripts/make-standby.ps1 (PowerShell, no Pillow needed).
"""Generate every logo/icon file from assets/logo-source.png (transparent PNG).

Outputs (committed, so builds don't need Pillow):
  assets/logo.png                         trimmed full logo (README)
  assets/icon.png                         square app icon, 512 px
  crates/page2cam/windows/page2cam.ico    exe + installer icon (16..256 px)
  crates/p2c-admin/ui/logo.png            control-page header logo (64 px high)
  crates/p2c-admin/ui/icon.png            control-page favicon (64 px)
  packaging/windows/installer-welcome.bmp NSIS welcome/finish side image (164x314)
  packaging/windows/installer-header.bmp  NSIS page header image (150x57)
Run: python3 scripts/make-brand-assets.py   (needs Pillow)
"""
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "assets" / "logo-source.png"
WHITE = (255, 255, 255, 255)


def trim(im):
    box = im.getchannel("A").point(lambda a: 255 if a > 8 else 0).getbbox()
    return im.crop(box)


def icon_part(im):
    """The browser-window-with-camera mark: everything left of the first
    fully transparent column gap after the mark."""
    a = im.getchannel("A")
    w, h = im.size
    seen = False
    for x in range(w):
        filled = any(a.getpixel((x, y)) > 8 for y in range(0, h, 2))
        if filled:
            seen = True
        elif seen:
            return trim(im.crop((0, 0, x, h)))
    return im


def square(im, size, pad=0.06):
    canvas = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    inner = int(size * (1 - 2 * pad))
    scale = inner / max(im.size)
    m = im.resize((max(1, round(im.width * scale)), max(1, round(im.height * scale))), Image.LANCZOS)
    canvas.alpha_composite(m, ((size - m.width) // 2, (size - m.height) // 2))
    return canvas


def fit_height(im, h):
    return im.resize((round(im.width * h / im.height), h), Image.LANCZOS)


def on_white(im):
    bg = Image.new("RGBA", im.size, WHITE)
    bg.alpha_composite(im)
    return bg.convert("RGB")


def main():
    logo = trim(Image.open(SRC).convert("RGBA"))
    mark = icon_part(logo)

    logo.save(ROOT / "assets" / "logo.png", optimize=True)
    square(mark, 512).save(ROOT / "assets" / "icon.png", optimize=True)

    ico = square(mark, 256, pad=0.02)
    ico.save(
        ROOT / "crates" / "page2cam" / "windows" / "page2cam.ico",
        sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
    )

    ui = ROOT / "crates" / "p2c-admin" / "ui"
    fit_height(logo, 64).save(ui / "logo.png", optimize=True)
    square(mark, 64, pad=0.02).save(ui / "icon.png", optimize=True)

    # NSIS welcome/finish image: mark on top, wordmark under it, white page.
    welcome = Image.new("RGBA", (164, 314), WHITE)
    m = square(mark, 120, pad=0.0)
    welcome.alpha_composite(m, ((164 - 120) // 2, 70))
    word = logo.crop((mark.width, 0, logo.width, logo.height))
    word = trim(word)
    word = word.resize((140, round(word.height * 140 / word.width)), Image.LANCZOS)
    welcome.alpha_composite(word, ((164 - word.width) // 2, 70 + 120 + 18))
    on_white(welcome).save(ROOT / "packaging" / "windows" / "installer-welcome.bmp")

    # NSIS header (right side of each page's top band).
    header = Image.new("RGBA", (150, 57), WHITE)
    small = fit_height(logo, 36)
    if small.width > 142:
        small = small.resize((142, round(small.height * 142 / small.width)), Image.LANCZOS)
    header.alpha_composite(small, ((150 - small.width) // 2, (57 - small.height) // 2))
    on_white(header).save(ROOT / "packaging" / "windows" / "installer-header.bmp")
    print("brand assets written")


if __name__ == "__main__":
    main()
