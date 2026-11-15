//! Снимок выделения исходного окна и ожидание одного жеста.
//!
//! Адаптер фиксирует внешний источник до открытия окон хоста. Дальше он
//! различает готовый текст, пустой диапазон, неподдерживаемый элемент,
//! парольное поле и потерю источника. Пиксели и Win32-дескрипторы остаются
//! внутри платформенной реализации; наружу выходят только типизированные
//! состояния и координаты в физических пикселях рабочего стола.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use util_maker_core::{CallSession, PlatformHost, SourceRef};
use util_maker_sdk::{error_codes, host_methods, FrameRegion, RpcError};

/// Сколько ждать жест выделения, если вызывающий не задал своё значение.
pub const DEFAULT_SELECTION_WAIT: Duration = Duration::from_secs(25);

/// Максимальный шаг проверки отмены и смены источника во время ожидания.
const WAIT_SLICE: Duration = Duration::from_millis(250);

/// Точка в физических пикселях виртуального рабочего стола.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenPoint {
    pub x: i32,
    pub y: i32,
}

/// Внешний источник, закреплённый до открытия собственных окон.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturedSource {
    /// Непрозрачная ссылка для `CallSession::context.source_ref`.
    pub source_ref: String,
    /// Прямоугольник окна в физических пикселях рабочего стола.
    pub window: FrameRegion,
    /// Идентификатор процесса источника для проверки принадлежности элемента.
    pub process_id: u32,
}

impl CapturedSource {
    pub fn as_ref(&self) -> SourceRef {
        SourceRef(self.source_ref.clone())
    }
}

/// Состояние выделения исходного элемента.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SelectionState {
    /// Готовый текст и прямоугольник выделения в координатах рабочего стола.
    Text { text: String, bounds: FrameRegion },
    /// Текстовый элемент поддерживает выделение, но диапазон пуст.
    Empty { caret: ScreenPoint },
    /// Элемент не отдаёт текстовое выделение.
    Unsupported { bounds: FrameRegion },
    /// Известное парольное поле: текст не читался.
    Password { bounds: FrameRegion },
    /// Окно закрылось или элемент больше не доступен.
    SourceLost,
}

/// Снимок состояния источника, привязанный к исходному окну.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionSnapshot {
    pub source: CapturedSource,
    pub state: SelectionState,
}

/// Итог ожидания одного жеста в исходном приложении.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum WaitOutcome {
    Selected { text: String, bounds: FrameRegion },
    Unsupported { bounds: FrameRegion },
    Password { bounds: FrameRegion },
    Cancelled,
    SourceChanged,
    SourceLost,
    TimedOut,
}

/// Ошибка платформенного источника выделения.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SelectionError {
    #[error("выделение недоступно на этой ОС: {0}")]
    Unavailable(String),
    #[error("ошибка UI Automation: {0}")]
    Automation(String),
    #[error("исходный источник потерян")]
    SourceLost,
}

/// Что сообщает ожидание изменения в исходном приложении.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionEvent {
    /// Выделение изменилось; нужно один раз перечитать снимок.
    Changed,
    /// Пользователь переключил приложение.
    SourceChanged,
    /// Пользователь нажал Esc или вызов отменён.
    Cancelled,
    /// Исходное окно закрылось.
    SourceLost,
    /// Текущий короткий отрезок ожидания истёк.
    TimedOut,
}

/// Нативный источник, который адаптер вернул вместе с публичным описанием.
#[derive(Debug, Clone)]
pub struct NativeCapture<C> {
    pub window: FrameRegion,
    pub process_id: u32,
    pub native: C,
}

/// Реализация платформы: Windows использует UI Automation, тесты — подменный
/// источник.
pub trait SelectionBackend {
    type Capture;

    /// Захватывает внешнее окно и его фокус до показа окон хоста.
    fn capture_foreground(&mut self) -> Result<NativeCapture<Self::Capture>, SelectionError>;

    /// Читает состояние выделения. Пароль проверяется до чтения текста.
    fn read(&mut self, capture: &mut Self::Capture) -> Result<SelectionState, SelectionError>;

    /// Ждёт одно изменение, отмену, смену приложения или потерю источника.
    /// Во время простоя метод не должен наблюдать за системой.
    fn wait_for_change(
        &mut self,
        capture: &mut Self::Capture,
        cancel: &AtomicBool,
        timeout: Duration,
    ) -> Result<SelectionEvent, SelectionError>;

    /// Освобождает подписки и нативные объекты источника.
    fn release(&mut self, capture: Self::Capture);
}

