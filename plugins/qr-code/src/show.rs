//! Состояние окна показа QR и выбор исходного текста.
//!
//! Окно не копирует текст в системный буфер: пользователь может исправить
//! содержимое и получить новый код. При пустом или слишком длинном вводе
//! прежний код сразу убирается, а причина остаётся у поля.

use serde::Serialize;
use serde_json::Value;

use crate::{EncodeError, QrLayout, QrMatrix, RenderError};

/// Размер окна по умолчанию в физических пикселях.
pub const DEFAULT_WINDOW_WIDTH: u32 = 480;
/// Высота окна по умолчанию в физических пикселях.
pub const DEFAULT_WINDOW_HEIGHT: u32 = 600;

/// Размер окна в физических пикселях.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowSize {
    pub width: u32,
    pub height: u32,
}

impl Default for WindowSize {
    fn default() -> Self {
        Self {
            width: DEFAULT_WINDOW_WIDTH,
            height: DEFAULT_WINDOW_HEIGHT,
        }
    }
}

impl WindowSize {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// Читает размер из параметров действия. Некорректные значения заменяются
    /// размером по умолчанию, чтобы окно всё равно открылось.
    pub fn from_params(params: &Value) -> Self {
        let width = params
            .get("windowWidth")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0);
        let height = params
            .get("windowHeight")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0);
        Self {
            width: width.unwrap_or(DEFAULT_WINDOW_WIDTH),
            height: height.unwrap_or(DEFAULT_WINDOW_HEIGHT),
        }
    }
}

/// Состояние исходного выделения, полученное от хоста.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionRead {
    Text(String),
    Empty,
    Unsupported,
    Password,
    SourceLost,
    Unavailable(String),
}

impl SelectionRead {
    /// Разбирает ответ `selection.get`, не извлекая текст из парольного поля.
    pub fn from_response(value: &Value) -> Self {
        let state = value.get("state").unwrap_or(value);
        let Some(kind) = state.get("kind").and_then(Value::as_str) else {
            return Self::Unavailable("ответ selection.get без состояния".to_owned());
        };
        match kind {
            "text" => match state.get("text").and_then(Value::as_str) {
                Some(text) if !text.is_empty() => Self::Text(text.to_owned()),
                Some(_) => Self::Empty,
                None => Self::Unsupported,
            },
            "empty" => Self::Empty,
            "unsupported" => Self::Unsupported,
            "password" => Self::Password,
            "sourceLost" | "source_lost" | "source-lost" => Self::SourceLost,
            other => Self::Unavailable(format!("неизвестное состояние выделения: {other}")),
        }
    }
}

/// Откуда взялся показанный текст.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum InputSource {
    Selection,
    Manual,
}

/// Почему пользователю предложено ручное поле вместо выделения.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ManualReason {
    NoSelection,
    Unsupported,
    Password,
    SourceLost,
    SelectionUnavailable,
}

impl ManualReason {
    pub fn message(self) -> &'static str {
        match self {
            Self::NoSelection => "Выделение пустое. Введите или вставьте текст.",
            Self::Unsupported => "Источник не отдаёт текст. Введите или вставьте его.",
            Self::Password => "Поле пароля не читается. Введите текст вручную.",
            Self::SourceLost => "Исходное выделение потеряно. Введите или вставьте текст.",
            Self::SelectionUnavailable => {
                "Выделение недоступно. Введите текст вручную или выделите его в другой программе и вызовите действие сочетанием клавиш."
            }
        }
    }
}

/// Фаза окна показа.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShowPhase {
    Open,
    Closed,
}

/// Исход попытки открыть окно повторной командой.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShowStart {
    Started,
    AlreadyActive,
}

/// Ошибка ввода или размера окна, которую видит пользователь.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ShowError {
    EmptyInput,
    TooLong {
        bytes: usize,
    },
    TooSmall {
        width: u32,
        height: u32,
        minimum: u32,
    },
}

impl ShowError {
    pub fn message(&self) -> String {
        match self {
            Self::EmptyInput => "Введите текст для QR-кода.".to_owned(),
            Self::TooLong { bytes } => {
                format!("Текст не помещается в один QR-код: {bytes} байт UTF-8.")
            }
            Self::TooSmall {
                width,
                height,
                minimum,
            } => format!(
                "Окно {width}×{height} слишком мало для QR. Увеличьте окно или сократите текст: нужно минимум {minimum}×{minimum} пикселей."
            ),
        }
    }

    /// Подсказка для случая, когда код не помещается.
    pub fn advice(&self) -> Option<&'static str> {
        match self {
            Self::TooLong { .. } | Self::TooSmall { .. } => {
                Some("Увеличьте окно или сократите текст.")
            }
            Self::EmptyInput => None,
        }
    }
}

/// Матрица и целый масштаб для отрисовки окном.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QrView {
    pub version: u8,
    pub size: usize,
    pub modules: Vec<bool>,
    pub layout: QrLayout,
}

