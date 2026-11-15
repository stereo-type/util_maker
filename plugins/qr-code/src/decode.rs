//! Локальное чтение QR из выбранного прямоугольника кадра BGRA8.
//!
//! Модуль не открывает ссылки, не выполняет команды и не пишет в буфер
//! обмена. Он только возвращает текст и категорию исхода, которую окно
//! предпросмотра показывает пользователю.

use rqrr::{DeQRError, PreparedImage};
use util_maker_sdk::{FrameOrigin, FrameRef, FRAME_FORMAT_BGRA8};

/// Размер одного пикселя BGRA8 в байтах.
pub const BYTES_PER_PIXEL: usize = 4;

/// Прямоугольник выбора в физических пикселях виртуального рабочего стола.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Selection {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Selection {
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// Ошибка подготовки кадра или выделения.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("формат кадра {actual} не поддерживается, ожидается BGRA8")]
    UnsupportedFormat { actual: String },
    #[error("размер кадра должен быть больше нуля")]
    EmptyFrame,
    #[error("stride {stride} меньше минимального размера строки {minimum}")]
    InvalidStride { stride: usize, minimum: usize },
    #[error("буфер кадра неполный: нужно {needed} байт, получено {actual}")]
    IncompleteFrame { needed: usize, actual: usize },
    #[error("выделение пустое")]
    EmptySelection,
    #[error("выделение выходит за пределы кадра")]
    OutsideFrame,
}

/// Ошибка серого изображения перед поиском QR.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImageError {
    #[error("размер изображения должен быть больше нуля")]
    EmptyImage,
    #[error("буфер изображения неполный: нужно {needed} байт, получено {actual}")]
    IncompleteImage { needed: usize, actual: usize },
}

/// Кадр BGRA8 с началом в физических координатах рабочего стола.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QrFrame<'a> {
    pixels: &'a [u8],
    width: u32,
    height: u32,
    stride: usize,
    origin: FrameOrigin,
}

impl<'a> QrFrame<'a> {
    /// Проверяет размеры и `stride` кадра.
    pub fn new(
        pixels: &'a [u8],
        width: u32,
        height: u32,
        stride: usize,
        origin: FrameOrigin,
    ) -> Result<Self, FrameError> {
        if width == 0 || height == 0 {
            return Err(FrameError::EmptyFrame);
        }
        let minimum = usize::try_from(width)
            .ok()
            .and_then(|width| width.checked_mul(BYTES_PER_PIXEL))
            .ok_or(FrameError::InvalidStride {
                stride,
                minimum: usize::MAX,
            })?;
        if stride < minimum {
            return Err(FrameError::InvalidStride { stride, minimum });
        }
        let needed = stride
            .checked_mul(usize::try_from(height).unwrap_or(usize::MAX))
            .ok_or(FrameError::IncompleteFrame {
                needed: usize::MAX,
                actual: pixels.len(),
            })?;
        if pixels.len() < needed {
            return Err(FrameError::IncompleteFrame {
                needed,
                actual: pixels.len(),
            });
        }
        Ok(Self {
            pixels,
            width,
            height,
            stride,
            origin,
        })
    }

