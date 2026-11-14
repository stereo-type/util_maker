//! Интерактивное состояние редактора «Скриншотов» без привязки к окну.
//!
//! Модуль получает кадр, пользователь выбирает область, добавляет аннотации и
//! решает, когда писать результат в буфер. Панель и маркеры в этот слой не
//! входят; кадр освобождается только после успешного копирования.

use image::RgbaImage;

use crate::{render, Annotation, Frame, RenderError, Selection};

/// Ошибка записи в буфер обмена.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClipboardError {
    #[error("буфер обмена занят")]
    Busy,
    #[error("{0}")]
    Other(String),
}

/// Приёмник готового изображения. В бою реализуется адаптером Windows.
pub trait ClipboardSink {
    fn write_image(&mut self, image: &RgbaImage) -> Result<(), ClipboardError>;
}

/// Исход попытки копирования.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyOutcome {
    Copied,
    ClipboardBusy,
}

/// Исход повторного шорткея.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeginOutcome {
    Started,
    AlreadyActive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorPhase {
    Idle,
    Ready,
    Editing,
    Completed,
}

#[derive(Debug, thiserror::Error)]
pub enum EditorError {
    #[error("кадр ещё не получен")]
    NoFrame,
    #[error("область ещё не выбрана")]
    NoSelection,
    #[error(transparent)]
    Render(#[from] RenderError),
    #[error("буфер обмена: {0}")]
    Clipboard(String),
}

/// Состояние одного вызова «Снять область».
#[derive(Debug, Default)]
pub struct EditorSession {
    frame: Option<Frame>,
    selection: Option<Selection>,
    annotations: Vec<Annotation>,
    phase: Option<EditorPhase>,
}

impl EditorSession {
    pub fn new() -> Self {
        Self {
            frame: None,
            selection: None,
            annotations: Vec::new(),
            phase: Some(EditorPhase::Idle),
        }
    }

    /// Открывает редактор после получения кадра. Повторный шорткей не делает
    /// новый снимок, а возвращает к текущему захвату.
    pub fn begin(&mut self, frame: Frame) -> BeginOutcome {
        if self.frame.is_some() {
            return BeginOutcome::AlreadyActive;
        }
        self.frame = Some(frame);
        self.selection = None;
        self.annotations.clear();
        self.phase = Some(EditorPhase::Ready);
        BeginOutcome::Started
    }

    pub fn repeat_shortcut(&self) -> bool {
        self.frame.is_some()
    }

    /// Задаёт область в физических пикселях и проверяет её по кадру.
    pub fn select(&mut self, selection: Selection) -> Result<(), EditorError> {
        let frame = self.frame.as_ref().ok_or(EditorError::NoFrame)?;
        render(frame, selection, &[])?;
        self.selection = Some(selection);
        self.phase = Some(EditorPhase::Editing);
        Ok(())
    }

    pub fn add_annotation(&mut self, annotation: Annotation) -> Result<(), EditorError> {
        if self.selection.is_none() {
            return Err(EditorError::NoSelection);
        }
        self.annotations.push(annotation);
        self.phase = Some(EditorPhase::Editing);
        Ok(())
    }

    pub fn undo_last(&mut self) -> Option<Annotation> {
        let removed = self.annotations.pop();
        if self.annotations.is_empty() && self.selection.is_some() {
            self.phase = Some(EditorPhase::Editing);
        }
        removed
    }

    pub fn annotations(&self) -> &[Annotation] {
        &self.annotations
    }

    pub fn selection(&self) -> Option<Selection> {
        self.selection
    }

    pub fn phase(&self) -> EditorPhase {
        self.phase.unwrap_or(EditorPhase::Idle)
    }

    pub fn frame_held(&self) -> bool {
        self.frame.is_some()
    }

    /// Сводит кадр и пишет его в буфер. При занятости буфера состояние и кадр
    /// сохраняются для повторной попытки.
    pub fn copy(&mut self, sink: &mut dyn ClipboardSink) -> Result<CopyOutcome, EditorError> {
        let frame = self.frame.as_ref().ok_or(EditorError::NoFrame)?;
        let selection = self.selection.ok_or(EditorError::NoSelection)?;
        let image = render(frame, selection, &self.annotations)?;
        match sink.write_image(&image) {
            Ok(()) => {
                self.frame = None;
                self.selection = None;
                self.annotations.clear();
                self.phase = Some(EditorPhase::Completed);
                Ok(CopyOutcome::Copied)
            }
            Err(ClipboardError::Busy) => Ok(CopyOutcome::ClipboardBusy),
            Err(ClipboardError::Other(message)) => Err(EditorError::Clipboard(message)),
        }
    }

    /// Esc до успешного копирования. Буфер не трогается; кадр освобождается.
    pub fn escape(&mut self) -> bool {
        let was_active = self.frame.is_some();
        self.frame = None;
        self.selection = None;
        self.annotations.clear();
        self.phase = Some(EditorPhase::Idle);
        was_active
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Point, RenderError};
    use image::{Rgba, RgbaImage};

    struct MockClipboard {
        written: Vec<RgbaImage>,
        busy: bool,
    }

    impl MockClipboard {
        fn ready() -> Self {
            Self {
                written: Vec::new(),
                busy: false,
            }
        }

        fn busy() -> Self {
            Self {
                written: Vec::new(),
                busy: true,
            }
        }
    }

    impl ClipboardSink for MockClipboard {
        fn write_image(&mut self, image: &RgbaImage) -> Result<(), ClipboardError> {
            if self.busy {
                return Err(ClipboardError::Busy);
            }
            self.written.push(image.clone());
            Ok(())
        }
    }

