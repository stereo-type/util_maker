//! Запись текста в системный буфер обмена.
//!
//! Вызывается только по явной команде пользователя. На Linux прототип не
//! подключает графическую сессию и возвращает понятную ошибку, поэтому
//! автоматические проверки не меняют общий буфер агента.

/// Ошибка записи в буфер.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClipboardError {
    #[error("буфер обмена недоступен на этой ОС")]
    Unavailable,
    #[error("буфер обмена: {0}")]
    Platform(String),
}

#[cfg(windows)]
pub fn write_text(text: &str) -> Result<(), ClipboardError> {
    use std::ptr;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalFree, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::System::Ole::CF_UNICODETEXT;

    let mut wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = wide.len() * std::mem::size_of::<u16>();
    let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) }
        .map_err(|error| ClipboardError::Platform(error.to_string()))?;
    let pointer = unsafe { GlobalLock(handle) };
    if pointer.is_null() {
        unsafe { GlobalFree(handle) };
        return Err(ClipboardError::Platform(
            "не удалось заблокировать буфер".to_owned(),
        ));
    }
    unsafe {
        ptr::copy_nonoverlapping(wide.as_ptr().cast::<u8>(), pointer.cast::<u8>(), bytes);
        let _ = GlobalUnlock(handle);
    }

    let opened = unsafe { OpenClipboard(None) };
    if opened.is_err() {
        unsafe { GlobalFree(handle) };
        return Err(ClipboardError::Platform(opened.unwrap_err().to_string()));
    }
    let result = (|| -> Result<(), ClipboardError> {
        unsafe { EmptyClipboard() }
            .map_err(|error| ClipboardError::Platform(error.to_string()))?;
        unsafe { SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(handle.0))) }
            .map_err(|error| ClipboardError::Platform(error.to_string()))?;
        Ok(())
    })();
    if result.is_err() {
        unsafe { GlobalFree(handle) };
    }
    let closed = unsafe { CloseClipboard() };
    if let Err(error) = closed {
        return Err(ClipboardError::Platform(error.to_string()));
    }
    result
}

#[cfg(not(windows))]
pub fn write_text(_text: &str) -> Result<(), ClipboardError> {
    Err(ClipboardError::Unavailable)
}
