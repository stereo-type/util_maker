//! Переносимая часть плагина «Текст с экрана».
//!
//! Крейт обрезает выбранный прямоугольник из кадра BGRA8 с учётом `origin` и
//! `stride`, переводит фрагмент в оттенки серого и запускает локальный
//! Tesseract через stdin/stdout. Кадр и результат не создают временных файлов.

mod crop;
mod engine;
mod region;

pub use crop::{BgraFrame, CropError, GrayImage, Selection, BYTES_PER_PIXEL};
pub use engine::{
    recognize_selection, OcrEngine, OcrError, OcrJob, OcrLanguage, OcrText, TesseractEngine,
};
pub use region::{
    Point, RegionError, RegionFrame, RegionPicker, RegionSession, SelectionPhase, SelectionUpdate,
    StartOutcome,
};
