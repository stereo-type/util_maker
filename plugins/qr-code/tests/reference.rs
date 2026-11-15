//! Проверки декодирования на фиксированных растрах стороннего генератора.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use util_maker_qr_code::{decode_qr, DecodeOutcome, FrameError, GrayImage, QrFrame, Selection};
use util_maker_sdk::FrameOrigin;

fn reference_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("reference")
}

fn read_json(name: &str) -> Value {
    let path = reference_dir().join(name);
    serde_json::from_str(&fs::read_to_string(&path).expect("чтение JSON")).expect("разбор JSON")
}

fn next_token<'a>(data: &'a [u8], position: &mut usize) -> &'a [u8] {
    while *position < data.len() && data[*position].is_ascii_whitespace() {
        *position += 1;
    }
    let start = *position;
    while *position < data.len() && !data[*position].is_ascii_whitespace() {
        *position += 1;
    }
    &data[start..*position]
}

fn read_pgm(name: &str) -> (u32, u32, Vec<u8>) {
    let path = reference_dir().join(name);
    let data = fs::read(&path).expect("чтение PGM");
    let mut position = 0;
    assert_eq!(next_token(&data, &mut position), b"P5");
    let width: u32 = std::str::from_utf8(next_token(&data, &mut position))
        .unwrap()
        .parse()
        .expect("ширина");
    let height: u32 = std::str::from_utf8(next_token(&data, &mut position))
        .unwrap()
        .parse()
        .expect("высота");
    assert_eq!(next_token(&data, &mut position), b"255");
    assert!(position < data.len() && data[position].is_ascii_whitespace());
    position += 1;
    let pixels = data[position..].to_vec();
    assert_eq!(pixels.len(), width as usize * height as usize);
    (width, height, pixels)
}

fn bgra(gray: &[u8], width: u32, height: u32, padding: usize) -> (Vec<u8>, usize) {
    let stride = width as usize * 4 + padding;
    let mut pixels = vec![0u8; stride * height as usize];
    for y in 0..height as usize {
        for x in 0..width as usize {
            let value = gray[y * width as usize + x];
            let offset = y * stride + x * 4;
            pixels[offset] = value;
            pixels[offset + 1] = value;
            pixels[offset + 2] = value;
            pixels[offset + 3] = 0xff;
        }
    }
    (pixels, stride)
}

fn physical_selection(rect: &Value, origin: FrameOrigin) -> Selection {
    Selection::new(
        origin.x + rect["x"].as_i64().unwrap() as i32,
        origin.y + rect["y"].as_i64().unwrap() as i32,
        rect["width"].as_u64().unwrap() as u32,
        rect["height"].as_u64().unwrap() as u32,
    )
}

#[test]
fn fixed_reference_images_return_expected_outcomes() {
    let reference = read_json("sample.json");
    let origin = FrameOrigin {
        x: reference["origin"]["x"].as_i64().unwrap() as i32,
        y: reference["origin"]["y"].as_i64().unwrap() as i32,
    };
    let padding = reference["stridePadding"].as_u64().unwrap() as usize;
    let mut checked = 0;

    for case in reference["cases"].as_array().expect("массив случаев") {
        let name = case["name"].as_str().expect("имя");
        let image = case["image"].as_str().expect("файл");
        let (width, height, gray) = read_pgm(image);
        assert_eq!(width, case["width"].as_u64().unwrap() as u32, "{name}");
        assert_eq!(height, case["height"].as_u64().unwrap() as u32, "{name}");
        let (pixels, stride) = bgra(&gray, width, height, padding);
        let frame = QrFrame::new(&pixels, width, height, stride, origin).expect("кадр");
        let selection = physical_selection(&case["selection"], origin);
        let outcome = decode_qr(&frame, selection).expect("выделение внутри кадра");

        match case["kind"].as_str().expect("категория") {
            "text" => {
                let expected = case["text"].as_str().expect("ожидаемый текст");
                assert_eq!(
                    outcome.text(),
                    Some(expected),
                    "текст не совпал для {name}: {outcome:?}"
                );
                match outcome {
                    DecodeOutcome::Text(text) => assert!(text.version() >= 1),
                    other => panic!("{name}: ожидался текст, получено {other:?}"),
                }
            }
            "no-code" => assert_eq!(outcome, DecodeOutcome::NoCode, "{name}"),
            "damaged" => assert_eq!(outcome, DecodeOutcome::Damaged, "{name}"),
            "multiple" => {
                let expected = case["count"].as_u64().unwrap() as usize;
                assert_eq!(
                    outcome,
                    DecodeOutcome::Multiple { count: expected },
                    "{name}"
                );
                assert!(outcome.needs_single_code());
            }
            "unsupported-encoding" => {
                assert_eq!(outcome, DecodeOutcome::UnsupportedEncoding, "{name}")
            }
            "binary" => assert_eq!(outcome, DecodeOutcome::BinaryPayload, "{name}"),
            kind => panic!("{name}: неизвестная категория {kind}"),
        }
        checked += 1;
    }

    assert_eq!(checked, 12, "проверены все эталонные случаи");
}

