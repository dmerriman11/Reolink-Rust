"""Render the app icon and installer artwork from the UI's brand mark (ui/style.css .brand-mark).

Mark geometry on its 22 px CSS box: dark core r<3, white ring 4..6, amber outer 7..11 (1 px soft edges).
Run from the repo root: python icons/make_icons.py   (needs Pillow; uses Windows' Bahnschrift font)
"""
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

BG = (10, 11, 13)
TEXT = (236, 235, 232)
ACCENT = (244, 177, 62)
PANEL = (16, 18, 21)
OUT = Path(__file__).parent
SS = 8  # supersampling factor


def mark(size: int) -> Image.Image:
    """The brand mark on a transparent square, supersampled for clean edges at any size."""
    big = size * SS
    img = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    c = big / 2
    unit = big / 22  # one CSS pixel of the 22 px mark
    for radius, color in [(11, ACCENT), (6.5, TEXT), (3.5, BG)]:
        r = radius * unit
        d.ellipse((c - r, c - r, c + r, c + r), fill=color + (255,))
    return img.resize((size, size), Image.LANCZOS)


def font(px: int) -> ImageFont.FreeTypeFont:
    f = ImageFont.truetype("C:/Windows/Fonts/bahnschrift.ttf", px)
    try:
        f.set_variation_by_name("SemiBold")
    except Exception:
        pass  # older Pillow/FreeType: regular weight is still on-brand
    return f


def wordmark(draw: ImageDraw.ImageDraw, x: int, y: int, px: int) -> None:
    f = font(px)
    draw.text((x, y), "Reolink", font=f, fill=TEXT, anchor="lm")
    w = draw.textlength("Reolink", font=f)
    draw.text((x + w + px * 0.12, y), "rs", font=f, fill=ACCENT, anchor="lm")


def logo_banner(w: int, h: int, mark_px: int, text_px: int, x0: int) -> Image.Image:
    img = Image.new("RGB", (w, h), PANEL)
    m = mark(mark_px)
    img.paste(m, (x0, (h - mark_px) // 2), m)
    wordmark(ImageDraw.Draw(img), x0 + mark_px + mark_px // 2, h // 2, text_px)
    return img


def sidebar(w: int, h: int) -> Image.Image:
    img = Image.new("RGB", (w, h), PANEL)
    d = ImageDraw.Draw(img)
    # Soft amber glow behind the mark, like the UI's box-shadow.
    glow = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    gd = ImageDraw.Draw(glow)
    cx, cy = w // 2, int(h * 0.36)
    for r in range(70, 0, -2):
        a = int(26 * (1 - r / 70) ** 2)
        gd.ellipse((cx - r, cy - r, cx + r, cy + r), fill=ACCENT + (a,))
    img.paste(glow, (0, 0), glow)
    m = mark(72)
    img.paste(m, (cx - 36, cy - 36), m)
    f = font(26)
    total = d.textlength("Reolink", font=f) + 3 + d.textlength("rs", font=f)
    wordmark(d, int(cx - total / 2), int(h * 0.58), 26)
    return img


if __name__ == "__main__":
    sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
    frames = [mark(s) for s in sizes]
    frames[-1].save(OUT / "icon.ico", sizes=[(s, s) for s in sizes], append_images=frames[:-1])
    mark(512).save(OUT / "icon.png")
    # NSIS: header 150x57, sidebar 164x314 (24-bit BMP). WiX: banner 493x58, dialog 493x312.
    logo_banner(150, 57, 26, 20, 12).save(OUT / "nsis-header.bmp")
    sidebar(164, 314).save(OUT / "nsis-sidebar.bmp")
    logo_banner(493, 58, 28, 22, 18).save(OUT / "wix-banner.bmp")
    dialog = Image.new("RGB", (493, 312), (255, 255, 255))  # WiX draws dialog text on the right; art goes left
    dialog.paste(sidebar(164, 312), (0, 0))
    dialog.save(OUT / "wix-dialog.bmp")
    print("wrote", ", ".join(p.name for p in sorted(OUT.glob("*")) if p.suffix in (".ico", ".png", ".bmp")))
