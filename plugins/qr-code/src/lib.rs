//! Локальное кодирование текста в QR Code Model 2.
//!
//! Крейт формирует матрицу с коррекцией M, кодирует Unicode как UTF-8 с
//! ECI 26 и отрисовывает чёрно-белое изображение с целым масштабом модулей и
//! белым полем не меньше четырёх модулей. Отрисовка не зависит от Windows API
//! и может проверяться независимым декодером.

use qrcodegen::{QrCode, QrCodeEcc, QrSegment, Version};

mod decode;

pub use decode::{
    decode_qr, DecodeOutcome, FrameError, GrayImage, ImageError, QrFrame, QrText, Selection,
    BYTES_PER_PIXEL,
};

/// Минимальное белое поле вокруг кода в модулях.
pub const QUIET_ZONE_MODULES: u32 = 4;

/// Назначение ECI для UTF-8.
pub const UTF8_ECI: u32 = 26;

/// Ошибка кодирования текста.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EncodeError {
    /// Пустая строка не превращается в QR: пользователь должен увидеть причину.
    #[error("Введите текст для QR-кода")]
    EmptyInput,
    /// Текст не помещается в один QR даже максимальной версии. Ввод не усекается.
    #[error("Текст не помещается в один QR-код: {bytes} байт UTF-8")]
    TooLong { bytes: usize },
}

/// Ошибка отрисовки матрицы в окне заданного размера.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RenderError {
    #[error("Ширина и высота окна должны быть больше нуля")]
    EmptyWindow,
    #[error(
        "Окно {width}×{height} слишком мало для QR: нужно минимум {minimum}×{minimum} пикселей"
    )]
    TooSmall {
        width: u32,
        height: u32,
        minimum: u32,
    },
}

/// Готовая матрица QR без белого поля.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrMatrix {
    version: u8,
    size: usize,
    modules: Vec<bool>,
}

impl QrMatrix {
    /// Кодирует непустой текст в QR Code Model 2 с коррекцией M.
    ///
    /// Перед байтовым сегментом добавляется ECI 26, поэтому кириллица, emoji
    /// и любые другие символы Unicode восстанавливаются из UTF-8 побайтно.
    /// Пробелы и переводы строк не нормализуются.
    pub fn encode(text: &str) -> Result<Self, EncodeError> {
        if text.is_empty() {
            return Err(EncodeError::EmptyInput);
        }

        let segments = [
            QrSegment::make_eci(UTF8_ECI),
            QrSegment::make_bytes(text.as_bytes()),
        ];
        let code = QrCode::encode_segments_advanced(
            &segments,
            QrCodeEcc::Medium,
            Version::MIN,
            Version::MAX,
            None,
            false,
        )
        .map_err(|_| EncodeError::TooLong { bytes: text.len() })?;

        let size = code.size() as usize;
        let mut modules = Vec::with_capacity(size * size);
        for y in 0..code.size() {
            for x in 0..code.size() {
                modules.push(code.get_module(x, y));
            }
        }
        Ok(Self {
            version: code.version().value(),
            size,
            modules,
        })
    }

    /// Версия QR в диапазоне 1–40. Она растёт при увеличении объёма данных.
    pub fn version(&self) -> u8 {
        self.version
    }

    /// Количество модулей по стороне без белого поля.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Цвет модуля: `true` — чёрный. Координаты вне матрицы считаются белыми.
    pub fn module(&self, x: usize, y: usize) -> bool {
        if x >= self.size || y >= self.size {
            return false;
        }
        self.modules[y * self.size + x]
    }

    /// Модули построчно, `true` — чёрный.
    pub fn modules(&self) -> &[bool] {
        &self.modules
    }

    /// Отрисовывает код по центру окна.
    ///
    /// Масштаб — наибольшее целое число, при котором код вместе с полем
    /// помещается в окно. Свободное место остаётся белым, поэтому фактическое
    /// поле может быть больше четырёх модулей. Если даже один пиксель на модуль
    /// не помещается, возвращается [`RenderError::TooSmall`].
    pub fn render(&self, width: u32, height: u32) -> Result<QrImage, RenderError> {
        if width == 0 || height == 0 {
            return Err(RenderError::EmptyWindow);
        }
        let total_modules = self.size as u32 + QUIET_ZONE_MODULES * 2;
        let scale = (width / total_modules).min(height / total_modules);
        if scale == 0 {
            return Err(RenderError::TooSmall {
                width,
                height,
                minimum: total_modules,
            });
        }

        let mut pixels = vec![255u8; width as usize * height as usize];
        let code_size = total_modules * scale;
        let offset_x = (width - code_size) / 2;
        let offset_y = (height - code_size) / 2;
        for y in 0..self.size {
            for x in 0..self.size {
                if !self.module(x, y) {
                    continue;
                }
                let left = offset_x + (QUIET_ZONE_MODULES + x as u32) * scale;
                let top = offset_y + (QUIET_ZONE_MODULES + y as u32) * scale;
                for py in top..top + scale {
                    let row = py as usize * width as usize;
                    for px in left..left + scale {
                        pixels[row + px as usize] = 0;
                    }
                }
            }
        }

        Ok(QrImage {
            width,
            height,
            scale,
            pixels,
        })
    }
}

