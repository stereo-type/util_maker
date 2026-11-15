//! Запуск локального Tesseract через stdin/stdout.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use crate::crop::{BgraFrame, CropError, GrayImage, Selection};

/// Языки, доступные в форме плагина.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OcrLanguage {
    #[default]
    RussianEnglish,
    Russian,
    English,
}

impl OcrLanguage {
    /// Значение для Tesseract `-l`.
    pub const fn codes(self) -> &'static str {
        match self {
            Self::RussianEnglish => "rus+eng",
            Self::Russian => "rus",
            Self::English => "eng",
        }
    }

    /// Отдельные локальные модели, которые должны быть в пакете.
    pub fn required_codes(self) -> &'static [&'static str] {
        match self {
            Self::RussianEnglish => &["rus", "eng"],
            Self::Russian => &["rus"],
            Self::English => &["eng"],
        }
    }

    /// Название режима для интерфейса.
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::RussianEnglish => "русский и английский",
            Self::Russian => "русский",
            Self::English => "английский",
        }
    }

    /// Название отсутствующей модели по коду Tesseract.
    pub fn display_name_for_code(code: &str) -> Option<&'static str> {
        match code {
            "rus" => Some("русский"),
            "eng" => Some("английский"),
            _ => None,
        }
    }

    /// Разбирает значение настройки. Неизвестное значение не подменяется
    /// режимом по умолчанию.
    pub fn from_setting(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "rus+eng" | "ru+en" | "russian-english" | "both" => Some(Self::RussianEnglish),
            "rus" | "ru" | "russian" => Some(Self::Russian),
            "eng" | "en" | "english" => Some(Self::English),
            _ => None,
        }
    }
}

/// Строки результата в порядке чтения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcrText {
    lines: Vec<String>,
}

impl OcrText {
    /// Нормализует вывод движка: убирает `\r`, служебный `\x0c` и пустые
    /// строки, но сохраняет порядок и пунктуацию.
    pub fn from_text(text: &str) -> Self {
        let cleaned = text.replace('\u{000c}', "");
        let lines = cleaned
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
        Self { lines }
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
}

/// Ошибка распознавания или подготовки фрагмента.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OcrError {
    #[error("Текст не найден")]
    NoText,
    #[error("модель языка «{language}» не найдена. Восстановите пакет OCR, чтобы продолжить.")]
    MissingLanguage { language: String },
    #[error("движок OCR не найден: {path}. Установите Tesseract или восстановите пакет OCR.")]
    EngineUnavailable { path: String },
    #[error("движок OCR завершился с ошибкой: {message}")]
    EngineFailed { message: String },
    #[error("движок OCR вернул не UTF-8")]
    InvalidOutput,
    #[error("не удалось подготовить кадр для OCR: {0}")]
    Crop(#[from] CropError),
    #[error("не удалось запустить OCR: {0}")]
    Io(String),
    #[error("задание OCR уже завершено")]
    JobFinished,
}

impl From<std::io::Error> for OcrError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

/// Переносимый движок распознавания. Реализация не обязана быть Tesseract.
pub trait OcrEngine {
    fn recognize(&self, image: &GrayImage, language: OcrLanguage) -> Result<OcrText, OcrError>;
}

/// Tesseract, запускаемый отдельным процессом.
///
/// `tessdata_dir` указывает на каталог с `rus.traineddata` и `eng.traineddata`
/// внутри пакета. Если каталог не задан, используется системный.
#[derive(Debug, Clone)]
pub struct TesseractEngine {
    executable: PathBuf,
    tessdata_dir: Option<PathBuf>,
}

impl TesseractEngine {
    pub fn new(executable: impl Into<PathBuf>, tessdata_dir: Option<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            tessdata_dir,
        }
    }

    pub fn system() -> Self {
        Self::new("tesseract", None)
    }

    pub fn with_tessdata_dir(tessdata_dir: impl Into<PathBuf>) -> Self {
        Self::new("tesseract", Some(tessdata_dir.into()))
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn tessdata_dir(&self) -> Option<&Path> {
        self.tessdata_dir.as_deref()
    }

    fn ensure_models(&self, language: OcrLanguage) -> Result<(), OcrError> {
        let Some(directory) = &self.tessdata_dir else {
            return Ok(());
        };
        for code in language.required_codes() {
            if !directory.join(format!("{code}.traineddata")).is_file() {
                let name = OcrLanguage::display_name_for_code(code)
                    .unwrap_or(language.display_name())
                    .to_owned();
                return Err(OcrError::MissingLanguage { language: name });
            }
        }
        Ok(())
    }

    fn engine_unavailable(&self) -> OcrError {
        OcrError::EngineUnavailable {
            path: self.executable.display().to_string(),
        }
    }

    /// Запускает распознавание и возвращает задание, которое можно отменить.
    pub fn spawn(&self, image: &GrayImage, language: OcrLanguage) -> Result<OcrJob, OcrError> {
        self.ensure_models(language)?;

        let mut command = Command::new(&self.executable);
        command
            .arg("stdin")
            .arg("stdout")
            .arg("-l")
            .arg(language.codes())
            .arg("--psm")
            .arg("6");
        if let Some(directory) = &self.tessdata_dir {
            command.arg("--tessdata-dir").arg(directory);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                self.engine_unavailable()
            } else {
                OcrError::Io(error.to_string())
            }
        })?;

        let pnm = image.to_pnm();
        let write_result = child
            .stdin
            .take()
            .map(|mut stdin| stdin.write_all(&pnm))
            .transpose();
        if let Err(error) = write_result {
            let _ = child.kill();
            let _ = child.wait();
            return Err(OcrError::Io(error.to_string()));
        }

