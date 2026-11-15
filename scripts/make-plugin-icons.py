#!/usr/bin/env python3
"""Генерирует иконки микроприложений и служебных пунктов меню трея.

Запуск: python3 scripts/make-plugin-icons.py

Скрипт не зависит от внешних библиотек: PNG собирается вручную, поэтому
иконки пересобираются в любом окружении и сравниваются побайтно.

Иконка микроприложения — 32×32 RGBA с прозрачными углами: цветная плитка
и белый символ. Служебные пункты меню трея получают самостоятельные
значки: шестерёнку настроек, паузу, перерегистрацию и выход. Заглушка
menu-fallback.png заменяет логотип приложения для микроприложения без
иконки. Логотип Util Maker остаётся только у контрольного модуля и самого
приложения.
"""

from __future__ import annotations

import hashlib
import math
import pathlib
import struct
import zlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
DESKTOP_ICONS = ROOT / "apps" / "desktop" / "icons"
WHITE = (255, 255, 255)
SUPERSAMPLE = 4

# Плитки микроприложений повторяют стиль иконки приложения: скруглённый
# квадрат с символом. Цвет и символ различают инструменты между собой.
SCREENSHOT_BLUE = (47, 111, 237)
TRANSLATOR_TEAL = (14, 148, 136)

# Служебные пункты: отдельные цвета по смыслу, ни один не повторяет
# логотип приложения.
MENU_COLORS = {
    "settings": (75, 85, 99),
    "pause": (224, 158, 28),
    "retry": (124, 92, 255),
    "quit": (200, 52, 52),
    "fallback": (107, 114, 128),
}


def inside_rounded_tile(u: float, v: float) -> bool:
    margin = 0.06
    radius = 0.22
    left, top = margin, margin
    right, bottom = 1.0 - margin, 1.0 - margin
    if u < left or u > right or v < top or v > bottom:
        return False
    center_u = min(max(u, left + radius), right - radius)
    center_v = min(max(v, top + radius), bottom - radius)
    return (u - center_u) ** 2 + (v - center_v) ** 2 <= radius * radius


def segment_distance(
    u: float, v: float, au: float, av: float, bu: float, bv: float
) -> float:
    du, dv = bu - au, bv - av
    length_squared = du * du + dv * dv
    if length_squared == 0:
        return math.hypot(u - au, v - av)
    t = max(0.0, min(1.0, ((u - au) * du + (v - av) * dv) / length_squared))
    return math.hypot(u - (au + t * du), v - (av + t * dv))


def inside_triangle(
    u: float,
    v: float,
    a: tuple[float, float],
    b: tuple[float, float],
    c: tuple[float, float],
) -> bool:
    def side(p, q):
        return (q[0] - p[0]) * (v - p[1]) - (q[1] - p[1]) * (u - p[0])

    first = side(a, b)
    second = side(b, c)
    third = side(c, a)
    has_negative = first < 0 or second < 0 or third < 0
    has_positive = first > 0 or second > 0 or third > 0
    return not (has_negative and has_positive)


def polar(radius: float, degrees: float) -> tuple[float, float]:
    angle = math.radians(degrees)
    return 0.5 + radius * math.cos(angle), 0.5 - radius * math.sin(angle)


def inside_frame(u: float, v: float) -> bool:
    """Рамка выделения — четыре угловых уголка."""
    inset, arm, half = 0.20, 0.23, 0.065
    for cu in (inset, 1.0 - inset):
        for cv in (inset, 1.0 - inset):
            step_u = 1.0 if cu < 0.5 else -1.0
            step_v = 1.0 if cv < 0.5 else -1.0
            if abs(u - cu) <= half:
                low, high = sorted((cv, cv + step_v * arm))
                if low <= v <= high:
                    return True
            if abs(v - cv) <= half:
                low, high = sorted((cu, cu + step_u * arm))
                if low <= u <= high:
                    return True
    return False


def inside_letter(u: float, v: float) -> bool:
    """Буква «А» — знак переводчика."""
    apex = (0.50, 0.22)
    left = (0.24, 0.78)
    right = (0.76, 0.78)
    if segment_distance(u, v, *apex, *left) <= 0.06:
        return True
    if segment_distance(u, v, *apex, *right) <= 0.06:
        return True
    return 0.33 <= u <= 0.67 and 0.56 <= v <= 0.68


