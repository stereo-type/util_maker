//! Обрезка выбранного прямоугольника из кадра BGRA8.

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
pub enum CropError {
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

/// Кадр BGRA8 с началом в физических координатах рабочего стола.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BgraFrame<'a> {
    pixels: &'a [u8],
    width: u32,
    height: u32,
    stride: usize,
    origin: FrameOrigin,
}

impl<'a> BgraFrame<'a> {
    /// Проверяет размеры и `stride` кадра.
    pub fn new(
        pixels: &'a [u8],
        width: u32,
        height: u32,
        stride: usize,
        origin: FrameOrigin,
    ) -> Result<Self, CropError> {
        if width == 0 || height == 0 {
            return Err(CropError::EmptyFrame);
        }
        let minimum = usize::try_from(width)
            .ok()
            .and_then(|width| width.checked_mul(BYTES_PER_PIXEL))
            .ok_or(CropError::InvalidStride {
                stride,
                minimum: usize::MAX,
            })?;
        if stride < minimum {
            return Err(CropError::InvalidStride { stride, minimum });
        }
        let needed = stride
            .checked_mul(usize::try_from(height).unwrap_or(usize::MAX))
            .ok_or(CropError::IncompleteFrame {
                needed: usize::MAX,
                actual: pixels.len(),
            })?;
        if pixels.len() < needed {
            return Err(CropError::IncompleteFrame {
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
    pub fn from_frame_ref(frame: &FrameRef, pixels: &'a [u8]) -> Result<Self, CropError> {
        if frame.format != FRAME_FORMAT_BGRA8 {
            return Err(CropError::UnsupportedFormat {
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
    /// Строка кадра читается по `stride`, поэтому байты выравнивания в конце
    /// строки не попадают в результат. Альфа-канал не используется.
    pub fn crop(&self, selection: Selection) -> Result<GrayImage, CropError> {
        if selection.width == 0 || selection.height == 0 {
            return Err(CropError::EmptySelection);
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
            return Err(CropError::OutsideFrame);
        }

        let width = usize::try_from(selection.width).map_err(|_| CropError::OutsideFrame)?;
        let height = usize::try_from(selection.height).map_err(|_| CropError::OutsideFrame)?;
        let row_bytes = width
            .checked_mul(BYTES_PER_PIXEL)
            .ok_or(CropError::OutsideFrame)?;
        let start_x = usize::try_from(left - frame_left).map_err(|_| CropError::OutsideFrame)?;
        let start_y = usize::try_from(top - frame_top).map_err(|_| CropError::OutsideFrame)?;
        let capacity = width.checked_mul(height).ok_or(CropError::OutsideFrame)?;
        let mut gray = Vec::with_capacity(capacity);

        for row in 0..height {
            let source_offset = (start_y + row)
                .checked_mul(self.stride)
                .and_then(|offset| offset.checked_add(start_x * BYTES_PER_PIXEL))
                .ok_or(CropError::OutsideFrame)?;
            let source = self
                .pixels
                .get(source_offset..source_offset + row_bytes)
                .ok_or(CropError::OutsideFrame)?;
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

/// Восьмибитное изображение в оттенках серого, готовое для Tesseract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrayImage {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl GrayImage {
    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// PNM/PGM в памяти: Tesseract читает его из stdin без временного файла.
    pub fn to_pnm(&self) -> Vec<u8> {
        let header = format!("P5\n{} {}\n255\n", self.width, self.height);
        let mut result = Vec::with_capacity(header.len() + self.pixels.len());
        result.extend_from_slice(header.as_bytes());
        result.extend_from_slice(&self.pixels);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_with_gray(
        width: u32,
        height: u32,
        stride: usize,
        _origin: FrameOrigin,
        values: &[u8],
    ) -> Vec<u8> {
        let mut pixels = vec![0xAAu8; stride * height as usize];
        for (index, value) in values.iter().enumerate() {
            let x = index % width as usize;
            let y = index / width as usize;
            let offset = y * stride + x * BYTES_PER_PIXEL;
            pixels[offset] = *value;
            pixels[offset + 1] = *value;
            pixels[offset + 2] = *value;
            pixels[offset + 3] = 0xFF;
        }
        pixels
    }

    #[test]
    fn negative_origin_and_stride_padding_are_respected() {
        let width = 3;
        let height = 2;
        let stride = width as usize * BYTES_PER_PIXEL + 5;
        let values = [0, 64, 128, 192, 224, 255];
        let pixels = frame_with_gray(width, height, stride, FrameOrigin { x: -8, y: -4 }, &values);
        let frame = BgraFrame::new(&pixels, width, height, stride, FrameOrigin { x: -8, y: -4 })
            .expect("кадр");

        let image = frame.crop(Selection::new(-8, -4, 2, 2)).expect("выделение");
        assert_eq!(image.width(), 2);
        assert_eq!(image.height(), 2);
        assert_eq!(image.pixels(), &[0, 64, 192, 224]);
        // Байты 0xAA в хвосте строки не должны попасть в изображение.
        assert!(!image.pixels().contains(&0xAA));
    }

    #[test]
    fn selection_with_negative_origin_is_accepted() {
        let pixels = frame_with_gray(10, 10, 40, FrameOrigin { x: -10, y: -10 }, &[255; 100]);
        let frame =
            BgraFrame::new(&pixels, 10, 10, 40, FrameOrigin { x: -10, y: -10 }).expect("кадр");
        let image = frame.crop(Selection::new(-5, -5, 3, 3)).expect("выделение");
        assert_eq!(image.pixels(), &[255; 9]);
    }

    #[test]
    fn empty_selection_is_rejected() {
        let pixels = frame_with_gray(2, 2, 8, FrameOrigin { x: 0, y: 0 }, &[0; 4]);
        let frame = BgraFrame::new(&pixels, 2, 2, 8, FrameOrigin { x: 0, y: 0 }).expect("кадр");
        assert_eq!(
            frame.crop(Selection::new(0, 0, 0, 2)),
            Err(CropError::EmptySelection)
        );
        assert_eq!(
            frame.crop(Selection::new(0, 0, 2, 0)),
            Err(CropError::EmptySelection)
        );
    }

    #[test]
    fn selection_outside_frame_is_rejected() {
        let pixels = frame_with_gray(4, 4, 16, FrameOrigin { x: 0, y: 0 }, &[0; 16]);
        let frame = BgraFrame::new(&pixels, 4, 4, 16, FrameOrigin { x: 0, y: 0 }).expect("кадр");
        assert_eq!(
            frame.crop(Selection::new(3, 3, 2, 2)),
            Err(CropError::OutsideFrame)
        );
        assert_eq!(
            frame.crop(Selection::new(-1, 0, 2, 2)),
            Err(CropError::OutsideFrame)
        );
    }

    #[test]
    fn incomplete_frame_and_short_stride_are_rejected() {
        assert_eq!(
            BgraFrame::new(&[0; 4], 2, 1, 4, FrameOrigin { x: 0, y: 0 }),
            Err(CropError::InvalidStride {
                stride: 4,
                minimum: 8
            })
        );
        assert_eq!(
            BgraFrame::new(&[0; 7], 2, 1, 8, FrameOrigin { x: 0, y: 0 }),
            Err(CropError::IncompleteFrame {
                needed: 8,
                actual: 7
            })
        );
    }

    #[test]
    fn frame_ref_format_is_checked() {
        let frame_ref = FrameRef {
            buffer_id: "frame-1".to_owned(),
            format: "rgba8".to_owned(),
            width: 1,
            height: 1,
            stride: 4,
            origin: FrameOrigin { x: 0, y: 0 },
            scale_factor: 1.0,
            expires_in_ms: 5_000,
            geometry: None,
        };
        assert_eq!(
            BgraFrame::from_frame_ref(&frame_ref, &[0, 0, 0, 255]),
            Err(CropError::UnsupportedFormat {
                actual: "rgba8".to_owned()
            })
        );
    }

    #[test]
    fn pnm_contains_header_and_payload() {
        let pixels = frame_with_gray(2, 1, 8, FrameOrigin { x: 0, y: 0 }, &[10, 20]);
        let frame = BgraFrame::new(&pixels, 2, 1, 8, FrameOrigin { x: 0, y: 0 }).expect("кадр");
        let image = frame.crop(Selection::new(0, 0, 2, 1)).expect("выделение");
        let pnm = image.to_pnm();
        assert!(pnm.starts_with(b"P5\n2 1\n255\n"));
        assert_eq!(&pnm[11..], &[10, 20]);
    }
}