        Ok(OcrJob { child: Some(child) })
    }

    pub fn recognize(&self, image: &GrayImage, language: OcrLanguage) -> Result<OcrText, OcrError> {
        self.spawn(image, language)?.wait()
    }
}

impl OcrEngine for TesseractEngine {
    fn recognize(&self, image: &GrayImage, language: OcrLanguage) -> Result<OcrText, OcrError> {
        TesseractEngine::recognize(self, image, language)
    }
}

/// Запущенный процесс OCR.
///
/// `wait` дочитывает вывод, `cancel` завершает процесс. `Drop` также завершает
/// процесс, поэтому отмена или падение вызывающего кода не оставляет Tesseract
/// работать в фоне.
#[derive(Debug)]
pub struct OcrJob {
    child: Option<Child>,
}

impl OcrJob {
    #[cfg(test)]
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    pub fn wait(mut self) -> Result<OcrText, OcrError> {
        self.wait_inner()
    }

    fn wait_inner(&mut self) -> Result<OcrText, OcrError> {
        let child = self.child.take().ok_or(OcrError::JobFinished)?;
        let output = child.wait_with_output().map_err(OcrError::from)?;
        parse_output(output)
    }

    pub fn cancel(mut self) -> Result<(), OcrError> {
        self.cancel_inner()
    }

    fn cancel_inner(&mut self) -> Result<(), OcrError> {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        Ok(())
    }
}

impl Drop for OcrJob {
    fn drop(&mut self) {
        let _ = self.cancel_inner();
    }
}

fn parse_output(output: Output) -> Result<OcrText, OcrError> {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if let Some(language) = missing_language(&stderr) {
        return Err(OcrError::MissingLanguage {
            language: language.to_owned(),
        });
    }
    if !output.status.success() {
        let message = stderr
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .unwrap_or("неизвестная ошибка");
        return Err(OcrError::EngineFailed {
            message: truncate(message, 300),
        });
    }

    let text = String::from_utf8(output.stdout).map_err(|_| OcrError::InvalidOutput)?;
    let text = OcrText::from_text(&text);
    if text.is_empty() {
        Err(OcrError::NoText)
    } else {
        Ok(text)
    }
}

fn missing_language(stderr: &str) -> Option<&'static str> {
    for (code, name) in [("rus", "русский"), ("eng", "английский")] {
        if stderr.contains(&format!("Failed loading language '{code}'"))
            || stderr.contains(&format!("{code}.traineddata"))
        {
            return Some(name);
        }
    }
    None
}

fn truncate(message: &str, max_chars: usize) -> String {
    if message.chars().count() <= max_chars {
        return message.to_owned();
    }
    let mut result: String = message.chars().take(max_chars).collect();
    result.push('…');
    result
}

/// Обрезает фрагмент и передаёт его движку.
pub fn recognize_selection(
    engine: &dyn OcrEngine,
    frame: &BgraFrame<'_>,
    selection: Selection,
    language: OcrLanguage,
) -> Result<OcrText, OcrError> {
    let image = frame.crop(selection)?;
    engine.recognize(&image, language)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crop::BYTES_PER_PIXEL;
    use util_maker_sdk::FrameOrigin;

    fn white_image() -> GrayImage {
        let pixels = [255u8; BYTES_PER_PIXEL];
        let frame = BgraFrame::new(&pixels, 1, 1, BYTES_PER_PIXEL, FrameOrigin { x: 0, y: 0 })
            .expect("кадр");
        frame.crop(Selection::new(0, 0, 1, 1)).expect("изображение")
    }

    #[test]
    fn language_codes_and_settings_are_stable() {
        assert_eq!(OcrLanguage::default(), OcrLanguage::RussianEnglish);
        assert_eq!(OcrLanguage::RussianEnglish.codes(), "rus+eng");
        assert_eq!(OcrLanguage::Russian.codes(), "rus");
        assert_eq!(OcrLanguage::English.codes(), "eng");
        assert_eq!(
            OcrLanguage::from_setting("RUS+ENG"),
            Some(OcrLanguage::RussianEnglish)
        );
        assert_eq!(OcrLanguage::from_setting("ru"), Some(OcrLanguage::Russian));
        assert_eq!(
            OcrLanguage::from_setting("english"),
            Some(OcrLanguage::English)
        );
        assert_eq!(OcrLanguage::from_setting("deu"), None);
    }

    #[test]
    fn text_normalizes_form_feed_and_empty_lines() {
        let text = OcrText::from_text("  Ошибка 404 \r\n\r\nFile missing\u{000c}\n\n");
        assert_eq!(text.lines(), &["Ошибка 404", "File missing"]);
        assert_eq!(text.text(), "Ошибка 404\nFile missing");
        assert!(!text.is_empty());
        assert!(OcrText::from_text(" \n\u{000c}\n").is_empty());
    }

    #[test]
    fn missing_model_is_reported_before_start() {
        let engine = TesseractEngine::new(
            "/definitely/not/tesseract",
            Some(PathBuf::from("/definitely/not/tessdata")),
        );
        assert!(matches!(
            engine.spawn(&white_image(), OcrLanguage::Russian),
            Err(OcrError::MissingLanguage { language }) if language == "русский"
        ));
    }

    #[test]
    fn missing_executable_is_reported() {
        let engine = TesseractEngine::new("/definitely/not/tesseract", None);
        assert!(matches!(
            engine.spawn(&white_image(), OcrLanguage::Russian),
            Err(OcrError::EngineUnavailable { .. })
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cancel_reaps_child_process() {
        let child = Command::new("sh")
            .arg("-c")
            .arg("sleep 30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("sleep");
        let pid = child.id();
        let job = OcrJob::new(child);
        job.cancel().expect("отмена");
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }
}