def inside_gear(u: float, v: float) -> bool:
    """Шестерёнка настроек: восемь зубцов и отверстие."""
    du, dv = u - 0.5, v - 0.5
    radius = math.hypot(du, dv)
    if radius < 0.12:
        return False
    if radius <= 0.27:
        return True
    if radius > 0.38:
        return False
    angle = math.degrees(math.atan2(-dv, du))
    delta = abs((angle + 22.5) % 45.0 - 22.5)
    return delta <= 12.0


def inside_pause(u: float, v: float) -> bool:
    if not 0.27 <= v <= 0.73:
        return False
    return 0.30 <= u <= 0.42 or 0.58 <= u <= 0.70


def inside_retry(u: float, v: float) -> bool:
    """Круговая стрелка перерегистрации: дуга и треугольный наконечник."""
    du, dv = u - 0.5, v - 0.5
    radius = math.hypot(du, dv)
    angle = math.degrees(math.atan2(-dv, du)) % 360.0
    if 0.14 <= radius <= 0.25 and 70.0 < angle < 330.0:
        return True
    head = (
        polar(0.34, 70.0),
        polar(0.10, 70.0),
        polar(0.22, 25.0),
    )
    return inside_triangle(u, v, *head)


def inside_power(u: float, v: float) -> bool:
    """Знак питания для выхода."""
    if 0.46 <= u <= 0.54 and 0.20 <= v <= 0.52:
        return True
    du, dv = u - 0.5, v - 0.5
    radius = math.hypot(du, dv)
    if not 0.14 <= radius <= 0.26:
        return False
    angle = math.degrees(math.atan2(-dv, du)) % 360.0
    return not 62.0 <= angle <= 118.0


def inside_fallback(u: float, v: float) -> bool:
    """Нейтральная заглушка: пустая рамка с точкой."""
    if 0.29 <= u <= 0.71 and 0.29 <= v <= 0.71:
        border = 0.10
        if (
            u <= 0.29 + border
            or u >= 0.71 - border
            or v <= 0.29 + border
            or v >= 0.71 - border
        ):
            return True
    return (u - 0.5) ** 2 + (v - 0.5) ** 2 <= 0.055**2


def chunk(kind: bytes, payload: bytes) -> bytes:
    checksum = zlib.crc32(kind + payload) & 0xFFFFFFFF
    return (
        struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", checksum)
    )


def png(size: int, background: tuple[int, int, int], glyph) -> bytes:
    total = SUPERSAMPLE * SUPERSAMPLE
    raw = bytearray()
    for pixel_v in range(size):
        raw.append(0)
        for pixel_u in range(size):
            base_hits = 0
            glyph_hits = 0
            for sample_v in range(SUPERSAMPLE):
                for sample_u in range(SUPERSAMPLE):
                    u = (pixel_u + (sample_u + 0.5) / SUPERSAMPLE) / size
                    v = (pixel_v + (sample_v + 0.5) / SUPERSAMPLE) / size
                    if inside_rounded_tile(u, v):
                        base_hits += 1
                        if glyph(u, v):
                            glyph_hits += 1
            if base_hits == 0:
                raw.extend((0, 0, 0, 0))
                continue
            ratio = glyph_hits / base_hits
            color = tuple(
                round(background[index] * (1 - ratio) + WHITE[index] * ratio)
                for index in range(3)
            )
            raw.extend((color[0], color[1], color[2], round(255 * base_hits / total)))
    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )


def write(path: pathlib.Path, size: int, background: tuple[int, int, int], glyph) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    data = png(size, background, glyph)
    path.write_bytes(data)
    digest = hashlib.sha256(data).hexdigest()
    relative = path.relative_to(ROOT)
    print(f"{relative}: {size}×{size}, sha256 {digest}")


def main() -> None:
    write(ROOT / "plugins/screenshot/assets/icon.png", 32, SCREENSHOT_BLUE, inside_frame)
    write(ROOT / "plugins/translator/assets/icon.png", 32, TRANSLATOR_TEAL, inside_letter)
    glyphs = {
        "settings": inside_gear,
        "pause": inside_pause,
        "retry": inside_retry,
        "quit": inside_power,
        "fallback": inside_fallback,
    }
    for name, glyph in glyphs.items():
        write(DESKTOP_ICONS / f"menu-{name}.png", 32, MENU_COLORS[name], glyph)


if __name__ == "__main__":
    main()