/// Чёрно-белое изображение QR в градациях серого: `0` — чёрный, `255` — белый.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrImage {
    width: u32,
    height: u32,
    scale: u32,
    pixels: Vec<u8>,
}

impl QrImage {
    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Целое число физических пикселей на один модуль.
    pub fn scale(&self) -> u32 {
        self.scale
    }

    /// Значение пикселя: `0` или `255`.
    pub fn pixel(&self, x: u32, y: u32) -> u8 {
        self.pixels[y as usize * self.width as usize + x as usize]
    }

    /// Пиксели построчно.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rqrr::PreparedImage;

    fn decode(image: &QrImage) -> String {
        let mut prepared = PreparedImage::prepare_from_greyscale(
            image.width() as usize,
            image.height() as usize,
            |x, y| image.pixel(x as u32, y as u32),
        );
        let grids = prepared.detect_grids();
        assert_eq!(grids.len(), 1, "ожидался один QR");
        let (_, text) = grids[0].decode().expect("независимое декодирование");
        text
    }

    fn round_trip(text: &str) {
        let matrix = QrMatrix::encode(text).expect("кодирование");
        let image = matrix.render(640, 640).expect("отрисовка");
        assert_eq!(decode(&image), text);
    }

    #[test]
    fn control_strings_round_trip_byte_for_byte() {
        round_trip("https://example.com/search?q=%D0%BF%D1%80%D0%B8%D0%B2%D0%B5%D1%82&lang=ru#top");
        round_trip("  Привет, мир! 👋\nСтрока 2\t— конец  ");
    }

    #[test]
    fn empty_input_is_rejected() {
        assert_eq!(QrMatrix::encode(""), Err(EncodeError::EmptyInput));
    }

    #[test]
    fn over_capacity_is_rejected_without_truncation() {
        let text = "a".repeat(5_000);
        assert_eq!(
            QrMatrix::encode(&text),
            Err(EncodeError::TooLong { bytes: text.len() })
        );
    }

    #[test]
    fn larger_input_uses_a_larger_version() {
        let small = QrMatrix::encode("ok").expect("маленький код");
        let large = QrMatrix::encode(&"x".repeat(500)).expect("большой код");
        assert!(large.version() > small.version());
    }

    #[test]
    fn render_rejects_zero_and_too_small_windows() {
        let matrix = QrMatrix::encode("ok").expect("кодирование");
        assert_eq!(matrix.render(0, 100), Err(RenderError::EmptyWindow));
        let minimum = matrix.size() as u32 + QUIET_ZONE_MODULES * 2;
        assert_eq!(
            matrix.render(minimum - 1, minimum - 1),
            Err(RenderError::TooSmall {
                width: minimum - 1,
                height: minimum - 1,
                minimum,
            })
        );
        assert!(matrix.render(minimum, minimum).is_ok());
    }

    #[test]
    fn render_uses_integer_scale_and_keeps_four_module_quiet_zone() {
        let matrix = QrMatrix::encode("https://example.com").expect("кодирование");
        let image = matrix.render(401, 333).expect("отрисовка");
        assert_eq!(image.width(), 401);
        assert_eq!(image.height(), 333);
        assert!(image.scale() >= 1);

        let mut min_x = image.width();
        let mut min_y = image.height();
        let mut max_x = 0;
        let mut max_y = 0;
        for y in 0..image.height() {
            for x in 0..image.width() {
                if image.pixel(x, y) == 0 {
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                }
            }
        }
        let quiet = QUIET_ZONE_MODULES * image.scale();
        assert!(min_x >= quiet, "слева {min_x} < {quiet}");
        assert!(min_y >= quiet, "сверху {min_y} < {quiet}");
        assert!(image.width() - 1 - max_x >= quiet, "справа мало поля");
        assert!(image.height() - 1 - max_y >= quiet, "снизу мало поля");

        // Каждый модуль занимает ровно scale пикселей: границы чёрных
        // прямоугольников кратны масштабу относительно начала кода.
        let total = matrix.size() as u32 + QUIET_ZONE_MODULES * 2;
        let code_size = total * image.scale();
        let offset_x = (image.width() - code_size) / 2;
        let offset_y = (image.height() - code_size) / 2;
        assert_eq!((min_x - offset_x) % image.scale(), 0);
        assert_eq!((min_y - offset_y) % image.scale(), 0);
    }

    #[test]
    fn rendered_pixels_are_only_black_or_white() {
        let matrix = QrMatrix::encode("ok").expect("кодирование");
        let image = matrix.render(200, 200).expect("отрисовка");
        assert!(image
            .pixels()
            .iter()
            .all(|pixel| *pixel == 0 || *pixel == 255));
    }
}
