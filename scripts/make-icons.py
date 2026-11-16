#!/usr/bin/env python3
"""Генерирует все иконки Util Maker в стиле «Пастель».

Запуск: python3 scripts/make-icons.py

Требуется Pillow; в dev-образе проекта есть python3-pil. Скрипт собирает
иконки приложения, статусов трея, служебных пунктов меню и микроприложений
из одной сетки, поэтому все плитки одного размера и радиуса, а глифы
приведены к общему оптическому размеру.

Стиль «Пастель»: светлая плитка цвета инструмента (акцент, осветлённый до
14 %), тонкая рамка того же акцента, тёмный акцентный глиф. Плитка занимает
весь холст, радиус 28 %, рамка 4.5 % от плитки, глиф — 68 % плитки.
"""

from __future__ import annotations

import math
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
ICONS = ROOT / "apps" / "desktop" / "icons"
FONT_BOLD = "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"
S = 1024
RADIUS = 0.28
BORDER = 0.045
GLYPH_BOX = 0.68
PASTEL = 0.86
BORDER_ALPHA = 0.55

ACCENTS = {
    "app": "#1D4ED8",
    "screenshot": "#2563EB",
    "translator": "#0D9488",
    "pipette": "#EA580C",
    "qr": "#4F46E5",
    "ocr": "#DB2777",
    "settings": "#475569",
    "pause": "#EAB308",
    "retry": "#7C3AED",
    "quit": "#DC2626",
    "fallback": "#94A3B8",
    "working": "#16A34A",
}


def hex_rgb(value):
    value = value.lstrip("#")
    return tuple(int(value[index : index + 2], 16) for index in (0, 2, 4))


def mix(first, second, ratio):
    return tuple(round(a * (1 - ratio) + b * ratio) for a, b in zip(first, second))


class Pen:
    """Рисует по альфа-маске в нормализованных координатах 0..1."""

    def __init__(self):
        self.image = Image.new("L", (S, S), 0)
        self.draw = ImageDraw.Draw(self.image)

    def rect(self, box, radius=0.0, fill=255):
        x0, y0, x1, y1 = (value * S for value in box)
        if radius > 0:
            self.draw.rounded_rectangle((x0, y0, x1, y1), radius=radius * S, fill=fill)
        else:
            self.draw.rectangle((x0, y0, x1, y1), fill=fill)

    def circle(self, center, radius, fill=255):
        cx, cy = center[0] * S, center[1] * S
        r = radius * S
        self.draw.ellipse((cx - r, cy - r, cx + r, cy + r), fill=fill)

    def ring(self, center, r_out, r_in, fill=255):
        self.circle(center, r_out, fill)
        self.circle(center, r_in, 0)

    def capsule(self, p1, p2, width, fill=255):
        self.draw.line(
            [(p1[0] * S, p1[1] * S), (p2[0] * S, p2[1] * S)],
            fill=fill,
            width=round(width * S),
        )
        self.circle(p1, width / 2, fill)
        self.circle(p2, width / 2, fill)

    def poly(self, points, fill=255):
        self.draw.polygon([(x * S, y * S) for x, y in points], fill=fill)

    def wedge(self, center, radius, start, end, fill=0):
        cx, cy = center[0] * S, center[1] * S
        r = radius * S
        self.draw.pieslice((cx - r, cy - r, cx + r, cy + r), start=start, end=end, fill=fill)

    def arc(self, center, radius, start, end, width, fill=255):
        cx, cy = center[0] * S, center[1] * S
        r = radius * S
        self.draw.arc(
            (cx - r, cy - r, cx + r, cy + r),
            start=start,
            end=end,
            fill=fill,
            width=round(width * S),
        )

    def text(self, center, value, size, fill=255):
        font = ImageFont.truetype(FONT_BOLD, round(size * S))
        left, top, right, bottom = font.getbbox(value)
        self.draw.text(
            (
                center[0] * S - (right - left) / 2 - left,
                center[1] * S - (bottom - top) / 2 - top,
            ),
            value,
            font=font,
            fill=fill,
        )


