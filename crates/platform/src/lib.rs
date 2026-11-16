//! Платформенный адаптер глобальных сочетаний.
//!
//! Windows-реализация регистрирует сочетания через `RegisterHotKey` в
//! отдельном потоке с message-only окном и доставляет `WM_HOTKEY` в канал.
//! На других ОС адаптер сообщает, что операция недоступна, — это позволяет
//! собирать и тестировать переносимое ядро на Linux.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use util_maker_core::hotkeys::{system_function, Hotkey};
use util_maker_core::{CallSession, PlatformHost};
use util_maker_sdk::{
    error_codes, FrameOrigin, FrameRef, FrameRequest, RpcError, ScreenGeometry, FRAME_FORMAT_BGRA8,
};

pub mod autostart;
pub mod capture;
pub mod selection;

pub use capture::{ScreenService, ScreenSource, ScreenSourceError};
pub use clipboard::ClipboardError;
pub use selection::{
    CapturedSource, NativeCapture, ScreenPoint, SelectionBackend, SelectionError, SelectionEvent,
    SelectionService, SelectionSnapshot, SelectionState, WaitOutcome, DEFAULT_SELECTION_WAIT,
};
pub use settings::{SecretStore, SettingsError, SettingsStore, StoredSettings};

/// Отказ регистрации, различимый интерфейсом.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterFailure {
    /// Сочетание зарезервировано Windows; функция известна до регистрации.
    SystemReserved { function: String },
    /// Сочетание занято другой программой.
    Occupied,
    /// Иная причина: ошибка Win32 или платформа без поддержки.
    Unavailable(String),
}

/// Событие нажатия сочетания. Автоповтор при удержании не доставляется.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotkeyEvent {
    pub id: u32,
    pub pressed: bool,
}

#[cfg(windows)]
#[path = "windows.rs"]
mod imp;
#[cfg(not(windows))]
#[path = "stub.rs"]
mod imp;

#[cfg(windows)]
#[path = "windows_capture.rs"]
mod native_capture;
#[cfg(not(windows))]
use capture::UnsupportedScreen as NativeScreen;

#[cfg(windows)]
#[path = "windows_selection.rs"]
mod native_selection;

pub mod clipboard;
pub mod settings;

/// Нативная реализация чтения выделения текущей ОС.
#[cfg(windows)]
pub use native_selection::WindowsSelection as NativeSelectionBackend;
#[cfg(not(windows))]
pub use selection::UnsupportedSelection as NativeSelectionBackend;

pub type NativeSelection = SelectionService<NativeSelectionBackend>;

#[cfg(windows)]
pub type NativeScreenService = ScreenService<native_capture::WindowsScreen>;
#[cfg(not(windows))]
pub type NativeScreenService = ScreenService<NativeScreen>;

/// Создаёт службу чтения выделения с нативным источником текущей ОС.
pub fn selection_service() -> NativeSelection {
    SelectionService::new(NativeSelectionBackend)
}

/// Создаёт службу захвата с нативным источником текущей ОС.
pub fn screen_service() -> NativeScreenService {
    #[cfg(windows)]
    {
        ScreenService::new(native_capture::WindowsScreen)
    }
    #[cfg(not(windows))]
    {
        ScreenService::new(NativeScreen)
    }
}

/// Снимок рабочего стола, который хост сделал до показа окна выбора области.
/// Вызов `frame.capture` получает его вместо нового захвата живого экрана:
/// пользователь видит и декодирует ровно тот кадр, где выбирал область.
#[derive(Debug, Clone)]
pub struct FrozenFrame {
    pub geometry: ScreenGeometry,
    pub origin: FrameOrigin,
    pub pixels: Vec<u8>,
    pub stride: u32,
}

/// Платформенный хост для одного вызова: кадры и закреплённые источники
/// выделения живут в общем хранилище приложения и освобождаются диспетчером.
pub struct NativeHost {
    screen: NativeScreenService,
    selection: Arc<Mutex<NativeSelection>>,
    settings: Arc<SettingsStore>,
    frozen: Option<FrozenFrame>,
    frozen_buffers: BTreeMap<String, PathBuf>,
    next_frozen: u64,
}

impl NativeHost {
    pub fn new(selection: Arc<Mutex<NativeSelection>>, settings: Arc<SettingsStore>) -> Self {
        Self {
            screen: screen_service(),
            selection,
            settings,
            frozen: None,
            frozen_buffers: BTreeMap::new(),
            next_frozen: 0,
        }
    }

    /// Подменяет живой захват заранее сделанным снимком. Используется
    /// действиями, которые сначала показывают кадр для выбора области.
    pub fn with_frozen_frame(mut self, frozen: FrozenFrame) -> Self {
        self.frozen = Some(frozen);
        self
    }

