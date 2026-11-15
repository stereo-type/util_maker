//! Запись изображения и текста в системный буфер обмена.
//!
//! Windows использует `CF_DIB`/`CF_UNICODETEXT`; ошибка занятого буфера
//! возвращается повторяемо, чтобы вызывающая сторона могла повторить команду.

use std::fmt;

#[derive(Clone, PartialEq, Eq)]
pub enum ClipboardContent {
    Text(String),
    /// RGBA8, построчно сверху вниз.
    Image {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
}

impl fmt::Debug for ClipboardContent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(text) => formatter
                .debug_struct("Text")
                .field("chars", &text.chars().count())
                .finish(),
            Self::Image {
                width,
                height,
                rgba,
            } => formatter
                .debug_struct("Image")
                .field("width", width)
                .field("height", height)
                .field("bytes", &rgba.len())
                .finish(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClipboardError {
    #[error("буфер обмена занят другой программой")]
    Busy,
    #[error("буфер обмена недоступен: {0}")]
    Unavailable(String),
    #[error("некорректное изображение: {0}")]
    InvalidImage(String),
}

pub trait ClipboardWriter {
    fn write(&mut self, content: &ClipboardContent) -> Result<(), ClipboardError>;
}

/// Системный буфер текущей ОС.
pub struct SystemClipboard;

impl SystemClipboard {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SystemClipboard {
    fn default() -> Self {
        Self::new()
    }
}

impl ClipboardWriter for SystemClipboard {
    fn write(&mut self, content: &ClipboardContent) -> Result<(), ClipboardError> {
        write_native(content)
    }
}

#[allow(dead_code)]
pub(crate) fn utf16_bytes(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// DIB 32 бита на пиксель, строки снизу вверх, BGRA.
#[allow(dead_code)]
pub(crate) fn dib_bytes(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, ClipboardError> {
    let expected = width as usize * height as usize * 4;
    if width == 0 || height == 0 || rgba.len() != expected {
        return Err(ClipboardError::InvalidImage(format!(
            "ожидалось {expected} байт RGBA"
        )));
    }
    let pixels = width as usize * height as usize * 4;
    let mut output = Vec::with_capacity(40 + pixels);
    output.extend_from_slice(&40u32.to_le_bytes());
    output.extend_from_slice(&(width as i32).to_le_bytes());
    output.extend_from_slice(&(height as i32).to_le_bytes());
    output.extend_from_slice(&1u16.to_le_bytes());
    output.extend_from_slice(&32u16.to_le_bytes());
    output.extend_from_slice(&0u32.to_le_bytes());
    output.extend_from_slice(&(pixels as u32).to_le_bytes());
    for _ in 0..4 {
        output.extend_from_slice(&0i32.to_le_bytes());
    }
    for row in (0..height as usize).rev() {
        for column in 0..width as usize {
            let index = (row * width as usize + column) * 4;
            output.extend_from_slice(&[
                rgba[index + 2],
                rgba[index + 1],
                rgba[index],
                rgba[index + 3],
            ]);
        }
    }
    Ok(output)
}

#[cfg(windows)]
fn write_native(content: &ClipboardContent) -> Result<(), ClipboardError> {
    use windows::Win32::Foundation::{HANDLE, HGLOBAL};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

    const CF_DIB: u32 = 8;
    const CF_UNICODETEXT: u32 = 13;

    let (format, bytes) = match content {
        ClipboardContent::Text(text) => (CF_UNICODETEXT, utf16_bytes(text)),
        ClipboardContent::Image {
            width,
            height,
            rgba,
        } => (CF_DIB, dib_bytes(*width, *height, rgba)?),
    };

    unsafe {
        OpenClipboard(None).map_err(|_| ClipboardError::Busy)?;
        let result = (|| {
            EmptyClipboard().map_err(|error| ClipboardError::Unavailable(error.to_string()))?;
            let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len())
                .map_err(|error| ClipboardError::Unavailable(error.to_string()))?;
            let pointer = GlobalLock(HGLOBAL(memory.0));
            if pointer.is_null() {
                return Err(ClipboardError::Unavailable(
                    "не удалось заблокировать буфер".to_owned(),
                ));
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer.cast::<u8>(), bytes.len());
            let _ = GlobalUnlock(HGLOBAL(memory.0));
            SetClipboardData(format, Some(HANDLE(memory.0)))
                .map_err(|error| ClipboardError::Unavailable(error.to_string()))?;
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}

#[cfg(not(windows))]
fn write_native(_content: &ClipboardContent) -> Result<(), ClipboardError> {
    Err(ClipboardError::Unavailable(
        "системный буфер доступен только в Windows".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_utf16_with_terminator() {
        let bytes = utf16_bytes("Привет\nмир");
        assert_eq!(bytes.len() % 2, 0);
        assert_eq!(&bytes[bytes.len() - 2..], &[0, 0]);
        let value: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take_while(|unit| *unit != 0)
            .collect();
        assert_eq!(String::from_utf16(&value).unwrap(), "Привет\nмир");
    }

    #[test]
    fn dib_is_bottom_up_bgra_with_header() {
        let rgba = vec![
            255, 0, 0, 255, // верхний левый — красный
            0, 255, 0, 255, // верхний правый — зелёный
            0, 0, 255, 255, // нижний левый — синий
            255, 255, 0, 255, // нижний правый — жёлтый
        ];
        let dib = dib_bytes(2, 2, &rgba).unwrap();
        assert_eq!(dib.len(), 40 + 16);
        assert_eq!(&dib[..4], &40u32.to_le_bytes());
        // Первая строка DIB — нижняя строка исходного изображения.
        assert_eq!(&dib[40..44], &[255, 0, 0, 255]);
        assert_eq!(&dib[44..48], &[0, 255, 255, 255]);
    }

    #[test]
    fn invalid_image_is_rejected() {
        assert!(matches!(
            dib_bytes(2, 2, &[0; 3]),
            Err(ClipboardError::InvalidImage(_))
        ));
    }

    #[cfg(not(windows))]
    #[test]
    fn native_clipboard_is_unavailable_on_linux() {
        let mut clipboard = SystemClipboard::new();
        assert!(matches!(
            clipboard.write(&ClipboardContent::Text("x".to_owned())),
            Err(ClipboardError::Unavailable(_))
        ));
    }
}