    fn frame() -> Frame {
        let mut image = RgbaImage::new(100, 80);
        for y in 0..80u32 {
            for x in 0..100u32 {
                image.put_pixel(
                    x,
                    y,
                    Rgba([(x % 251) as u8, (y % 241) as u8, ((x + y) % 239) as u8, 255]),
                );
            }
        }
        Frame::new(Point::new(-50, -30), image)
    }

    fn selection() -> Selection {
        Selection::new(-40, -20, 20, 10)
    }

    #[test]
    fn copy_without_annotations_is_pixel_identical() {
        let source = frame();
        let mut session = EditorSession::new();
        assert_eq!(session.begin(source.clone()), BeginOutcome::Started);
        session.select(selection()).expect("выбор");
        let mut clipboard = MockClipboard::ready();
        assert_eq!(session.copy(&mut clipboard).unwrap(), CopyOutcome::Copied);
        assert_eq!(clipboard.written.len(), 1);
        let result = &clipboard.written[0];
        assert_eq!(result.dimensions(), (20, 10));
        for y in 0..10u32 {
            for x in 0..20u32 {
                let source_pixel = source.image.get_pixel(
                    (selection().x - source.origin.x) as u32 + x,
                    (selection().y - source.origin.y) as u32 + y,
                );
                assert_eq!(result.get_pixel(x, y), source_pixel);
            }
        }
        assert!(!session.frame_held());
    }

    #[test]
    fn undo_removes_only_last_annotation_and_panel_is_absent() {
        let source = frame();
        let mut session = EditorSession::new();
        session.begin(source.clone());
        session.select(selection()).expect("выбор");
        let arrow = Annotation::Arrow {
            from: Point::new(-35, -15),
            to: Point::new(-25, -10),
            color: [255, 0, 0, 255],
            thickness: 2,
        };
        let rectangle = Annotation::Rectangle {
            rect: Selection::new(-35, -18, 10, 6),
            color: [0, 255, 0, 255],
            thickness: 2,
        };
        session.add_annotation(arrow).expect("стрелка");
        session.add_annotation(rectangle).expect("рамка");
        assert_eq!(session.undo_last(), Some(rectangle));
        assert_eq!(session.annotations(), &[arrow]);

        let mut clipboard = MockClipboard::ready();
        session.copy(&mut clipboard).expect("копирование");
        let expected = render(&source, selection(), &[arrow]).expect("эталон");
        assert_eq!(clipboard.written[0], expected);
    }

    #[test]
    fn busy_clipboard_keeps_frame_and_annotations_for_retry() {
        let mut session = EditorSession::new();
        session.begin(frame());
        session.select(selection()).expect("выбор");
        session
            .add_annotation(Annotation::Rectangle {
                rect: Selection::new(-35, -18, 10, 6),
                color: [0, 255, 0, 255],
                thickness: 2,
            })
            .expect("рамка");
        let mut busy = MockClipboard::busy();
        assert_eq!(session.copy(&mut busy).unwrap(), CopyOutcome::ClipboardBusy);
        assert!(session.frame_held());
        assert_eq!(session.annotations().len(), 1);

        let mut clipboard = MockClipboard::ready();
        assert_eq!(session.copy(&mut clipboard).unwrap(), CopyOutcome::Copied);
        assert!(!session.frame_held());
        assert!(session.annotations().is_empty());
    }

    #[test]
    fn escape_preserves_previous_clipboard_and_frees_frame() {
        let mut session = EditorSession::new();
        session.begin(frame());
        session.select(selection()).expect("выбор");
        session
            .add_annotation(Annotation::Arrow {
                from: Point::new(-35, -15),
                to: Point::new(-25, -10),
                color: [255, 0, 0, 255],
                thickness: 2,
            })
            .expect("стрелка");
        let clipboard = MockClipboard::ready();
        assert!(session.escape());
        assert!(!session.frame_held());
        assert!(clipboard.written.is_empty());
    }

    #[test]
    fn selection_crossing_virtual_desktop_origin_matches_pixels() {
        let source = frame();
        let selection = Selection::new(-10, -10, 20, 20);
        let mut session = EditorSession::new();
        session.begin(source.clone());
        session
            .select(selection)
            .expect("выбор через начало координат");
        let mut clipboard = MockClipboard::ready();
        session.copy(&mut clipboard).expect("копирование");
        let result = &clipboard.written[0];
        assert_eq!(result.dimensions(), (20, 20));
        for y in 0..20u32 {
            for x in 0..20u32 {
                let pixel = source.image.get_pixel(
                    (selection.x - source.origin.x) as u32 + x,
                    (selection.y - source.origin.y) as u32 + y,
                );
                assert_eq!(result.get_pixel(x, y), pixel);
            }
        }
    }

    #[test]
    fn repeat_shortcut_returns_to_current_capture() {
        let mut session = EditorSession::new();
        assert_eq!(session.begin(frame()), BeginOutcome::Started);
        session.select(selection()).expect("выбор");
        assert!(session.repeat_shortcut());
        assert_eq!(session.begin(frame()), BeginOutcome::AlreadyActive);
        assert_eq!(session.selection(), Some(selection()));
    }

    #[test]
    fn invalid_selection_is_rejected() {
        let mut session = EditorSession::new();
        session.begin(frame());
        assert!(matches!(
            session.select(Selection::new(60, 0, 10, 10)),
            Err(EditorError::Render(RenderError::OutOfFrame))
        ));
        assert_eq!(session.phase(), EditorPhase::Ready);
    }
}