    pub fn selection(&self) -> &Arc<Mutex<NativeSelection>> {
        &self.selection
    }

    pub fn settings(&self) -> &Arc<SettingsStore> {
        &self.settings
    }

    fn capture_frozen(
        &mut self,
        context: &CallSession,
        request: &FrameRequest,
    ) -> Result<FrameRef, RpcError> {
        let frozen = self
            .frozen
            .as_ref()
            .ok_or_else(|| RpcError::new(error_codes::INTERNAL_ERROR, "снимок не подготовлен"))?;
        let rect = request.region.unwrap_or(frozen.geometry.virtual_screen);
        if rect.width == 0 || rect.height == 0 {
            return Err(RpcError::new(
                error_codes::INVALID_PARAMS,
                "область снимка пуста",
            ));
        }
        let local_x = rect.x - frozen.origin.x;
        let local_y = rect.y - frozen.origin.y;
        if local_x < 0
            || local_y < 0
            || local_x as u32 + rect.width > frozen.geometry.virtual_screen.width
            || local_y as u32 + rect.height > frozen.geometry.virtual_screen.height
        {
            return Err(RpcError::new(
                error_codes::INVALID_PARAMS,
                "область выходит за пределы снимка",
            ));
        }
        let source_stride = frozen.stride as usize;
        let stride = rect.width.checked_mul(4).ok_or_else(|| {
            RpcError::new(error_codes::INVALID_PARAMS, "ширина снимка слишком велика")
        })?;
        let mut pixels =
            Vec::with_capacity(stride as usize * usize::try_from(rect.height).unwrap_or(0));
        for row in 0..rect.height {
            let start = (local_y as u32 + row) as usize * source_stride + local_x as usize * 4;
            let end = start + stride as usize;
            let Some(slice) = frozen.pixels.get(start..end) else {
                return Err(RpcError::new(
                    error_codes::INTERNAL_ERROR,
                    "снимок короче заявленной геометрии",
                ));
            };
            pixels.extend_from_slice(slice);
        }

        self.next_frozen = self.next_frozen.saturating_add(1);
        let buffer_id = format!("{}-frozen-{}", context.call_id, self.next_frozen);
        let safe: String = buffer_id
            .chars()
            .map(|value| {
                if value.is_ascii_alphanumeric() || value == '-' || value == '_' {
                    value
                } else {
                    '_'
                }
            })
            .collect();
        let path = std::env::temp_dir().join(format!("util-maker-frozen-{safe}.bgra"));
        std::fs::write(&path, &pixels).map_err(|error| {
            RpcError::new(error_codes::INTERNAL_ERROR, format!("файл снимка: {error}"))
        })?;
        self.frozen_buffers.insert(buffer_id.clone(), path.clone());
        Ok(FrameRef {
            buffer_id,
            format: FRAME_FORMAT_BGRA8.to_owned(),
            width: rect.width,
            height: rect.height,
            stride,
            origin: FrameOrigin {
                x: rect.x,
                y: rect.y,
            },
            scale_factor: 1.0,
            expires_in_ms: 30_000,
            geometry: Some(frozen.geometry.clone()),
            pixel_path: Some(path.to_string_lossy().into_owned()),
        })
    }
}