/// Заглушка для Linux и macOS.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnsupportedSelection;

impl SelectionBackend for UnsupportedSelection {
    type Capture = ();

    fn capture_foreground(&mut self) -> Result<NativeCapture<Self::Capture>, SelectionError> {
        Err(SelectionError::Unavailable(
            "чтение выделения поддерживается только в Windows".to_owned(),
        ))
    }

    fn read(&mut self, _capture: &mut Self::Capture) -> Result<SelectionState, SelectionError> {
        Err(SelectionError::Unavailable(
            "чтение выделения поддерживается только в Windows".to_owned(),
        ))
    }

    fn wait_for_change(
        &mut self,
        _capture: &mut Self::Capture,
        _cancel: &AtomicBool,
        _timeout: Duration,
    ) -> Result<SelectionEvent, SelectionError> {
        Err(SelectionError::Unavailable(
            "ожидание выделения поддерживается только в Windows".to_owned(),
        ))
    }

    fn release(&mut self, _capture: Self::Capture) {}
}

struct StoredCapture<C> {
    source: CapturedSource,
    native: C,
}

/// Хранилище закреплённых источников и публичные операции адаптера.
pub struct SelectionService<B: SelectionBackend> {
    backend: B,
    captures: BTreeMap<SourceRef, StoredCapture<B::Capture>>,
    next_source: u64,
}

impl<B: SelectionBackend> SelectionService<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            captures: BTreeMap::new(),
            next_source: 0,
        }
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// Закрепляет текущее внешнее окно и возвращает его непрозрачную ссылку.
    pub fn capture_foreground(&mut self) -> Result<CapturedSource, SelectionError> {
        let native = self.backend.capture_foreground()?;
        self.next_source = self.next_source.saturating_add(1);
        let source = CapturedSource {
            source_ref: format!("selection-{}-{}", std::process::id(), self.next_source),
            window: native.window,
            process_id: native.process_id,
        };
        self.captures.insert(
            source.as_ref(),
            StoredCapture {
                source: source.clone(),
                native: native.native,
            },
        );
        Ok(source)
    }

    /// Читает состояние закреплённого источника. Ссылка остаётся привязанной
    /// к исходному окну, даже если меню трея уже открывалось.
    pub fn snapshot(&mut self, source: &SourceRef) -> Result<SelectionSnapshot, SelectionError> {
        let stored = self
            .captures
            .get_mut(source)
            .ok_or(SelectionError::SourceLost)?;
        let state = self.backend.read(&mut stored.native)?;
        Ok(SelectionSnapshot {
            source: stored.source.clone(),
            state,
        })
    }

    /// Ждёт один жест выделения по умолчанию не дольше
    /// [`DEFAULT_SELECTION_WAIT`].
    pub fn wait_for_selection(
        &mut self,
        source: &SourceRef,
        cancel: &AtomicBool,
    ) -> Result<WaitOutcome, SelectionError> {
        self.wait_for_selection_with_timeout(source, cancel, DEFAULT_SELECTION_WAIT)
    }

    /// Ждёт один жест выделения с явным сроком. После первого непустого
    /// выделения повторное чтение и наблюдение прекращаются.
    pub fn wait_for_selection_with_timeout(
        &mut self,
        source: &SourceRef,
        cancel: &AtomicBool,
        timeout: Duration,
    ) -> Result<WaitOutcome, SelectionError> {
        let deadline = Instant::now() + timeout;
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Ok(WaitOutcome::Cancelled);
            }
            let snapshot = self.snapshot(source)?;
            match snapshot.state {
                SelectionState::Text { text, bounds } => {
                    return Ok(WaitOutcome::Selected { text, bounds });
                }
                SelectionState::Unsupported { bounds } => {
                    return Ok(WaitOutcome::Unsupported { bounds });
                }
                SelectionState::Password { bounds } => {
                    return Ok(WaitOutcome::Password { bounds });
                }
                SelectionState::SourceLost => return Ok(WaitOutcome::SourceLost),
                SelectionState::Empty { .. } => {}
            }

            let now = Instant::now();
            if now >= deadline {
                return Ok(WaitOutcome::TimedOut);
            }
            let slice = (deadline - now).min(WAIT_SLICE);
            let event = {
                let stored = self
                    .captures
                    .get_mut(source)
                    .ok_or(SelectionError::SourceLost)?;
                self.backend
                    .wait_for_change(&mut stored.native, cancel, slice)?
            };
            match event {
                SelectionEvent::Changed | SelectionEvent::TimedOut => continue,
                SelectionEvent::Cancelled => return Ok(WaitOutcome::Cancelled),
                SelectionEvent::SourceChanged => return Ok(WaitOutcome::SourceChanged),
                SelectionEvent::SourceLost => return Ok(WaitOutcome::SourceLost),
            }
        }
    }

    /// Снимает подписки и забывает источник. Повторный вызов безопасен.
    pub fn release_source(&mut self, source: &SourceRef) {
        if let Some(stored) = self.captures.remove(source) {
            self.backend.release(stored.native);
        }
    }

    /// Освобождает все закреплённые источники при завершении приложения.
    pub fn release_all(&mut self) {
        let sources: Vec<_> = self.captures.keys().cloned().collect();
        for source in sources {
            self.release_source(&source);
        }
    }

    pub fn tracked_sources(&self) -> usize {
        self.captures.len()
    }
}

