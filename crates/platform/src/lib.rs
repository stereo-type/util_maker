//! Платформенный адаптер глобальных сочетаний.
//!
//! Windows-реализация регистрирует сочетания через `RegisterHotKey` в
//! отдельном потоке с message-only окном и доставляет `WM_HOTKEY` в канал.
//! На других ОС адаптер сообщает, что операция недоступна, — это позволяет
//! собирать и тестировать переносимое ядро на Linux.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use util_maker_core::hotkeys::{system_function, Hotkey};

pub mod autostart;
pub mod capture;

pub use capture::{ScreenService, ScreenSource, ScreenSourceError};

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
pub type NativeScreenService = ScreenService<native_capture::WindowsScreen>;
#[cfg(not(windows))]
pub type NativeScreenService = ScreenService<NativeScreen>;

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