impl Drop for NativeHost {
    fn drop(&mut self) {
        for path in self.frozen_buffers.values() {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl PlatformHost for NativeHost {
    fn capture_frame(
        &mut self,
        context: &CallSession,
        request: &FrameRequest,
    ) -> Result<FrameRef, RpcError> {
        if self.frozen.is_some() {
            return self.capture_frozen(context, request);
        }
        self.screen.capture_frame(context, request)
    }

    fn release_frame(&mut self, context: &CallSession, buffer_id: &str) -> Result<(), RpcError> {
        if let Some(path) = self.frozen_buffers.remove(buffer_id) {
            let _ = std::fs::remove_file(path);
            return Ok(());
        }
        self.screen.release_frame(context, buffer_id)
    }

    fn selection_get(&mut self, context: &CallSession) -> Result<serde_json::Value, RpcError> {
        let mut selection = self.selection.lock().map_err(|_| {
            RpcError::new(
                util_maker_sdk::error_codes::INTERNAL_ERROR,
                "хранилище источников выделения повреждено",
            )
        })?;
        selection.selection_get(context)
    }

    fn selection_wait(
        &mut self,
        context: &CallSession,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        let mut selection = self.selection.lock().map_err(|_| {
            RpcError::new(
                util_maker_sdk::error_codes::INTERNAL_ERROR,
                "хранилище источников выделения повреждено",
            )
        })?;
        selection.selection_wait(context, params)
    }

    fn settings_read(
        &mut self,
        context: &CallSession,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        let profile = params.get("profile").and_then(serde_json::Value::as_str);
        self.settings
            .read(&context.module_id, profile)
            .map_err(settings_rpc_error)
    }

    fn buffer_copy(
        &mut self,
        _context: &CallSession,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        let Some(text) = params.get("text").and_then(serde_json::Value::as_str) else {
            return Err(RpcError::new(
                util_maker_sdk::error_codes::INVALID_PARAMS,
                "не передан текст для буфера обмена",
            ));
        };
        clipboard::write_text(text).map_err(|error| {
            RpcError::new(
                util_maker_sdk::error_codes::CAPABILITY_DENIED,
                error.to_string(),
            )
        })?;
        Ok(serde_json::json!({
            "copied": true,
            "bytes": text.len(),
        }))
    }

    fn secret_read(
        &mut self,
        context: &CallSession,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        let Some(key) = params.get("key").and_then(serde_json::Value::as_str) else {
            return Err(RpcError::new(
                util_maker_sdk::error_codes::INVALID_PARAMS,
                "не передана ссылка на секрет",
            ));
        };
        let value = self
            .settings
            .read_secret(&context.module_id, key)
            .map_err(settings_rpc_error)?;
        Ok(value.map_or(serde_json::Value::Null, serde_json::Value::String))
    }
}

fn settings_rpc_error(error: settings::SettingsError) -> RpcError {
    RpcError::new(
        util_maker_sdk::error_codes::INTERNAL_ERROR,
        error.to_string(),
    )
}

/// Служба глобальных сочетаний.
pub struct HotkeyService {
    inner: imp::Service,
}

impl HotkeyService {
    /// Создаёт службу и её поток сообщений.
    pub fn start() -> Result<Self, String> {
        Ok(Self {
            inner: imp::Service::start()?,
        })
    }

    /// Регистрирует сочетание с заданным числовым идентификатором.
    pub fn register(&self, combo: &str, id: u32) -> Result<(), RegisterFailure> {
        let hotkey = Hotkey::parse(combo)
            .map_err(|error| RegisterFailure::Unavailable(error.to_string()))?;
        if let Some(function) = system_function(&hotkey) {
            return Err(RegisterFailure::SystemReserved { function });
        }
        self.inner.register(&hotkey, id)
    }

    /// Освобождает регистрацию. Повторный вызов безопасен.
    pub fn unregister(&self, id: u32) {
        self.inner.unregister(id);
    }

    /// Ждёт событие сочетания не дольше указанного времени.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<HotkeyEvent, RecvTimeoutError> {
        self.inner.recv_timeout(timeout)
    }

    /// Забирает приёмник событий, чтобы слушатель не удерживал службу.
    pub fn take_events(&mut self) -> Receiver<HotkeyEvent> {
        self.inner.take_events()
    }
}

/// Виртуальный код клавиши Windows для нормализованного сочетания.
pub fn virtual_key(hotkey: &Hotkey) -> Option<u32> {
    let named = match hotkey.key.as_str() {
        "Escape" => Some(0x1B),
        "Tab" => Some(0x09),
        "Space" => Some(0x20),
        "Enter" => Some(0x0D),
        "Backspace" => Some(0x08),
        "Delete" => Some(0x2E),
        "Insert" => Some(0x2D),
        "Home" => Some(0x24),
        "End" => Some(0x23),
        "PageUp" => Some(0x21),
        "PageDown" => Some(0x22),
        "ArrowUp" => Some(0x26),
        "ArrowDown" => Some(0x28),
        "ArrowLeft" => Some(0x25),
        "ArrowRight" => Some(0x27),
        "PrintScreen" => Some(0x2C),
        "Pause" => Some(0x13),
        "CapsLock" => Some(0x14),
        "NumLock" => Some(0x90),
        "ScrollLock" => Some(0x91),
        _ => None,
    };
    if named.is_some() {
        return named;
    }
    if hotkey.key.len() == 1 {
        let character = hotkey.key.chars().next()?;
        if character.is_ascii_uppercase() || character.is_ascii_digit() {
            return Some(character as u32);
        }
    }
    if let Some(number) = hotkey.key.strip_prefix('F') {
        if let Ok(number) = number.parse::<u32>() {
            if (1..=24).contains(&number) {
                return Some(0x70 + number - 1);
            }
        }
    }
    None
}

/// Приёмник событий, который на неподдерживаемой ОС никогда не срабатывает.
#[cfg(not(windows))]
pub(crate) fn dormant_events() -> std::sync::mpsc::Receiver<HotkeyEvent> {
    let (_sender, receiver) = std::sync::mpsc::channel();
    receiver
}

/// Позиция курсора в физических пикселях виртуального рабочего стола.
#[cfg(windows)]
pub fn cursor_position() -> Option<(i32, i32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut point = POINT::default();
    unsafe { GetCursorPos(&mut point).ok()? };
    Some((point.x, point.y))
}

#[cfg(not(windows))]
pub fn cursor_position() -> Option<(i32, i32)> {
    None
}

/// Поднимает окно поверх остальных и на передний план.
#[cfg(windows)]
pub fn raise_window(handle: isize) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetForegroundWindow, SetWindowPos, HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    };
    let hwnd = HWND(handle as *mut std::ffi::c_void);
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
        );
        let _ = SetForegroundWindow(hwnd);
    }
}

