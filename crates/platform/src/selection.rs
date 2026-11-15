//! Чтение выделения исходного окна через Windows UI Automation.
//!
//! Адаптер возвращает типизированное состояние до открытия окон хоста.
//! Парольное поле проверяется до чтения текста; пустой диапазон не отправляет
//! запрос, а ждёт один жест пользователя.

use std::time::{Duration, Instant};

/// Снимок источника в момент вызова.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionSnapshot {
    Text(String),
    Empty,
    Unsupported,
    Password,
    ContextLost,
}

/// Порт чтения выделения. Хост и плагин не знают про Win32.
pub trait SelectionSource {
    fn snapshot(&mut self) -> SelectionSnapshot;

    /// Ждёт появления непустого выделения после жеста. `None` — отмена/потеря.
    fn wait_for_selection(&mut self, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match self.snapshot() {
                SelectionSnapshot::Text(text) => return Some(text),
                SelectionSnapshot::Password | SelectionSnapshot::ContextLost => return None,
                SelectionSnapshot::Empty | SelectionSnapshot::Unsupported => {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
        None
    }
}

/// Неподдерживаемая платформа: явный отказ вместо пустого текста.
pub struct UnsupportedSelection;

impl SelectionSource for UnsupportedSelection {
    fn snapshot(&mut self) -> SelectionSnapshot {
        SelectionSnapshot::Unsupported
    }
}

#[cfg(windows)]
mod native {
    use super::{SelectionSnapshot, SelectionSource};
    use windows::core::BSTR;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    };
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationTextPattern, UIA_TextPatternId,
    };

    /// UI Automation-адаптер. COM инициализируется лениво в текущем потоке.
    pub struct WindowsSelectionSource {
        automation: IUIAutomation,
    }

    impl WindowsSelectionSource {
        pub fn new() -> Result<Self, String> {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                let automation: IUIAutomation =
                    CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
                        .map_err(|error| format!("UI Automation: {error}"))?;
                Ok(Self { automation })
            }
        }
    }

    impl SelectionSource for WindowsSelectionSource {
        fn snapshot(&mut self) -> SelectionSnapshot {
            unsafe {
                let element = match self.automation.GetFocusedElement() {
                    Ok(element) => element,
                    Err(_) => return SelectionSnapshot::ContextLost,
                };
                if element
                    .CurrentIsPassword()
                    .map(|value| value.as_bool())
                    .unwrap_or(false)
                {
                    return SelectionSnapshot::Password;
                }
                let pattern: IUIAutomationTextPattern =
                    match element.GetCurrentPatternAs(UIA_TextPatternId) {
                        Ok(pattern) => pattern,
                        Err(_) => return SelectionSnapshot::Unsupported,
                    };
                let ranges = match pattern.GetSelection() {
                    Ok(ranges) => ranges,
                    Err(_) => return SelectionSnapshot::ContextLost,
                };
                match ranges.Length() {
                    Ok(0) => SelectionSnapshot::Empty,
                    Ok(_) => match ranges.GetElement(0) {
                        Ok(range) => match range.GetText(-1) {
                            Ok(text) => text_to_snapshot(text),
                            Err(_) => SelectionSnapshot::ContextLost,
                        },
                        Err(_) => SelectionSnapshot::ContextLost,
                    },
                    Err(_) => SelectionSnapshot::ContextLost,
                }
            }
        }
    }

    fn text_to_snapshot(text: BSTR) -> SelectionSnapshot {
        let value = text.to_string();
        if value.is_empty() {
            SelectionSnapshot::Empty
        } else {
            SelectionSnapshot::Text(value)
        }
    }
}

#[cfg(windows)]
pub use native::WindowsSelectionSource;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_source_is_explicit() {
        let mut source = UnsupportedSelection;
        assert_eq!(source.snapshot(), SelectionSnapshot::Unsupported);
        assert_eq!(source.wait_for_selection(Duration::from_millis(10)), None);
    }

    #[test]
    fn text_detection_distinguishes_empty() {
        assert_eq!(
            SelectionSnapshot::Text("x".to_owned()).clone(),
            SelectionSnapshot::Text("x".to_owned())
        );
        assert_ne!(
            SelectionSnapshot::Empty,
            SelectionSnapshot::Text("x".to_owned())
        );
    }
}
