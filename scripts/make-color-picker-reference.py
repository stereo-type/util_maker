#!/usr/bin/env python3
"""Создаёт эталонное изображение и таблицу точек для плагина «Пипетка цвета»."""

import json
import struct
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "plugins" / "color-picker" / "reference"
WIDTH = 240
HEIGHT = 160
ORIGIN = (-40, -20)
STRIDE = WIDTH * 4 + 16


def image_point(x: int, y: int) -> tuple[int, int]:
    return x - ORIGIN[0], y - ORIGIN[1]


def put_rgb(pixels: bytearray, x: int, y: int, rgb: tuple[int, int, int]) -> None:
    image_x, image_y = image_point(x, y)
    offset = (image_y * WIDTH + image_x) * 3
    pixels[offset : offset + 3] = bytes(rgb)


def png_chunk(kind: bytes, data: bytes) -> bytes:
    crc = zlib.crc32(kind + data) & 0xFFFFFFFF
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", crc)


def make_png(pixels: bytearray) -> bytes:
    raw = bytearray()
    for y in range(HEIGHT):
        raw.append(0)  # filter: none
        start = y * WIDTH * 3
        raw.extend(pixels[start : start + WIDTH * 3])
    header = struct.pack(">IIBBBBB", WIDTH, HEIGHT, 8, 2, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", header)
        + png_chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + png_chunk(b"IEND", b"")
    )


def main() -> None:
    pixels = bytearray(WIDTH * HEIGHT * 3)
    for y in range(HEIGHT):
        for x in range(WIDTH):
            offset = (y * WIDTH + x) * 3
            # Низкочастотная шахматка хорошо сжимается и не мешает
            # различать контрастные контрольные точки.
            light = ((x // 20) + (y // 20)) % 2 == 0
            pixels[offset : offset + 3] = bytes(
                (46, 62, 84) if light else (28, 38, 54)
            )

    # Контрастные точки. PNG хранит RGB, а тестовый буфер — BGRA8 с отступом.
    put_rgb(pixels, 0, 0, (0x2F, 0x80, 0xED))
    put_rgb(pixels, 1, 0, (0x33, 0x22, 0x11))
    put_rgb(pixels, 0, 1, (0x66, 0x55, 0x44))
    put_rgb(pixels, -40, -20, (0x00, 0x00, 0x00))
    put_rgb(pixels, -39, -20, (0xFF, 0xFF, 0xFF))
    put_rgb(pixels, -38, -20, (0x30, 0x20, 0x10))

    points = [
        {
            "name": "target",
            "physical": {"x": 0, "y": 0},
            "image": {"x": 40, "y": 20},
            "bgra": [0xED, 0x80, 0x2F, 0xFF],
            "hex": "#2F80ED",
            "cssRgb": "rgb(47, 128, 237)",
        },
        {
            "name": "right",
            "physical": {"x": 1, "y": 0},
            "image": {"x": 41, "y": 20},
            "bgra": [0x11, 0x22, 0x33, 0xFF],
            "hex": "#332211",
            "cssRgb": "rgb(51, 34, 17)",
        },
        {
            "name": "down",
            "physical": {"x": 0, "y": 1},
            "image": {"x": 40, "y": 21},
            "bgra": [0x44, 0x55, 0x66, 0xFF],
            "hex": "#665544",
            "cssRgb": "rgb(102, 85, 68)",
        },
        {
            "name": "black",
            "physical": {"x": -40, "y": -20},
            "image": {"x": 0, "y": 0},
            "bgra": [0x00, 0x00, 0x00, 0xFF],
            "hex": "#000000",
            "cssRgb": "rgb(0, 0, 0)",
        },
        {
            "name": "white",
            "physical": {"x": -39, "y": -20},
            "image": {"x": 1, "y": 0},
            "bgra": [0xFF, 0xFF, 0xFF, 0xFF],
            "hex": "#FFFFFF",
            "cssRgb": "rgb(255, 255, 255)",
        },
        {
            "name": "alpha-ignored",
            "physical": {"x": -38, "y": -20},
            "image": {"x": 2, "y": 0},
            "bgra": [0x10, 0x20, 0x30, 0x00],
            "hex": "#302010",
            "cssRgb": "rgb(48, 32, 16)",
        },
    ]

    reference = {
        "format": "bgra8",
        "origin": {"x": ORIGIN[0], "y": ORIGIN[1]},
        "width": WIDTH,
        "height": HEIGHT,
        "stride": STRIDE,
        "monitors": [
            {
                "x": ORIGIN[0],
                "y": ORIGIN[1],
                "width": WIDTH,
                "height": HEIGHT,
            }
        ],
        "points": points,
    }

    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "sample.png").write_bytes(make_png(pixels))
    (OUT / "sample.json").write_text(
        json.dumps(reference, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    print(f"Записано: {OUT / 'sample.png'}, {OUT / 'sample.json'}")


if __name__ == "__main__":
    main()