/// Снимок окна для хоста и тестов.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShowView {
    pub phase: ShowPhase,
    pub input: InputSource,
    pub manual_reason: Option<ManualReason>,
    pub source_error: Option<String>,
    pub text: String,
    pub qr: Option<QrView>,
    pub error: Option<ShowError>,
    pub width: u32,
    pub height: u32,
    pub message: String,
}

/// Окно показа QR: текст, матрица, масштаб и причина ошибки.
#[derive(Debug, Clone)]
pub struct ShowWindow {
    phase: ShowPhase,
    input: InputSource,
    manual_reason: Option<ManualReason>,
    source_error: Option<String>,
    text: String,
    matrix: Option<QrMatrix>,
    qr: Option<QrView>,
    error: Option<ShowError>,
    size: WindowSize,
}

impl ShowWindow {
    pub fn from_selection(selection: SelectionRead, size: WindowSize) -> Self {
        let mut window = Self::new(size);
        match selection {
            SelectionRead::Text(text) => {
                window.input = InputSource::Selection;
                window.text = text;
            }
            SelectionRead::Empty => window.set_manual(ManualReason::NoSelection, None),
            SelectionRead::Unsupported => window.set_manual(ManualReason::Unsupported, None),
            SelectionRead::Password => window.set_manual(ManualReason::Password, None),
            SelectionRead::SourceLost => window.set_manual(ManualReason::SourceLost, None),
            SelectionRead::Unavailable(detail) => {
                window.set_manual(ManualReason::SelectionUnavailable, Some(detail))
            }
        }
        window.refresh();
        window
    }

    pub fn manual(reason: ManualReason, detail: Option<String>, size: WindowSize) -> Self {
        let mut window = Self::new(size);
        window.set_manual(reason, detail);
        window.refresh();
        window
    }

    pub fn manual_text(text: impl Into<String>, size: WindowSize) -> Self {
        let mut window = Self::new(size);
        window.input = InputSource::Manual;
        window.text = text.into();
        window.refresh();
        window
    }

    fn new(size: WindowSize) -> Self {
        Self {
            phase: ShowPhase::Open,
            input: InputSource::Manual,
            manual_reason: None,
            source_error: None,
            text: String::new(),
            matrix: None,
            qr: None,
            error: None,
            size,
        }
    }

    fn set_manual(&mut self, reason: ManualReason, detail: Option<String>) {
        self.input = InputSource::Manual;
        self.manual_reason = Some(reason);
        self.source_error = detail;
    }

    pub fn is_open(&self) -> bool {
        self.phase == ShowPhase::Open
    }

    pub fn size(&self) -> WindowSize {
        self.size
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn qr(&self) -> Option<&QrView> {
        self.qr.as_ref()
    }

    pub fn error(&self) -> Option<&ShowError> {
        self.error.as_ref()
    }

    /// Повторный вызов активирует уже открытое окно без нового выделения.
    pub fn activate(&mut self) -> ShowStart {
        if self.is_open() {
            ShowStart::AlreadyActive
        } else {
            self.phase = ShowPhase::Open;
            ShowStart::Started
        }
    }

    /// Правит текст и пересобирает код. При ошибке старый код убирается.
    pub fn edit(&mut self, text: impl Into<String>) {
        self.input = InputSource::Manual;
        self.manual_reason = None;
        self.source_error = None;
        self.text = text.into();
        self.refresh();
    }

    /// Пересчитывает целый масштаб после смены размера или DPI.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.size = WindowSize::new(width, height);
        self.apply_layout();
    }

    /// Esc закрывает окно и очищает текст и матрицу.
    pub fn close(&mut self) {
        self.phase = ShowPhase::Closed;
        self.manual_reason = None;
        self.source_error = None;
        self.text.clear();
        self.matrix = None;
        self.qr = None;
        self.error = None;
    }

    fn refresh(&mut self) {
        self.matrix = None;
        self.qr = None;
        self.error = None;
        if self.text.is_empty() {
            self.error = Some(ShowError::EmptyInput);
            return;
        }
        match QrMatrix::encode(&self.text) {
            Ok(matrix) => {
                self.matrix = Some(matrix);
                self.apply_layout();
            }
            Err(EncodeError::EmptyInput) => {
                self.error = Some(ShowError::EmptyInput);
            }
            Err(EncodeError::TooLong { bytes }) => {
                self.error = Some(ShowError::TooLong { bytes });
            }
        }
    }

    fn apply_layout(&mut self) {
        self.qr = None;
        self.error = None;
        let Some(matrix) = self.matrix.as_ref() else {
            return;
        };
        match matrix.layout(self.size.width, self.size.height) {
            Ok(layout) => {
                self.qr = Some(QrView {
                    version: matrix.version(),
                    size: matrix.size(),
                    modules: matrix.modules().to_vec(),
                    layout,
                });
            }
            Err(RenderError::EmptyWindow) => {
                self.error = Some(ShowError::TooSmall {
                    width: self.size.width,
                    height: self.size.height,
                    minimum: matrix.size() as u32 + crate::QUIET_ZONE_MODULES * 2,
                });
            }
            Err(RenderError::TooSmall {
                width,
                height,
                minimum,
            }) => {
                self.error = Some(ShowError::TooSmall {
                    width,
                    height,
                    minimum,
                });
            }
        }
    }

