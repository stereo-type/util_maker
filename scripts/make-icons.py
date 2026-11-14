#!/usr/bin/env python3
"""Генерирует иконки прототипа без внешних зависимостей.

Запуск: python3 scripts/make-icons.py
Создаёт apps/desktop/icons/32x32.png, 128x128.png и icon.ico (32 и 256 px).
"""

from __future__ import annotations

import pathlib
import struct
import zlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
ICONS = ROOT / "apps" / "desktop" / "icons"

BLUE = (47, 111, 237)
WHITE = (255, 255, 255)
SUPERSAMPLE = 3


def inside_rounded_rect(x: float, y: float, size: float) -> bool:
    margin = size * 0.06
    radius = size * 0.22
    left, top = margin, margin
    right, bottom = size - margin, size - margin
    if x < left or x > right or y < top or y > bottom:
        return False
    center_x = min(max(x, left + radius), right - radius)
    center_y = min(max(y, top + radius), bottom - radius)
    return (x - center_x) ** 2 + (y - center_y) ** 2 <= radius * radius


def inside_u(x: float, y: float, size: float) -> bool:
    left = size * 0.30
    right = size * 0.70
    top = size * 0.26
    bottom = size * 0.66
    thickness = size * 0.11
    if top <= y <= bottom and left <= x <= left + thickness:
        return True
    if top <= y <= bottom and right - thickness <= x <= right:
        return True
    inner_left = left + thickness * 0.55
    inner_right = right - thickness * 0.55
    if bottom - thickness <= y <= bottom and inner_left <= x <= inner_right:
        # Нижняя дуга буквы: отсекаем внешние углы.
        dx = abs(x - size * 0.5)
        return dx <= (inner_right - inner_left) / 2
    return False


def render(size: int) -> list[list[tuple[int, int, int, int]]]:
    rows = []
    step = 1.0 / SUPERSAMPLE
    for pixel_y in range(size):
        row = []
        for pixel_x in range(size):
            blue_hits = 0
            white_hits = 0
            for sample_y in range(SUPERSAMPLE):
                for sample_x in range(SUPERSAMPLE):
                    x = pixel_x + (sample_x + 0.5) * step
                    y = pixel_y + (sample_y + 0.5) * step
                    if inside_rounded_rect(x, y, size):
                        blue_hits += 1
                        if inside_u(x, y, size):
                            white_hits += 1
            total = SUPERSAMPLE * SUPERSAMPLE
            if blue_hits == 0:
                row.append((0, 0, 0, 0))
                continue
            alpha = round(255 * blue_hits / total)
            ratio = white_hits / blue_hits
            color = tuple(round(BLUE[i] * (1 - ratio) + WHITE[i] * ratio) for i in range(3))
            row.append((color[0], color[1], color[2], alpha))
        rows.append(row)
    return rows


def chunk(kind: bytes, payload: bytes) -> bytes:
    checksum = zlib.crc32(kind + payload) & 0xFFFFFFFF
    return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", checksum)


def png(size: int) -> bytes:
    rows = render(size)
    raw = b"".join(
        b"\x00" + bytes(value for pixel in row for value in pixel) for row in rows
    )
    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def ico(images: list[tuple[int, bytes]]) -> bytes:
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    entries = b""
    payload = b""
    for size, data in images:
        dimension = 0 if size >= 256 else size
        entries += struct.pack(
            "<BBBBHHII",
            dimension,
            dimension,
            0,
            0,
            1,
            32,
            len(data),
            offset + len(payload),
        )
        payload += data
    return header + entries + payload


def main() -> None:
    ICONS.mkdir(parents=True, exist_ok=True)
    for size in (32, 128, 256):
        (ICONS / f"{size}x{size}.png").write_bytes(png(size))
    images = [(32, png(32)), (256, png(256))]
    (ICONS / "icon.ico").write_bytes(ico(images))
    print(f"готово: {ICONS}")


if __name__ == "__main__":
    main()