def glyph_app(pen):
    """Логотип приложения: буква U из того же набора, что и остальные глифы."""
    pen.text((0.5, 0.5), "U", 0.62)


def glyph_screenshot(pen):
    pen.rect((0.17, 0.33, 0.83, 0.79), radius=0.10)
    pen.rect((0.35, 0.22, 0.62, 0.36), radius=0.05)
    pen.circle((0.50, 0.56), 0.16, fill=0)
    pen.circle((0.74, 0.42), 0.035, fill=0)


def glyph_translator(pen):
    pen.rect((0.14, 0.16, 0.86, 0.68), radius=0.14)
    pen.poly([(0.28, 0.66), (0.46, 0.66), (0.28, 0.84)])
    pen.text((0.50, 0.42), "A", 0.32, fill=0)


def glyph_pipette(pen):
    pen.poly([(0.16, 0.90), (0.30, 0.70), (0.40, 0.80)])
    pen.capsule((0.30, 0.76), (0.64, 0.38), 0.13)
    pen.circle((0.72, 0.29), 0.15)


def glyph_qr(pen):
    def finder(x, y):
        pen.rect((x, y, x + 0.30, y + 0.30), radius=0.03)
        pen.rect((x + 0.055, y + 0.055, x + 0.245, y + 0.245), radius=0.01, fill=0)
        pen.rect((x + 0.105, y + 0.105, x + 0.195, y + 0.195), radius=0.01)

    finder(0.12, 0.12)
    finder(0.58, 0.12)
    finder(0.12, 0.58)
    for x, y in [(0.58, 0.58), (0.72, 0.58), (0.58, 0.72), (0.72, 0.72), (0.86, 0.72), (0.72, 0.86)]:
        pen.rect((x, y, x + 0.10, y + 0.10), radius=0.015)


def glyph_ocr(pen):
    width = 0.06
    pen.capsule((0.15, 0.17), (0.33, 0.17), width)
    pen.capsule((0.15, 0.17), (0.15, 0.35), width)
    pen.capsule((0.85, 0.83), (0.67, 0.83), width)
    pen.capsule((0.85, 0.83), (0.85, 0.65), width)
    pen.rect((0.25, 0.34, 0.75, 0.43), radius=0.045)
    pen.rect((0.25, 0.48, 0.63, 0.57), radius=0.045)
    pen.rect((0.25, 0.62, 0.70, 0.71), radius=0.045)


def glyph_settings(pen):
    for index in range(8):
        angle = math.pi * 2 * index / 8
        half = math.radians(13)
        points = []
        for offset, radius in [(-half, 0.25), (-half, 0.42), (half, 0.42), (half, 0.25)]:
            points.append(
                (0.5 + radius * math.cos(angle + offset), 0.5 - radius * math.sin(angle + offset))
            )
        pen.poly(points)
    pen.circle((0.5, 0.5), 0.30)
    pen.circle((0.5, 0.5), 0.145, fill=0)


def glyph_play(pen):
    """Работа: треугольник «play» на зелёной плитке."""
    pen.poly([(0.30, 0.22), (0.80, 0.50), (0.30, 0.78)])


def glyph_pause(pen):
    pen.capsule((0.38, 0.27), (0.38, 0.73), 0.15)
    pen.capsule((0.62, 0.27), (0.62, 0.73), 0.15)


def glyph_retry(pen):
    pen.ring((0.5, 0.5), 0.33, 0.21)
    pen.wedge((0.5, 0.5), 0.36, 272, 338)
    pen.poly([(0.50, 0.13), (0.50, 0.33), (0.72, 0.23)])


def glyph_quit(pen):
    pen.ring((0.5, 0.55), 0.32, 0.20)
    pen.wedge((0.5, 0.55), 0.36, 245, 295)
    pen.capsule((0.5, 0.18), (0.5, 0.44), 0.08)