impl<B: SelectionBackend> PlatformHost for SelectionService<B> {
    fn selection_get(&mut self, context: &CallSession) -> Result<Value, RpcError> {
        let source = context.context.source_ref.as_ref().ok_or_else(|| {
            RpcError::new(error_codes::CONTEXT_LOST, "внешний источник не закреплён")
        })?;
        let snapshot = self.snapshot(source).map_err(selection_rpc_error)?;
        serde_json::to_value(snapshot).map_err(|error| {
            RpcError::new(
                error_codes::INTERNAL_ERROR,
                format!("сериализация выделения: {error}"),
            )
        })
    }

    fn selection_wait(&mut self, context: &CallSession, params: &Value) -> Result<Value, RpcError> {
        let source = context.context.source_ref.as_ref().ok_or_else(|| {
            RpcError::new(error_codes::CONTEXT_LOST, "внешний источник не закреплён")
        })?;
        let timeout = params
            .get("timeoutMs")
            .and_then(Value::as_u64)
            .map_or(Duration::from_secs(1), Duration::from_millis)
            .clamp(Duration::from_millis(1), DEFAULT_SELECTION_WAIT);
        let cancel = AtomicBool::new(false);
        let outcome = self
            .wait_for_selection_with_timeout(source, &cancel, timeout)
            .map_err(selection_rpc_error)?;
        serde_json::to_value(outcome).map_err(|error| {
            RpcError::new(
                error_codes::INTERNAL_ERROR,
                format!("сериализация ожидания выделения: {error}"),
            )
        })
    }

    fn buffer_copy(&mut self, _context: &CallSession, _params: &Value) -> Result<Value, RpcError> {
        Err(RpcError::new(
            error_codes::CAPABILITY_DENIED,
            format!("операция {} пока не подключена", host_methods::BUFFER_COPY),
        ))
    }
}