#[test]
fn code_outside_selection_does_not_participate() {
    let reference = read_json("sample.json");
    let origin = FrameOrigin {
        x: reference["origin"]["x"].as_i64().unwrap() as i32,
        y: reference["origin"]["y"].as_i64().unwrap() as i32,
    };
    let padding = reference["stridePadding"].as_u64().unwrap() as usize;
    let (width, height, gray) = read_pgm("scene.pgm");
    let (pixels, stride) = bgra(&gray, width, height, padding);
    let frame = QrFrame::new(&pixels, width, height, stride, origin).expect("кадр");

    let inside = physical_selection(
        &serde_json::json!({"x": 20, "y": 20, "width": 262, "height": 262}),
        origin,
    );
    let outcome = decode_qr(&frame, inside).expect("выделение");
    assert_eq!(outcome.text(), Some("https://inside.example/выбор"));

    let blank = physical_selection(
        &serde_json::json!({"x": 30, "y": 440, "width": 300, "height": 60}),
        origin,
    );
    assert_eq!(decode_qr(&frame, blank), Ok(DecodeOutcome::NoCode));
}

#[test]
fn frame_rejects_bad_stride_selection_and_bounds() {
    let origin = FrameOrigin { x: -10, y: -20 };
    let (pixels, stride) = bgra(&[255; 16], 4, 4, 8);
    let frame = QrFrame::new(&pixels, 4, 4, stride, origin).expect("кадр");
    assert_eq!(
        QrFrame::new(&pixels, 4, 4, 3, origin),
        Err(FrameError::InvalidStride {
            stride: 3,
            minimum: 16
        })
    );
    assert_eq!(
        QrFrame::new(&pixels[..10], 4, 4, stride, origin),
        Err(FrameError::IncompleteFrame {
            needed: stride * 4,
            actual: 10
        })
    );
    assert_eq!(
        frame.crop(Selection::new(-10, -20, 0, 4)),
        Err(FrameError::EmptySelection)
    );
    assert_eq!(
        frame.crop(Selection::new(-10, -20, 5, 4)),
        Err(FrameError::OutsideFrame)
    );
    assert_eq!(
        frame
            .crop(Selection::new(-10, -20, 4, 4))
            .map(|image| image.decode()),
        Ok(DecodeOutcome::NoCode)
    );
}

#[test]
fn empty_gray_image_is_rejected() {
    assert_eq!(
        GrayImage::new(0, 0, Vec::new()),
        Err(util_maker_qr_code::ImageError::EmptyImage)
    );
    assert_eq!(
        GrayImage::new(2, 2, vec![255, 255]),
        Err(util_maker_qr_code::ImageError::IncompleteImage {
            needed: 4,
            actual: 2
        })
    );
}

#[test]
fn empty_qr_payload_does_not_produce_text() {
    use qrcodegen::{QrCode, QrCodeEcc};

    let code = QrCode::encode_segments(&[], QrCodeEcc::Medium).expect("пустой QR");
    let modules = code.size() as usize;
    let scale = 8usize;
    let border = 4usize;
    let side = (modules + border * 2) * scale;
    let mut pixels = vec![255u8; side * side];
    for y in 0..modules {
        for x in 0..modules {
            if !code.get_module(x as i32, y as i32) {
                continue;
            }
            let left = (border + x) * scale;
            let top = (border + y) * scale;
            for row in top..top + scale {
                for column in left..left + scale {
                    pixels[row * side + column] = 0;
                }
            }
        }
    }
    let image = GrayImage::new(side as u32, side as u32, pixels).expect("серое изображение");
    assert_eq!(image.decode(), DecodeOutcome::Empty);
    assert_eq!(image.decode().text(), None);
}