#[cfg(not(windows))]
pub fn raise_window(_handle: isize) {}

/// Поднимает окно на передний план в Windows. На других ОС — no-op.
#[cfg(windows)]
pub fn focus_window(handle: isize) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, SetForegroundWindow, ShowWindow, SW_SHOW,
    };

    let hwnd = HWND(handle as *mut core::ffi::c_void);
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
    }
}

/// Поднимает окно на передний план в Windows. На других ОС — no-op.
#[cfg(not(windows))]
pub fn focus_window(_handle: isize) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(combo: &str) -> Hotkey {
        Hotkey::parse(combo).expect("сочетание")
    }

    #[test]
    fn maps_letters_digits_and_function_keys() {
        assert_eq!(virtual_key(&key("Ctrl+Shift+U")), Some(0x55));
        assert_eq!(virtual_key(&key("Alt+1")), Some(0x31));
        assert_eq!(virtual_key(&key("F12")), Some(0x7B));
        assert_eq!(virtual_key(&key("Ctrl+Alt+Delete")), Some(0x2E));
        assert_eq!(virtual_key(&key("Win+L")), Some(0x4C));
        assert_eq!(virtual_key(&key("Shift+Space")), Some(0x20));
    }

    #[test]
    fn frozen_frame_is_cropped_served_and_released() {
        use util_maker_core::{CallSession, InvocationContext};
        use util_maker_sdk::{FrameRegion, InvocationSource, MonitorGeometry, ScreenGeometry};

        let root =
            std::env::temp_dir().join(format!("util-maker-frozen-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("каталог настроек");
        let settings = Arc::new(SettingsStore::new(&root));
        let selection = Arc::new(Mutex::new(selection_service()));
        let geometry = ScreenGeometry {
            virtual_screen: FrameRegion {
                x: -10,
                y: 5,
                width: 4,
                height: 3,
            },
            monitors: vec![MonitorGeometry {
                rect: FrameRegion {
                    x: -10,
                    y: 5,
                    width: 4,
                    height: 3,
                },
                scale_factor: 1.0,
                primary: true,
            }],
        };
        let pixels: Vec<u8> = (0..4 * 3 * 4).map(|value| (value % 251) as u8).collect();
        let mut host = NativeHost::new(selection, settings).with_frozen_frame(FrozenFrame {
            geometry: geometry.clone(),
            origin: FrameOrigin { x: -10, y: 5 },
            pixels: pixels.clone(),
            stride: 16,
        });
        let session = CallSession::new(
            "call-1",
            "org.test",
            "read",
            InvocationContext::new(InvocationSource::Hotkey),
        );
        let frame = host
            .capture_frame(
                &session,
                &FrameRequest {
                    monitor: None,
                    region: Some(FrameRegion {
                        x: -9,
                        y: 6,
                        width: 2,
                        height: 2,
                    }),
                },
            )
            .expect("кадр из снимка");
        assert_eq!(frame.width, 2);
        assert_eq!(frame.height, 2);
        assert_eq!(frame.stride, 8);
        assert_eq!(frame.origin, FrameOrigin { x: -9, y: 6 });
        assert_eq!(frame.geometry.as_ref(), Some(&geometry));
        let path = PathBuf::from(frame.pixel_path.clone().expect("файл кадра"));
        let bytes = std::fs::read(&path).expect("пиксели кадра");
        assert_eq!(bytes.len(), 2 * 2 * 4);
        // Второй пиксель второй строки снимка: смещение внутри кадра.
        let row = 1usize;
        let column = 1usize;
        let source = row * 16 + column * 4;
        assert_eq!(&bytes[0..4], &pixels[source..source + 4]);
        host.release_frame(&session, &frame.buffer_id)
            .expect("освобождение");
        assert!(!path.exists(), "файл кадра удалён после release");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn system_reservation_is_reported_without_platform_call() {
        let service = HotkeyService::start().expect("служба");
        let error = service.register("Win+L", 1).expect_err("Win+L");
        assert!(matches!(
            error,
            RegisterFailure::SystemReserved { function } if function == "блокировка компьютера"
        ));
        let error = service.register("Alt+Tab", 2).expect_err("Alt+Tab");
        assert!(matches!(
            error,
            RegisterFailure::SystemReserved { function } if function == "переключение между окнами"
        ));
    }
}