fn selection_rpc_error(error: SelectionError) -> RpcError {
    let code = match error {
        SelectionError::Unavailable(_) => error_codes::CAPABILITY_DENIED,
        SelectionError::Automation(_) => error_codes::INTERNAL_ERROR,
        SelectionError::SourceLost => error_codes::CONTEXT_LOST,
    };
    RpcError::new(code, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[derive(Debug, Clone)]
    struct FakeCapture {
        state: SelectionState,
    }

    struct FakeBackend {
        captures: Vec<FakeCapture>,
        initial: VecDeque<SelectionState>,
        events: VecDeque<(SelectionEvent, Option<SelectionState>)>,
        read_count: usize,
        wait_count: usize,
        released: usize,
    }

    impl FakeBackend {
        fn new(initial: impl IntoIterator<Item = SelectionState>) -> Self {
            Self {
                captures: Vec::new(),
                initial: initial.into_iter().collect(),
                events: VecDeque::new(),
                read_count: 0,
                wait_count: 0,
                released: 0,
            }
        }

        fn with_events(
            mut self,
            events: impl IntoIterator<Item = (SelectionEvent, Option<SelectionState>)>,
        ) -> Self {
            self.events = events.into_iter().collect();
            self
        }
    }

    impl SelectionBackend for FakeBackend {
        type Capture = usize;

        fn capture_foreground(&mut self) -> Result<NativeCapture<Self::Capture>, SelectionError> {
            let state = self.initial.pop_front().ok_or(SelectionError::SourceLost)?;
            self.captures.push(FakeCapture { state });
            Ok(NativeCapture {
                window: FrameRegion {
                    x: -1920,
                    y: 100,
                    width: 1920,
                    height: 1080,
                },
                process_id: 42,
                native: self.captures.len() - 1,
            })
        }

        fn read(&mut self, capture: &mut Self::Capture) -> Result<SelectionState, SelectionError> {
            self.read_count += 1;
            Ok(self
                .captures
                .get(*capture)
                .ok_or(SelectionError::SourceLost)?
                .state
                .clone())
        }

        fn wait_for_change(
            &mut self,
            capture: &mut Self::Capture,
            _cancel: &AtomicBool,
            _timeout: Duration,
        ) -> Result<SelectionEvent, SelectionError> {
            self.wait_count += 1;
            let Some((event, state)) = self.events.pop_front() else {
                return Ok(SelectionEvent::TimedOut);
            };
            if let Some(state) = state {
                self.captures
                    .get_mut(*capture)
                    .ok_or(SelectionError::SourceLost)?
                    .state = state;
            }
            Ok(event)
        }

        fn release(&mut self, _capture: Self::Capture) {
            self.released += 1;
        }
    }

    #[test]
    fn captures_source_and_reads_text_with_physical_coordinates() {
        let mut service = SelectionService::new(FakeBackend::new([SelectionState::Text {
            text: "  строка\nс переносом  ".to_owned(),
            bounds: FrameRegion {
                x: -1800,
                y: 300,
                width: 420,
                height: 90,
            },
        }]));
        let captured = service.capture_foreground().expect("источник");
        assert_eq!(captured.window.x, -1920);
        assert_eq!(captured.process_id, 42);
        let snapshot = service.snapshot(&captured.as_ref()).expect("снимок");
        match snapshot.state {
            SelectionState::Text { text, bounds } => {
                assert_eq!(text, "  строка\nс переносом  ");
                assert_eq!(bounds.x, -1800);
                assert_eq!(bounds.width, 420);
            }
            other => panic!("ожидался текст, получено {other:?}"),
        }
    }

    #[test]
    fn password_and_unsupported_are_distinct_states() {
        let mut service = SelectionService::new(FakeBackend::new([
            SelectionState::Password {
                bounds: FrameRegion {
                    x: 10,
                    y: 20,
                    width: 300,
                    height: 40,
                },
            },
            SelectionState::Unsupported {
                bounds: FrameRegion {
                    x: 40,
                    y: 50,
                    width: 200,
                    height: 60,
                },
            },
        ]));
        let password = service.capture_foreground().expect("пароль");
        assert!(matches!(
            service.snapshot(&password.as_ref()).expect("снимок").state,
            SelectionState::Password { .. }
        ));
        let unsupported = service.capture_foreground().expect("элемент");
        assert!(matches!(
            service
                .snapshot(&unsupported.as_ref())
                .expect("снимок")
                .state,
            SelectionState::Unsupported { .. }
        ));
    }

    #[test]
    fn empty_selection_waits_for_one_gesture_and_no_more() {
        let mut backend = FakeBackend::new([SelectionState::Empty {
            caret: ScreenPoint { x: 5, y: 6 },
        }]);
        backend.events = VecDeque::from([
            (
                SelectionEvent::Changed,
                Some(SelectionState::Text {
                    text: "первый жест".to_owned(),
                    bounds: FrameRegion {
                        x: 1,
                        y: 2,
                        width: 30,
                        height: 4,
                    },
                }),
            ),
            (
                SelectionEvent::Changed,
                Some(SelectionState::Text {
                    text: "поздний второй жест".to_owned(),
                    bounds: FrameRegion {
                        x: 7,
                        y: 8,
                        width: 9,
                        height: 10,
                    },
                }),
            ),
        ]);
        let mut service = SelectionService::new(backend);
        let captured = service.capture_foreground().expect("источник");
        let cancel = AtomicBool::new(false);
        let outcome = service
            .wait_for_selection_with_timeout(&captured.as_ref(), &cancel, Duration::from_secs(1))
            .expect("ожидание");
        assert_eq!(
            outcome,
            WaitOutcome::Selected {
                text: "первый жест".to_owned(),
                bounds: FrameRegion {
                    x: 1,
                    y: 2,
                    width: 30,
                    height: 4,
                },
            }
        );
        assert_eq!(service.backend().wait_count, 1);
        assert_eq!(service.backend().read_count, 2);
        assert_eq!(service.tracked_sources(), 1);
    }

    #[test]
    fn cancel_and_foreground_change_stop_the_wait() {
        let mut service = SelectionService::new(
            FakeBackend::new([SelectionState::Empty {
                caret: ScreenPoint { x: 0, y: 0 },
            }])
            .with_events([(SelectionEvent::Cancelled, None)]),
        );
        let captured = service.capture_foreground().expect("источник");
        let cancel = AtomicBool::new(false);
        assert_eq!(
            service
                .wait_for_selection_with_timeout(
                    &captured.as_ref(),
                    &cancel,
                    Duration::from_secs(1)
                )
                .expect("ожидание"),
            WaitOutcome::Cancelled
        );

        let mut service = SelectionService::new(
            FakeBackend::new([SelectionState::Empty {
                caret: ScreenPoint { x: 0, y: 0 },
            }])
            .with_events([(SelectionEvent::SourceChanged, None)]),
        );
        let captured = service.capture_foreground().expect("источник");
        assert_eq!(
            service
                .wait_for_selection_with_timeout(
                    &captured.as_ref(),
                    &cancel,
                    Duration::from_secs(1)
                )
                .expect("ожидание"),
            WaitOutcome::SourceChanged
        );
    }

    #[test]
    fn cancellation_flag_wins_before_reading_the_source() {
        let mut service = SelectionService::new(FakeBackend::new([SelectionState::Empty {
            caret: ScreenPoint { x: 0, y: 0 },
        }]));
        let captured = service.capture_foreground().expect("источник");
        let cancel = AtomicBool::new(true);
        assert_eq!(
            service
                .wait_for_selection_with_timeout(
                    &captured.as_ref(),
                    &cancel,
                    Duration::from_secs(1)
                )
                .expect("ожидание"),
            WaitOutcome::Cancelled
        );
        assert_eq!(service.backend().read_count, 0);
    }

    #[test]
    fn lost_source_is_reported_and_reference_is_released() {
        let mut service = SelectionService::new(FakeBackend::new([SelectionState::SourceLost]));
        let captured = service.capture_foreground().expect("источник");
        let cancel = AtomicBool::new(false);
        assert_eq!(
            service
                .wait_for_selection_with_timeout(
                    &captured.as_ref(),
                    &cancel,
                    Duration::from_secs(1)
                )
                .expect("ожидание"),
            WaitOutcome::SourceLost
        );
        service.release_source(&captured.as_ref());
        assert!(matches!(
            service.snapshot(&captured.as_ref()),
            Err(SelectionError::SourceLost)
        ));
        assert_eq!(service.backend().released, 1);
        assert_eq!(service.tracked_sources(), 0);
    }

    #[test]
    fn empty_wait_times_out_without_extra_reads() {
        let mut service = SelectionService::new(FakeBackend::new([SelectionState::Empty {
            caret: ScreenPoint { x: 0, y: 0 },
        }]));
        let captured = service.capture_foreground().expect("источник");
        let cancel = AtomicBool::new(false);
        assert_eq!(
            service
                .wait_for_selection_with_timeout(
                    &captured.as_ref(),
                    &cancel,
                    Duration::from_millis(5)
                )
                .expect("ожидание"),
            WaitOutcome::TimedOut
        );
        assert_eq!(service.tracked_sources(), 1);
    }

    #[test]
    fn source_token_stays_bound_after_menu_like_gap() {
        let mut service = SelectionService::new(FakeBackend::new([SelectionState::Text {
            text: "готово".to_owned(),
            bounds: FrameRegion {
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            },
        }]));
        let captured = service.capture_foreground().expect("источник");
        // Между захватом и чтением хост мог открыть меню трея и свои окна.
        let from_menu = captured.as_ref();
        let snapshot = service.snapshot(&from_menu).expect("снимок");
        assert_eq!(snapshot.source.source_ref, captured.source_ref);
        assert_eq!(snapshot.source.window, captured.window);
    }

    #[test]
    fn unsupported_selection_returns_manual_input_signal() {
        let mut service = SelectionService::new(FakeBackend::new([SelectionState::Unsupported {
            bounds: FrameRegion {
                x: 10,
                y: 20,
                width: 100,
                height: 30,
            },
        }]));
        let captured = service.capture_foreground().expect("источник");
        let cancel = AtomicBool::new(false);
        assert_eq!(
            service
                .wait_for_selection_with_timeout(
                    &captured.as_ref(),
                    &cancel,
                    Duration::from_secs(1)
                )
                .expect("ожидание"),
            WaitOutcome::Unsupported {
                bounds: FrameRegion {
                    x: 10,
                    y: 20,
                    width: 100,
                    height: 30,
                },
            }
        );
    }
}
