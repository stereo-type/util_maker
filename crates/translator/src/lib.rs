//! Состояния выбора текста до и после вызова переводчика.
//!
//! Контроллер не знает, как получено выделение: источник представлен
//! адаптером, а запрос перевода — приёмником. Это позволяет проверить оба
//! порядка действий, отмену и ручной ввод на тестовых адаптерах без Windows.

/// Снимок исходного элемента в момент вызова.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceSnapshot {
    /// Уже выделенный текст; передаётся провайдеру ровно как есть.
    Selected(String),
    /// Текстовое выделение поддерживается, но диапазон пуст.
    Empty,
    /// Элемент не отдаёт текстовое выделение.
    Unsupported,
    /// Известное парольное поле.
    Password,
    /// Исходное окно закрылось или потеряло контекст.
    ContextLost,
}

/// Первый шаг после шорткея.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartOutcome {
    /// Немедленный запрос с готовым выделением.
    Translate(String),
    /// Показать «Выделите текст» и ждать один жест мыши.
    AwaitSelection,
    /// Предложить вставку или ручной ввод.
    ManualInput,
    /// Запрос запрещён (парольное поле или потерянный контекст).
    Blocked(BlockReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockReason {
    Password,
    ContextLost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Phase {
    #[default]
    Idle,
    Awaiting,
    Manual,
}

/// Состояние одного вызова переводчика.
#[derive(Debug, Default)]
pub struct SelectionController {
    phase: Phase,
    manual_text: Option<String>,
}

impl SelectionController {
    pub fn new() -> Self {
        Self::default()
    }

    /// Обрабатывает шорткей по снимку источника. Возвращает первый запрос,
    /// ожидание жеста или предложение ручного ввода.
    pub fn start(&mut self, snapshot: SourceSnapshot) -> StartOutcome {
        self.phase = Phase::Idle;
        self.manual_text = None;
        match snapshot {
            SourceSnapshot::Selected(text) => StartOutcome::Translate(text),
            SourceSnapshot::Empty => {
                self.phase = Phase::Awaiting;
                StartOutcome::AwaitSelection
            }
            SourceSnapshot::Unsupported => {
                self.phase = Phase::Manual;
                StartOutcome::ManualInput
            }
            SourceSnapshot::Password => StartOutcome::Blocked(BlockReason::Password),
            SourceSnapshot::ContextLost => StartOutcome::Blocked(BlockReason::ContextLost),
        }
    }

    /// Завершение жеста выделения в режиме ожидания. Запрос отдаётся ровно
    /// один раз; повторный жест того же вызова игнорируется.
    pub fn selection_finished(&mut self, text: impl Into<String>) -> Option<String> {
        if self.phase != Phase::Awaiting {
            return None;
        }
        self.phase = Phase::Idle;
        let text = text.into();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }

    /// Ручной ввод сохраняется, но запрос не отправляется до команды.
    pub fn manual_text(&mut self, text: impl Into<String>) -> bool {
        if self.phase != Phase::Manual {
            return false;
        }
        let text = text.into();
        if text.is_empty() {
            self.manual_text = None;
            false
        } else {
            self.manual_text = Some(text);
            true
        }
    }

    /// Явная команда «Перевести» для ручного режима.
    pub fn confirm_manual(&mut self) -> Option<String> {
        if self.phase != Phase::Manual {
            return None;
        }
        let text = self.manual_text.take()?;
        self.phase = Phase::Idle;
        Some(text)
    }

    /// Esc или смена исходного приложения. Возвращает true, если вызов был
    /// активен; запрос при этом не отправляется.
    pub fn cancel(&mut self) -> bool {
        let was_active = self.phase != Phase::Idle;
        self.phase = Phase::Idle;
        self.manual_text = None;
        was_active
    }

    pub fn is_waiting(&self) -> bool {
        self.phase == Phase::Awaiting
    }

    pub fn is_manual(&self) -> bool {
        self.phase == Phase::Manual
    }
}

/// Тестовый источник текста; в бою реализуется Windows-адаптером.
pub trait SourceAdapter {
    fn snapshot(&mut self) -> SourceSnapshot;
    /// Ждёт завершения одного жеста выделения; None — отмена или смена окна.
    fn wait_for_selection(&mut self) -> Option<String>;
}

/// Приёмник запросов перевода.
pub trait TranslationSink {
    fn request(&mut self, text: &str);
}

/// Сквозной сценарий шорткея: готовое выделение, ожидание жеста или ручной ввод.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowOutcome {
    Requested,
    AwaitingManualInput,
    Blocked,
    Cancelled,
}

pub fn run_shortcut<S: SourceAdapter, T: TranslationSink>(
    source: &mut S,
    sink: &mut T,
) -> FlowOutcome {
    let mut controller = SelectionController::new();
    match controller.start(source.snapshot()) {
        StartOutcome::Translate(text) => {
            sink.request(&text);
            FlowOutcome::Requested
        }
        StartOutcome::AwaitSelection => match source.wait_for_selection() {
            Some(text) => {
                if let Some(text) = controller.selection_finished(text) {
                    sink.request(&text);
                    FlowOutcome::Requested
                } else {
                    FlowOutcome::Cancelled
                }
            }
            None => {
                controller.cancel();
                FlowOutcome::Cancelled
            }
        },
        StartOutcome::ManualInput => FlowOutcome::AwaitingManualInput,
        StartOutcome::Blocked(_) => FlowOutcome::Blocked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestSource {
        snapshot: SourceSnapshot,
        gesture: Option<String>,
    }

    impl SourceAdapter for TestSource {
        fn snapshot(&mut self) -> SourceSnapshot {
            self.snapshot.clone()
        }

        fn wait_for_selection(&mut self) -> Option<String> {
            self.gesture.take()
        }
    }

    #[derive(Default)]
    struct RecordingSink {
        requests: Vec<String>,
    }

    impl TranslationSink for RecordingSink {
        fn request(&mut self, text: &str) {
            self.requests.push(text.to_owned());
        }
    }

    fn new_controller() -> SelectionController {
        SelectionController::new()
    }

    #[test]
    fn ready_selection_is_forwarded_exactly() {
        let text = "  строка с пробелами\nи переносом  ";
        let mut source = TestSource {
            snapshot: SourceSnapshot::Selected(text.to_owned()),
            gesture: None,
        };
        let mut sink = RecordingSink::default();
        assert_eq!(run_shortcut(&mut source, &mut sink), FlowOutcome::Requested);
        assert_eq!(sink.requests, vec![text.to_owned()]);
    }

    #[test]
    fn empty_range_requests_once_after_gesture() {
        let mut controller = new_controller();
        assert_eq!(
            controller.start(SourceSnapshot::Empty),
            StartOutcome::AwaitSelection
        );
        assert!(controller.is_waiting());
        assert_eq!(
            controller.selection_finished("выбранный текст"),
            Some("выбранный текст".to_owned())
        );
        assert_eq!(controller.selection_finished("повторный жест"), None);
        assert!(!controller.is_waiting());
    }

    #[test]
    fn escape_and_context_change_produce_no_request() {
        let mut controller = new_controller();
        controller.start(SourceSnapshot::Empty);
        assert!(controller.cancel());
        assert_eq!(controller.selection_finished("текст"), None);

        let mut another = new_controller();
        another.start(SourceSnapshot::Empty);
        assert!(another.cancel());
        assert!(!another.is_waiting());
    }

    #[test]
    fn password_field_blocks_before_any_request() {
        let mut source = TestSource {
            snapshot: SourceSnapshot::Password,
            gesture: Some("секрет".to_owned()),
        };
        let mut sink = RecordingSink::default();
        assert_eq!(run_shortcut(&mut source, &mut sink), FlowOutcome::Blocked);
        assert!(sink.requests.is_empty());
    }

    #[test]
    fn unsupported_source_requires_explicit_manual_command() {
        let mut controller = new_controller();
        assert_eq!(
            controller.start(SourceSnapshot::Unsupported),
            StartOutcome::ManualInput
        );
        assert!(controller.manual_text("вставленный текст"));
        assert_eq!(
            controller.confirm_manual(),
            Some("вставленный текст".to_owned())
        );
        assert_eq!(controller.confirm_manual(), None);
    }

    #[test]
    fn manual_text_alone_never_sends_request() {
        let mut source = TestSource {
            snapshot: SourceSnapshot::Unsupported,
            gesture: None,
        };
        let mut sink = RecordingSink::default();
        assert_eq!(
            run_shortcut(&mut source, &mut sink),
            FlowOutcome::AwaitingManualInput
        );
        assert!(sink.requests.is_empty());
    }

    #[test]
    fn lost_context_is_blocked() {
        let mut controller = new_controller();
        assert_eq!(
            controller.start(SourceSnapshot::ContextLost),
            StartOutcome::Blocked(BlockReason::ContextLost)
        );
    }
}
