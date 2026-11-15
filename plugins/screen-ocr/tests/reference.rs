//! Проверка эталонного растра и реального Tesseract на Linux.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;
use util_maker_screen_ocr::{BgraFrame, OcrError, OcrLanguage, Selection, TesseractEngine};
use util_maker_sdk::FrameOrigin;

fn reference_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("reference")
        .join(name)
}

fn read_json(name: &str) -> Value {
    let text = fs::read_to_string(reference_path(name)).expect("эталон JSON");
    serde_json::from_str(&text).expect("разбор эталона")
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
    let data = fs::read(reference_path(name)).expect("эталон PGM");
    let mut position = 0;
    assert_eq!(next_token(&data, &mut position), b"P5");
    let width = std::str::from_utf8(next_token(&data, &mut position))
        .expect("ширина")
        .parse::<u32>()
        .expect("ширина");
    let height = std::str::from_utf8(next_token(&data, &mut position))
        .expect("высота")
        .parse::<u32>()
        .expect("высота");
    assert_eq!(next_token(&data, &mut position), b"255");
    // После максимального значения в нашем файле ровно один перевод строки.
    position += 1;
    let pixels = data[position..].to_vec();
    assert_eq!(pixels.len(), width as usize * height as usize);
    (width, height, pixels)
}

fn bgra_frame(reference: &Value) -> (BgraFrame<'static>, FrameOrigin, u32, u32) {
    let (width, height, gray) = read_pgm("sample.pgm");
    let frame = &reference["frame"];
    let origin = FrameOrigin {
        x: frame["origin"]["x"].as_i64().unwrap() as i32,
        y: frame["origin"]["y"].as_i64().unwrap() as i32,
    };
    let stride = frame["stride"].as_u64().unwrap() as usize;
    let mut pixels = vec![0xAAu8; stride * height as usize];
    for y in 0..height as usize {
        for x in 0..width as usize {
            let value = gray[y * width as usize + x];
            let offset = y * stride + x * 4;
            pixels[offset] = value;
            pixels[offset + 1] = value;
            pixels[offset + 2] = value;
            pixels[offset + 3] = 0xFF;
        }
    }
    let leaked: &'static [u8] = Box::leak(pixels.into_boxed_slice());
    let frame = BgraFrame::new(leaked, width, height, stride, origin).expect("эталонный кадр");
    (frame, origin, width, height)
}

fn physical_selection(rect: &Value, origin: FrameOrigin) -> Selection {
    Selection::new(
        origin.x + rect["x"].as_i64().unwrap() as i32,
        origin.y + rect["y"].as_i64().unwrap() as i32,
        rect["width"].as_u64().unwrap() as u32,
        rect["height"].as_u64().unwrap() as u32,
    )
}

fn expected_lines(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("массив строк")
        .iter()
        .map(|item| item.as_str().expect("строка").to_owned())
        .collect()
}

fn tesseract_executable() -> String {
    env::var("TESSERACT").unwrap_or_else(|_| "tesseract".to_owned())
}

fn tesseract_available(executable: &str) -> bool {
    Command::new(executable)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn tesseract_languages(executable: &str) -> Vec<String> {
    let Ok(output) = Command::new(executable).arg("--list-langs").output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("List of available"))
        .filter(|line| !line.contains(':'))
        .map(str::to_owned)
        .collect()
}

#[test]
fn crop_uses_reference_geometry_and_excludes_outside_word() {
    let reference = read_json("sample.json");
    let (frame, origin, width, height) = bgra_frame(&reference);
    assert_eq!(frame.width(), width);
    assert_eq!(frame.height(), height);
    assert_eq!(frame.origin(), origin);

    let selection = physical_selection(&reference["selection"], origin);
    let image = frame.crop(selection).expect("выделение");
    assert_eq!(image.width(), 520);
    assert_eq!(image.height(), 180);

    // Строка «лишнее» находится ниже выделения и не входит в кадр OCR.
    let outside = physical_selection(&reference["lines"][2]["rect"], origin);
    assert!(frame.crop(outside).is_ok());
    assert!(outside.y >= selection.y + selection.height as i32);
}

#[test]
fn tesseract_reference_checks() {
    let reference = read_json("sample.json");
    let (frame, origin, _, _) = bgra_frame(&reference);
    let executable = tesseract_executable();
    let require = env::var_os("UTIL_MAKER_REQUIRE_TESSERACT").is_some();

    if !tesseract_available(&executable) {
        if require {
            panic!("Tesseract не найден: {executable}");
        }
        eprintln!("Tesseract не найден — проверка реального движка пропущена");
        return;
    }
    let languages = tesseract_languages(&executable);
    if !languages.iter().any(|item| item == "rus") || !languages.iter().any(|item| item == "eng") {
        if require {
            panic!("Нужны модели rus и eng, доступны: {languages:?}");
        }
        eprintln!("Модели rus/eng не установлены — проверка реального движка пропущена");
        return;
    }

    let engine = TesseractEngine::new(&executable, None);
    let selection = physical_selection(&reference["selection"], origin);
    let image = frame.crop(selection).expect("выделение");
    let bilingual = engine
        .recognize(&image, OcrLanguage::RussianEnglish)
        .expect("двуязычное распознавание");
    assert_eq!(
        bilingual.lines().to_vec(),
        expected_lines(&reference["expected"]["russianEnglish"])
    );
    assert!(!bilingual
        .text()
        .contains(reference["expected"]["outside"].as_str().unwrap()));

    let russian_rect = physical_selection(&reference["crops"]["russian"], origin);
    let russian_image = frame.crop(russian_rect).expect("русский фрагмент");
    let russian = engine
        .recognize(&russian_image, OcrLanguage::Russian)
        .expect("русское распознавание");
    assert_eq!(
        russian.lines().to_vec(),
        expected_lines(&reference["expected"]["russian"])
    );

    let english_rect = physical_selection(&reference["crops"]["english"], origin);
    let english_image = frame.crop(english_rect).expect("английский фрагмент");
    let english = engine
        .recognize(&english_image, OcrLanguage::English)
        .expect("английское распознавание");
    assert_eq!(
        english.lines().to_vec(),
        expected_lines(&reference["expected"]["english"])
    );

    let empty_rect = physical_selection(&reference["crops"]["empty"], origin);
    let empty_image = frame.crop(empty_rect).expect("пустой фрагмент");
    assert_eq!(
        engine.recognize(&empty_image, OcrLanguage::RussianEnglish),
        Err(OcrError::NoText)
    );

    let missing_models =
        TesseractEngine::new(&executable, Some(PathBuf::from("/definitely/not/tessdata")));
    assert_eq!(
        missing_models.recognize(&russian_image, OcrLanguage::Russian),
        Err(OcrError::MissingLanguage {
            language: "русский".to_owned()
        })
    );
}