    /// Создаёт представление из ответа `frame.capture`.
    pub fn from_frame_ref(frame: &FrameRef, pixels: &'a [u8]) -> Result<Self, FrameError> {
        if frame.format != FRAME_FORMAT_BGRA8 {
            return Err(FrameError::UnsupportedFormat {
                actual: frame.format.clone(),
            });
        }
        Self::new(
            pixels,
            frame.width,
            frame.height,
            usize::try_from(frame.stride).unwrap_or(usize::MAX),
            frame.origin,
        )
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn stride(&self) -> usize {
        self.stride
    }

    pub fn origin(&self) -> FrameOrigin {
        self.origin
    }

    /// Вырезает физический прямоугольник и переводит BGRA в оттенки серого.
    ///
    /// Строка читается по `stride`, поэтому байты выравнивания в конце строки
    /// не попадают в результат. Альфа-канал не используется.
    pub fn crop(&self, selection: Selection) -> Result<GrayImage, FrameError> {
        if selection.width == 0 || selection.height == 0 {
            return Err(FrameError::EmptySelection);
        }

        let left = i64::from(selection.x);
        let top = i64::from(selection.y);
        let right = left + i64::from(selection.width);
        let bottom = top + i64::from(selection.height);
        let frame_left = i64::from(self.origin.x);
        let frame_top = i64::from(self.origin.y);
        let frame_right = frame_left + i64::from(self.width);
        let frame_bottom = frame_top + i64::from(self.height);
        if left < frame_left || top < frame_top || right > frame_right || bottom > frame_bottom {
            return Err(FrameError::OutsideFrame);
        }

        let width = usize::try_from(selection.width).map_err(|_| FrameError::OutsideFrame)?;
        let height = usize::try_from(selection.height).map_err(|_| FrameError::OutsideFrame)?;
        let row_bytes = width
            .checked_mul(BYTES_PER_PIXEL)
            .ok_or(FrameError::OutsideFrame)?;
        let start_x = usize::try_from(left - frame_left).map_err(|_| FrameError::OutsideFrame)?;
        let start_y = usize::try_from(top - frame_top).map_err(|_| FrameError::OutsideFrame)?;
        let capacity = width.checked_mul(height).ok_or(FrameError::OutsideFrame)?;
        let mut gray = Vec::with_capacity(capacity);

        for row in 0..height {
            let source_offset = (start_y + row)
                .checked_mul(self.stride)
                .and_then(|offset| offset.checked_add(start_x * BYTES_PER_PIXEL))
                .ok_or(FrameError::OutsideFrame)?;
            let source = self
                .pixels
                .get(source_offset..source_offset + row_bytes)
                .ok_or(FrameError::OutsideFrame)?;
            for pixel in source.as_chunks::<BYTES_PER_PIXEL>().0 {
                let blue = u32::from(pixel[0]);
                let green = u32::from(pixel[1]);
                let red = u32::from(pixel[2]);
                let luma = (red * 77 + green * 150 + blue * 29) >> 8;
                gray.push(luma as u8);
            }
        }

        Ok(GrayImage {
            width: selection.width,
            height: selection.height,
            pixels: gray,
        })
    }
}

/// Восьмибитное изображение в оттенках серого для поиска QR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrayImage {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl GrayImage {
    /// Проверяет размеры и полноту буфера.
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, ImageError> {
        if width == 0 || height == 0 {
            return Err(ImageError::EmptyImage);
        }
        let needed = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .ok_or(ImageError::IncompleteImage {
                needed: usize::MAX,
                actual: pixels.len(),
            })?;
        if pixels.len() < needed {
            return Err(ImageError::IncompleteImage {
                needed,
                actual: pixels.len(),
            });
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Ищет и декодирует QR в изображении.
    ///
    /// Один код возвращается как [`DecodeOutcome::Text`]. Несколько найденных
    /// сеток требуют выбора одного кода. Повреждённый код и отсутствие кода
    /// различаются, чтобы окно могло предложить новый выбор.
    pub fn decode(&self) -> DecodeOutcome {
        let width = self.width as usize;
        let height = self.height as usize;
        let mut prepared =
            PreparedImage::prepare_from_greyscale(width, height, |x, y| self.pixels[y * width + x]);
        let grids = prepared.detect_grids();
        if grids.is_empty() {
            return DecodeOutcome::NoCode;
        }
        if grids.len() > 1 {
            return DecodeOutcome::Multiple { count: grids.len() };
        }

        let grid = &grids[0];
        let mut bytes = Vec::new();
        match grid.decode_to(&mut bytes) {
            Ok(meta) => classify_payload(meta.version.0, &bytes),
            Err(DeQRError::EncodingError) => classify_invalid_bytes(&bytes),
            Err(_) => DecodeOutcome::Damaged,
        }
    }
}

/// Распознанный текст и версия QR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrText {
    text: String,
    version: u8,
}

impl QrText {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn version(&self) -> u8 {
        self.version
    }
}

/// Исход локального чтения QR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeOutcome {
    /// Найден один текстовый QR.
    Text(QrText),
    /// В выбранной области нет QR.
    NoCode,
    /// QR найден, но повреждён и не декодируется.
    Damaged,
    /// В области несколько кодов: нужно выделить один.
    Multiple { count: usize },
    /// Текст закодирован в неподдерживаемой кодировке.
    UnsupportedEncoding,
    /// Байты не представимы обычным текстом.
    BinaryPayload,
    /// QR содержит пустую строку; копировать нечего.
    Empty,
}

impl DecodeOutcome {
    /// Текст для предпросмотра или копирования.
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text.text()),
            _ => None,
        }
    }

    /// Нужно ли предложить пользователю новый выбор области.
    pub fn needs_new_selection(&self) -> bool {
        matches!(self, Self::NoCode | Self::Damaged)
    }

    /// Нужно ли попросить выделить только один код.
    pub fn needs_single_code(&self) -> bool {
        matches!(self, Self::Multiple { .. })
    }
}

/// Вырезает область и декодирует её.
pub fn decode_qr(frame: &QrFrame<'_>, selection: Selection) -> Result<DecodeOutcome, FrameError> {
    Ok(frame.crop(selection)?.decode())
}

fn classify_payload(version: usize, bytes: &[u8]) -> DecodeOutcome {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return classify_invalid_bytes(bytes);
    };
    if text.is_empty() {
        return DecodeOutcome::Empty;
    }
    if contains_binary_controls(text) {
        return DecodeOutcome::BinaryPayload;
    }
    DecodeOutcome::Text(QrText {
        text: text.to_owned(),
        version: u8::try_from(version).unwrap_or(40),
    })
}

fn classify_invalid_bytes(bytes: &[u8]) -> DecodeOutcome {
    if bytes.is_empty() {
        return DecodeOutcome::Empty;
    }
    // Невалидная UTF-8-строка без управляющих байтов похожа на текст в
    // однобайтовой кодировке, которую плагин не поддерживает. Управляющие
    // байты и NUL означают двоичную нагрузку.
    if bytes
        .iter()
        .any(|byte| (*byte < 0x20 && !matches!(*byte, 0x09 | 0x0a | 0x0d)) || *byte == 0x7f)
    {
        DecodeOutcome::BinaryPayload
    } else {
        DecodeOutcome::UnsupportedEncoding
    }
}

fn contains_binary_controls(text: &str) -> bool {
    text.chars()
        .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
}