def glyph_fallback(pen):
    for x, y in [(0.18, 0.18), (0.57, 0.18), (0.18, 0.57), (0.57, 0.57)]:
        pen.rect((x, y, x + 0.25, y + 0.25), radius=0.06)


GLYPHS = {
    "app": glyph_app,
    "screenshot": glyph_screenshot,
    "translator": glyph_translator,
    "pipette": glyph_pipette,
    "qr": glyph_qr,
    "ocr": glyph_ocr,
    "settings": glyph_settings,
    "pause": glyph_pause,
    "retry": glyph_retry,
    "quit": glyph_quit,
    "fallback": glyph_fallback,
    "play": glyph_play,
}

OPTICAL = {
    "app": 0.86,
    "qr": 0.98,
    "ocr": 0.98,
}


def normalize(mask, target):
    bbox = mask.getbbox()
    if not bbox:
        return mask
    glyph = mask.crop(bbox)
    width, height = glyph.size
    scale = target * S / max(width, height)
    size = (max(1, round(width * scale)), max(1, round(height * scale)))
    glyph = glyph.resize(size, Image.LANCZOS)
    result = Image.new("L", (S, S), 0)
    result.paste(glyph, (round(S / 2 - size[0] / 2), round(S / 2 - size[1] / 2)))
    return result


def render(key, accent_hex, size):
    accent = hex_rgb(accent_hex)
    tile = Pen()
    tile.rect((0, 0, 1, 1), radius=RADIUS)

    background = mix(accent, (255, 255, 255), PASTEL)
    icon = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    icon.paste(Image.new("RGB", (S, S), background), mask=tile.image)

    pen = Pen()
    GLYPHS[key](pen)
    glyph_mask = normalize(pen.image, GLYPH_BOX * OPTICAL.get(key, 1.0))
    glyph_mask = ImageChops.multiply(glyph_mask, tile.image)
    glyph_color = mix(accent, (0, 0, 0), 0.25)
    icon.paste(Image.new("RGB", (S, S), glyph_color), mask=glyph_mask)

    overlay = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    overlay_draw = ImageDraw.Draw(overlay)
    inset = BORDER * S / 2
    overlay_draw.rounded_rectangle(
        (inset, inset, S - inset, S - inset),
        radius=(RADIUS - BORDER / 2) * S,
        outline=accent + (round(255 * BORDER_ALPHA),),
        width=round(BORDER * S),
    )
    overlay.putalpha(ImageChops.multiply(overlay.getchannel("A"), tile.image))
    icon = Image.alpha_composite(icon, overlay)

    return icon.resize((size, size), Image.LANCZOS)


def save(path, key, accent, size):
    path.parent.mkdir(parents=True, exist_ok=True)
    image = render(key, accent, size)
    if path.suffix == ".ico":
        image.save(path, format="ICO", sizes=[(32, 32), (256, 256)])
    else:
        image.save(path)
    print(f"{path.relative_to(ROOT)}: {size}×{size}")


def main():
    app_accent = ACCENTS["app"]

    for size in (32, 128, 256):
        save(ICONS / f"{size}x{size}.png", "app", app_accent, size)
    save(ICONS / "icon.ico", "app", app_accent, 256)

    save(ICONS / "tray-working.png", "play", ACCENTS["working"], 32)
    save(ICONS / "tray-paused.png", "pause", ACCENTS["pause"], 32)

    for name in ("settings", "pause", "retry", "quit", "fallback"):
        save(ICONS / f"menu-{name}.png", name, ACCENTS[name], 32)

    # Иконка «Пипетки» осталась прежней: авторский синий значок лежит в
    # plugins/color-picker/assets и генератором не перезаписывается.
    plugins = {
        "screenshot": "screenshot",
        "translator": "translator",
        "control": "app",
        "qr-code": "qr",
        "screen-ocr": "ocr",
    }
    for folder, key in plugins.items():
        save(ROOT / "plugins" / folder / "assets" / "icon.png", key, ACCENTS[key], 128)


if __name__ == "__main__":
    main()