#!/usr/bin/env python3
"""Создаёт эталонные изображения QR для плагина «QR-коды».

Готовые PNG/PGM/JSON хранятся в репозитории, поэтому сборка и тесты не
зависят от генератора. Для повторной генерации нужны python3-segno и
python3-pil; они есть в dev-образе проекта.
"""

import json
from pathlib import Path

import segno
from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "plugins" / "qr-code" / "reference"

URL = "https://example.com/search?q=%D0%BF%D1%80%D0%B8%D0%B2%D0%B5%D1%82&lang=ru#top"
UNICODE = "  Привет, мир! 👋\nСтрока 2\t— конец  "
WIFI = "WIFI:T:WPA;S:Office;P:secret;;"
COMMAND = "rm -rf /tmp/example && echo '<b>ok</b>'"
INSIDE = "https://inside.example/выбор"
OUTSIDE = "https://outside.example/не-выбор"


def qr_image(content, *, encoding=None, eci=False, scale=7, border=4):
    qr = segno.make(
        content,
        error="m",
        encoding=encoding,
        eci=eci,
        micro=False,
        boost_error=False,
    )
    matrix = qr.matrix
    size = len(matrix)
    total = (size + border * 2) * scale
    image = Image.new("L", (total, total), 255)
    draw = ImageDraw.Draw(image)
    for y, row in enumerate(matrix):
        for x, value in enumerate(row):
            if value:
                left = (border + x) * scale
                top = (border + y) * scale
                draw.rectangle(
                    [left, top, left + scale - 1, top + scale - 1],
                    fill=0,
                )
    return image


def write(image, name):
    image.save(OUT / f"{name}.png")
    image.save(OUT / f"{name}.pgm")
    return {
        "image": f"{name}.pgm",
        "width": image.width,
        "height": image.height,
        "selection": {"x": 0, "y": 0, "width": image.width, "height": image.height},
    }


def add_text(case, text, **extra):
    case.update({"kind": "text", "text": text})
    case.update(extra)
    return case


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    cases = []

    url = qr_image(URL, encoding="utf-8", eci=True)
    cases.append(add_text(write(url, "url"), URL, name="url-eci-utf8"))

    unicode_image = qr_image(UNICODE, encoding="utf-8", eci=True)
    cases.append(add_text(write(unicode_image, "unicode"), UNICODE, name="unicode-eci-utf8"))

    rotated = url.transpose(Image.Transpose.ROTATE_90)
    cases.append(add_text(write(rotated, "rotated"), URL, name="rotated-90"))

    ascii_image = qr_image(WIFI, eci=False)
    cases.append(add_text(write(ascii_image, "ascii"), WIFI, name="ascii-no-eci"))

    command = qr_image(COMMAND, eci=False)
    cases.append(add_text(write(command, "command"), COMMAND, name="command-markup"))

    unsupported = qr_image("Привет", encoding="iso-8859-5", eci=True)
    unsupported_case = write(unsupported, "unsupported")
    unsupported_case.update({"name": "unsupported-encoding", "kind": "unsupported-encoding"})
    cases.append(unsupported_case)

    binary = qr_image(bytes([0x00, 0xFF, 0xFE, 0x01, 0x80]), eci=False)
    binary_case = write(binary, "binary")
    binary_case.update({"name": "binary-payload", "kind": "binary"})
    cases.append(binary_case)

    damaged = url.copy()
    draw = ImageDraw.Draw(damaged)
    width, height = damaged.size
    draw.rectangle(
        [width * 35 // 100, height * 30 // 100, width * 65 // 100, height * 70 // 100],
        fill=0,
    )
    damaged_case = write(damaged, "damaged")
    damaged_case.update({"name": "damaged", "kind": "damaged"})
    cases.append(damaged_case)

    left = qr_image(INSIDE, encoding="utf-8", eci=True, scale=6)
    right = qr_image(OUTSIDE, encoding="utf-8", eci=True, scale=6)
    scene = Image.new("L", (1000, 520), 255)
    scene.paste(left, (40, 40))
    scene.paste(right, (560, 40))

    inside_case = {
        "name": "scene-inside-only",
        "image": "scene.pgm",
        "width": scene.width,
        "height": scene.height,
        "selection": {
            "x": 20,
            "y": 20,
            "width": left.width + 40,
            "height": left.height + 40,
        },
        "kind": "text",
        "text": INSIDE,
    }
    cases.append(inside_case)

    outside_case = {
        "name": "scene-outside-only",
        "image": "scene.pgm",
        "width": scene.width,
        "height": scene.height,
        "selection": {
            "x": 540,
            "y": 20,
            "width": right.width + 40,
            "height": right.height + 40,
        },
        "kind": "text",
        "text": OUTSIDE,
    }
    cases.append(outside_case)

    both_case = {
        "name": "scene-two-codes",
        "image": "scene.pgm",
        "width": scene.width,
        "height": scene.height,
        "selection": {
            "x": 20,
            "y": 20,
            "width": 900,
            "height": 420,
        },
        "kind": "multiple",
        "count": 2,
    }
    cases.append(both_case)

    empty_case = {
        "name": "scene-no-code",
        "image": "scene.pgm",
        "width": scene.width,
        "height": scene.height,
        "selection": {"x": 30, "y": 440, "width": 300, "height": 60},
        "kind": "no-code",
    }
    cases.append(empty_case)

    scene.save(OUT / "scene.png")
    scene.save(OUT / "scene.pgm")

    reference = {
        "format": "bgra8",
        "origin": {"x": -120, "y": -80},
        "stridePadding": 20,
        "cases": cases,
    }
    (OUT / "sample.json").write_text(
        json.dumps(reference, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )
    print(f"Записано: {OUT / 'sample.json'} и {len(cases)} случаев")


if __name__ == "__main__":
    main()
