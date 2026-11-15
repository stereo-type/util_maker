#!/usr/bin/env python3
"""Создаёт эталонный растр для плагина «Текст с экрана».

Для повторной генерации нужны Pillow и шрифт DejaVu Sans; они есть в
dev-образе проекта. Готовые PNG/PGM/JSON хранятся в репозитории, поэтому
сборка и тесты не зависят от генератора.
"""

import json
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "plugins" / "screen-ocr" / "reference"
FONT_PATH = Path("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf")
WIDTH = 600
HEIGHT = 340
ORIGIN = (-120, -80)
STRIDE = WIDTH * 4 + 24
FONT_SIZE = 36
LINES = [
    ("Ошибка 404", 40, 20),
    ("File missing", 40, 120),
    ("лишнее", 40, 220),
]


def main() -> None:
    font = ImageFont.truetype(str(FONT_PATH), FONT_SIZE)
    image = Image.new("L", (WIDTH, HEIGHT), 255)
    draw = ImageDraw.Draw(image)
    line_records = []
    for text, x, y in LINES:
        draw.text((x, y), text, font=font, fill=0)
        left, top, right, bottom = draw.textbbox((x, y), text, font=font)
        line_records.append(
            {
                "text": text,
                "rect": {
                    "x": left,
                    "y": top,
                    "width": right - left,
                    "height": bottom - top,
                },
            }
        )

    reference = {
        "format": "bgra8",
        "image": {
            "width": WIDTH,
            "height": HEIGHT,
            "png": "sample.png",
            "pgm": "sample.pgm",
        },
        "frame": {
            "origin": {"x": ORIGIN[0], "y": ORIGIN[1]},
            "width": WIDTH,
            "height": HEIGHT,
            "stride": STRIDE,
        },
        "selection": {"x": 40, "y": 10, "width": 520, "height": 180},
        "lines": line_records,
        "crops": {
            "russian": {"x": 20, "y": 5, "width": 500, "height": 80},
            "english": {"x": 20, "y": 105, "width": 500, "height": 80},
            "empty": {"x": 450, "y": 10, "width": 110, "height": 110},
        },
        "expected": {
            "russianEnglish": ["Ошибка 404", "File missing"],
            "russian": ["Ошибка 404"],
            "english": ["File missing"],
            "outside": "лишнее",
        },
    }

    OUT.mkdir(parents=True, exist_ok=True)
    image.save(OUT / "sample.png")
    image.save(OUT / "sample.pgm")
    (OUT / "sample.json").write_text(
        json.dumps(reference, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    print(f"Записано: {OUT / 'sample.png'}, {OUT / 'sample.pgm'}, {OUT / 'sample.json'}")


if __name__ == "__main__":
    main()
