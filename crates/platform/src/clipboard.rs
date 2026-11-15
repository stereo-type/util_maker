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
    use windows::Win32::Foundation::{GlobalFree, HANDLE};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::System::Ole::CF_UNICODETEXT;

    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = wide.len() * std::mem::size_of::<u16>();
    let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) }
        .map_err(|error| ClipboardError::Platform(error.to_string()))?;
    let pointer = unsafe { GlobalLock(handle) };
    if pointer.is_null() {
        let _ = unsafe { GlobalFree(Some(handle)) };
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
        let _ = unsafe { GlobalFree(Some(handle)) };
        return Err(ClipboardError::Platform(opened.unwrap_err().to_string()));
    }
    let result = (|| -> Result<(), ClipboardError> {
        unsafe { EmptyClipboard() }.map_err(|error| ClipboardError::Platform(error.to_string()))?;
        unsafe { SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(handle.0))) }
            .map_err(|error| ClipboardError::Platform(error.to_string()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = unsafe { GlobalFree(Some(handle)) };
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

/// Записывает RGBA8-изображение как CF_DIB (32 бита, BGRA, снизу вверх).
#[cfg(windows)]
pub fn write_image(width: u32, height: u32, rgba: &[u8]) -> Result<(), ClipboardError> {
    use windows::Win32::System::Ole::CF_DIB;
    if width == 0 || height == 0 || rgba.len() != width as usize * height as usize * 4 {
        return Err(ClipboardError::Platform(
            "некорректный размер RGBA-изображения".to_owned(),
        ));
    }
    let mut dib = Vec::with_capacity(40 + rgba.len());
    dib.extend_from_slice(&40u32.to_le_bytes());
    dib.extend_from_slice(&(width as i32).to_le_bytes());
    dib.extend_from_slice(&(height as i32).to_le_bytes());
    dib.extend_from_slice(&1u16.to_le_bytes());
    dib.extend_from_slice(&32u16.to_le_bytes());
    dib.extend_from_slice(&0u32.to_le_bytes());
    dib.extend_from_slice(&(rgba.len() as u32).to_le_bytes());
    for _ in 0..4 {
        dib.extend_from_slice(&0i32.to_le_bytes());
    }
    for row in (0..height as usize).rev() {
        for column in 0..width as usize {
            let index = (row * width as usize + column) * 4;
            dib.extend_from_slice(&[
                rgba[index + 2],
                rgba[index + 1],
                rgba[index],
                rgba[index + 3],
            ]);
        }
    }
    write_clipboard(u32::from(CF_DIB.0), &dib)
}

#[cfg(not(windows))]
pub fn write_image(_width: u32, _height: u32, _rgba: &[u8]) -> Result<(), ClipboardError> {
    Err(ClipboardError::Unavailable)
}

#[cfg(windows)]
fn write_clipboard(format: u32, bytes: &[u8]) -> Result<(), ClipboardError> {
    use std::ptr;
    use windows::Win32::Foundation::{GlobalFree, HANDLE};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) }
        .map_err(|error| ClipboardError::Platform(error.to_string()))?;
    let pointer = unsafe { GlobalLock(handle) };
    if pointer.is_null() {
        let _ = unsafe { GlobalFree(Some(handle)) };
        return Err(ClipboardError::Platform(
            "не удалось заблокировать буфер".to_owned(),
        ));
    }
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), pointer.cast::<u8>(), bytes.len());
        let _ = GlobalUnlock(handle);
    }
    if unsafe { OpenClipboard(None) }.is_err() {
        let _ = unsafe { GlobalFree(Some(handle)) };
        return Err(ClipboardError::Platform("буфер занят".to_owned()));
    }
    let result = (|| -> Result<(), ClipboardError> {
        unsafe { EmptyClipboard() }.map_err(|error| ClipboardError::Platform(error.to_string()))?;
        unsafe { SetClipboardData(format, Some(HANDLE(handle.0))) }
            .map_err(|error| ClipboardError::Platform(error.to_string()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = unsafe { GlobalFree(Some(handle)) };
    }
    let _ = unsafe { CloseClipboard() };
    result
}