    pub fn view(&self) -> ShowView {
        let message = if self.phase == ShowPhase::Closed {
            "Окно закрыто".to_owned()
        } else if self.text.is_empty() {
            self.manual_reason.map_or_else(
                || ShowError::EmptyInput.message(),
                |reason| reason.message().to_owned(),
            )
        } else if let Some(error) = &self.error {
            match error.advice() {
                Some(advice) => format!("{} {advice}", error.message()),
                None => error.message(),
            }
        } else if self.qr.is_some() {
            "QR-код готов. Его можно считать телефоном.".to_owned()
        } else {
            "Введите или вставьте текст.".to_owned()
        };
        ShowView {
            phase: self.phase,
            input: self.input,
            manual_reason: self.manual_reason,
            source_error: self.source_error.clone(),
            text: self.text.clone(),
            qr: self.qr.clone(),
            error: self.error.clone(),
            width: self.size.width,
            height: self.size.height,
            message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn size(width: u32, height: u32) -> WindowSize {
        WindowSize::new(width, height)
    }

    #[test]
    fn selection_text_is_shown_byte_for_byte_with_qr() {
        let text = "  Привет, мир! 👋\nСтрока 2\t— конец  ";
        let window =
            ShowWindow::from_selection(SelectionRead::Text(text.to_owned()), size(480, 600));
        let view = window.view();
        assert_eq!(view.input, InputSource::Selection);
        assert_eq!(view.text, text);
        assert!(view.error.is_none());
        let qr = view.qr.expect("QR");
        assert!(qr.version >= 1);
        assert_eq!(qr.modules.len(), qr.size * qr.size);
        assert_eq!(qr.layout.total_modules, qr.size as u32 + 8);
        assert!(qr.layout.scale >= 1);
    }

    #[test]
    fn password_state_never_exposes_text() {
        let response = json!({
            "state": {
                "kind": "password",
                "text": "секрет",
                "bounds": {"x": 0, "y": 0, "width": 100, "height": 20}
            }
        });
        let window =
            ShowWindow::from_selection(SelectionRead::from_response(&response), size(480, 600));
        let view = window.view();
        assert_eq!(view.manual_reason, Some(ManualReason::Password));
        assert_eq!(view.text, "");
        assert!(view.qr.is_none());
        assert!(view.error.is_some());
        assert!(view.message.contains("Поле пароля"), "{}", view.message);
    }

    #[test]
    fn empty_and_unavailable_sources_open_manual_input() {
        let empty = ShowWindow::from_selection(SelectionRead::Empty, size(480, 600));
        assert_eq!(empty.view().input, InputSource::Manual);
        assert_eq!(empty.view().manual_reason, Some(ManualReason::NoSelection));

        let unavailable = ShowWindow::from_selection(
            SelectionRead::Unavailable("нет моста".to_owned()),
            size(480, 600),
        );
        let view = unavailable.view();
        assert_eq!(view.manual_reason, Some(ManualReason::SelectionUnavailable));
        assert_eq!(view.source_error.as_deref(), Some("нет моста"));
        assert_eq!(view.text, "");
        assert!(
            view.message.contains("Выделение недоступно"),
            "{}",
            view.message
        );
    }

    #[test]
    fn invalid_input_removes_previous_qr() {
        let mut window = ShowWindow::manual_text("ok", size(480, 600));
        assert!(window.qr().is_some());
        window.edit("");
        assert!(window.qr().is_none());
        assert_eq!(window.error(), Some(&ShowError::EmptyInput));
        window.edit("ok");
        assert!(window.qr().is_some());
        window.edit("a".repeat(5000));
        assert!(window.qr().is_none());
        assert!(matches!(
            window.error(),
            Some(ShowError::TooLong { bytes: 5000 })
        ));
        assert_eq!(window.text().len(), 5000, "ввод не усекается");
    }

    #[test]
    fn resize_recomputes_integer_scale_and_hides_code_when_too_small() {
        let mut window = ShowWindow::manual_text("ok", size(200, 200));
        let small_scale = window.qr().expect("QR").layout.scale;
        window.resize(400, 400);
        let large_scale = window.qr().expect("QR").layout.scale;
        assert!(large_scale > small_scale);
        window.resize(10, 10);
        assert!(window.qr().is_none());
        assert!(matches!(window.error(), Some(ShowError::TooSmall { .. })));
        window.resize(400, 400);
        assert!(window.qr().is_some());
    }

    #[test]
    fn repeat_activates_and_escape_clears_content() {
        let mut window = ShowWindow::manual_text("https://example.com", size(400, 400));
        assert_eq!(window.activate(), ShowStart::AlreadyActive);
        window.close();
        assert!(!window.is_open());
        assert_eq!(window.text(), "");
        assert!(window.qr().is_none());
        assert_eq!(window.activate(), ShowStart::Started);
    }
}
