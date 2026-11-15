//! Переносимая выборка цвета из кадра BGRA8 для плагина «Пипетка цвета».
//!
//! Крейт не обращается к Windows API и не показывает overlay. Он принимает
//! снимок виртуального рабочего стола, проверяет физические координаты по
//! геометрии мониторов и возвращает RGB. Формат кадра — BGRA8, верхняя строка
//! первая; альфа-канал при выводе опускается.

use util_maker_sdk::{
    FrameOrigin, FrameRef, FrameRegion, MonitorGeometry, ScreenGeometry, FRAME_FORMAT_BGRA8,
};

pub mod picker;

pub use picker::{
    Arrow, BlockReason, CaptureError, CapturedFrame, ClipboardError, ClipboardSink, CopyOutcome,
    FrameData, FrameLease, FrameRect, FrameSource, Loupe, LoupeError, Picker, PickerError,
    PickerPhase, PickerSession, StartOutcome,
};

/// Размер одного пикселя BGRA8 в байтах.
pub const BYTES_PER_PIXEL: usize = 4;

/// Точка в физических пикселях виртуального рабочего стола.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// Прямоугольник монитора в физических пикселях виртуального рабочего стола.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MonitorRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl MonitorRect {
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Содержит ли монитор точку. Правая и нижняя границы не входят.
    pub fn contains(self, point: Point) -> bool {
        let x = i64::from(point.x);
        let y = i64::from(point.y);
        let left = i64::from(self.x);
        let top = i64::from(self.y);
        x >= left
            && y >= top
            && x < left + i64::from(self.width)
            && y < top + i64::from(self.height)
    }

    fn is_valid(self) -> bool {
        self.width > 0 && self.height > 0
    }

    pub(crate) fn from_sdk(monitor: &MonitorGeometry) -> Self {
        Self::from_region(monitor.rect)
    }

    fn from_region(region: FrameRegion) -> Self {
        Self {
            x: region.x,
            y: region.y,
            width: region.width,
            height: region.height,
        }
    }
}

/// Цвет пикселя в порядке R, G, B. Альфа не хранится.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Читает B, G, R из пикселя BGRA8. Альфа игнорируется.
    pub const fn from_bgra(pixel: [u8; BYTES_PER_PIXEL]) -> Self {
        Self {
            r: pixel[2],
            g: pixel[1],
            b: pixel[0],
        }
    }

    /// Шесть цифр в верхнем регистре, например `#2F80ED`.
    pub fn to_hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }

    /// CSS RGB с десятичными компонентами, например `rgb(47, 128, 237)`.
    pub fn to_css_rgb(self) -> String {
        format!("rgb({}, {}, {})", self.r, self.g, self.b)
    }

    /// Строка в выбранном пользователем формате.
    pub fn format(self, format: ColorFormat) -> String {
        match format {
            ColorFormat::Hex => self.to_hex(),
            ColorFormat::CssRgb => self.to_css_rgb(),
        }
    }
}

/// Формат строки, которую плагин копирует в буфер обмена.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorFormat {
    /// `#RRGGBB`, формат по умолчанию.
    #[default]
    Hex,
    /// `rgb(R, G, B)`.
    CssRgb,
}

impl ColorFormat {
    /// Значение настройки «Формат цвета».
    pub const fn setting_value(self) -> &'static str {
        match self {
            Self::Hex => "hex",
            Self::CssRgb => "rgb",
        }
    }

    /// Разбирает значение настройки без учёта регистра. Неизвестное значение
    /// не подменяется форматом по умолчанию: вызывающий код решает, показать
    /// ошибку или вернуть HEX.
    pub fn from_setting(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "hex" => Some(Self::Hex),
            "rgb" | "css-rgb" | "css_rgb" => Some(Self::CssRgb),
            _ => None,
        }
    }
}

/// Ошибка подготовки кадра до чтения пикселей.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("формат кадра {actual} не поддерживается, ожидается BGRA8")]
    UnsupportedFormat { actual: String },
    #[error("геометрия мониторов в ответе отсутствует")]
    GeometryUnavailable,
    #[error("размер кадра должен быть больше нуля")]
    EmptyFrame,
    #[error("stride {stride} меньше минимального размера строки {minimum}")]
    InvalidStride { stride: usize, minimum: usize },
    #[error("буфер кадра неполный: нужно {needed} байт, получено {actual}")]
    IncompleteFrame { needed: usize, actual: usize },
    #[error("монитор {index} имеет неположительный размер")]
    InvalidMonitor { index: usize },
}

/// Причина отказа при чтении точки.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SampleError {
    #[error("точка ({x}, {y}) не принадлежит ни одному монитору")]
    OutsideMonitors { x: i32, y: i32 },
    #[error("точка ({x}, {y}) вне кадра")]
    OutsideFrame { x: i32, y: i32 },
    #[error("пиксель выходит за границы буфера кадра")]
    OutOfBounds,
}

/// Снимок BGRA8 и геометрия, необходимые для выборки цвета.
///
/// `pixels` содержит строки сверху вниз, `stride` задан в байтах и может быть
/// больше `width * 4`. Хвост строки не читается. Геометрия мониторов копируется
/// при создании, поэтому снимок не зависит от времени жизни `FrameRef`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameView<'a> {
    pixels: &'a [u8],
    width: u32,
    height: u32,
    stride: usize,
    origin: Point,
    monitors: Vec<MonitorRect>,
}

impl<'a> FrameView<'a> {
    /// Проверяет размеры кадра и геометрию мониторов.
    pub fn new(
        pixels: &'a [u8],
        width: u32,
        height: u32,
        stride: usize,
        origin: Point,
        monitors: &[MonitorRect],
    ) -> Result<Self, FrameError> {
        if width == 0 || height == 0 {
            return Err(FrameError::EmptyFrame);
        }
        if monitors.is_empty() {
            return Err(FrameError::GeometryUnavailable);
        }
        for (index, monitor) in monitors.iter().enumerate() {
            if !monitor.is_valid() {
                return Err(FrameError::InvalidMonitor { index });
            }
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
            monitors: monitors.to_vec(),
        })
    }

    /// Создаёт представление из ответа `frame.capture`. Кадр без геометрии
    /// отклоняется: без мониторов нельзя отличить пиксель от промежутка.
    pub fn from_frame_ref(frame: &FrameRef, pixels: &'a [u8]) -> Result<Self, FrameError> {
        if frame.format != FRAME_FORMAT_BGRA8 {
            return Err(FrameError::UnsupportedFormat {
                actual: frame.format.clone(),
            });
        }
        let geometry = frame
            .geometry
            .as_ref()
            .ok_or(FrameError::GeometryUnavailable)?;
        let monitors = geometry
            .monitors
            .iter()
            .map(MonitorRect::from_sdk)
            .collect::<Vec<_>>();
        Self::new(
            pixels,
            frame.width,
            frame.height,
            usize::try_from(frame.stride).unwrap_or(usize::MAX),
            Point::new(frame.origin.x, frame.origin.y),
            &monitors,
        )
    }

    /// Создаёт представление из уже разобранной геометрии.
    pub fn from_screen(
        pixels: &'a [u8],
        width: u32,
        height: u32,
        stride: usize,
        origin: FrameOrigin,
        geometry: &ScreenGeometry,
    ) -> Result<Self, FrameError> {
        let monitors = geometry
            .monitors
            .iter()
            .map(MonitorRect::from_sdk)
            .collect::<Vec<_>>();
        Self::new(
            pixels,
            width,
            height,
            stride,
            Point::new(origin.x, origin.y),
            &monitors,
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

    pub fn origin(&self) -> Point {
        self.origin
    }

    pub fn monitors(&self) -> &[MonitorRect] {
        &self.monitors
    }

    /// Читает цвет в физической точке виртуального рабочего стола.
    ///
    /// Сначала точка должна попасть в монитор, затем в кадр. Поэтому пиксель
    /// из промежутка между мониторами и пиксель за пределами снимка дают разные
    /// ошибки и никогда не читаются как случайный сосед.
    pub fn sample(&self, x: i32, y: i32) -> Result<Rgb, SampleError> {
        FrameLayout {
            pixels: self.pixels,
            width: self.width,
            height: self.height,
            stride: self.stride,
            origin: self.origin,
            monitors: &self.monitors,
        }
        .sample(x, y)
    }

    /// Читает цвет в точке [`Point`].
    pub fn sample_point(&self, point: Point) -> Result<Rgb, SampleError> {
        self.sample(point.x, point.y)
    }
}

struct FrameLayout<'a> {
    pixels: &'a [u8],
    width: u32,
    height: u32,
    stride: usize,
    origin: Point,
    monitors: &'a [MonitorRect],
}

impl FrameLayout<'_> {
    fn sample(&self, x: i32, y: i32) -> Result<Rgb, SampleError> {
        let point = Point::new(x, y);
        if !self.monitors.iter().any(|monitor| monitor.contains(point)) {
            return Err(SampleError::OutsideMonitors { x, y });
        }

        let local_x = i64::from(x) - i64::from(self.origin.x);
        let local_y = i64::from(y) - i64::from(self.origin.y);
        if local_x < 0
            || local_y < 0
            || local_x >= i64::from(self.width)
            || local_y >= i64::from(self.height)
        {
            return Err(SampleError::OutsideFrame { x, y });
        }

        let row = (local_y as usize)
            .checked_mul(self.stride)
            .ok_or(SampleError::OutOfBounds)?;
        let offset = row
            .checked_add((local_x as usize).saturating_mul(BYTES_PER_PIXEL))
            .ok_or(SampleError::OutOfBounds)?;
        let pixel = self
            .pixels
            .get(offset..offset.saturating_add(BYTES_PER_PIXEL))
            .ok_or(SampleError::OutOfBounds)?;
        Ok(Rgb::from_bgra([pixel[0], pixel[1], pixel[2], pixel[3]]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn monitor(x: i32, y: i32, width: u32, height: u32) -> MonitorRect {
        MonitorRect::new(x, y, width, height)
    }

    fn one_pixel_frame(pixel: [u8; 4]) -> FrameView<'static> {
        // Для теста нужен буфер со временем жизни программы.
        let pixels: &'static [u8] = Box::leak(pixel.to_vec().into_boxed_slice());
        let monitors = [monitor(0, 0, 1, 1)];
        FrameView::new(pixels, 1, 1, 4, Point::new(0, 0), &monitors).expect("кадр 1x1")
    }

    #[test]
    fn target_pixel_is_formatted_as_hex_and_css_rgb() {
        let frame = one_pixel_frame([0xED, 0x80, 0x2F, 0xFF]);
        let color = frame.sample(0, 0).expect("пиксель");
        assert_eq!(color, Rgb::new(0x2F, 0x80, 0xED));
        assert_eq!(color.to_hex(), "#2F80ED");
        assert_eq!(color.to_css_rgb(), "rgb(47, 128, 237)");
        assert_eq!(color.format(ColorFormat::Hex), "#2F80ED");
        assert_eq!(color.format(ColorFormat::CssRgb), "rgb(47, 128, 237)");
    }

    #[test]
    fn negative_origin_and_stride_padding_select_exact_neighbors() {
        let width = 3u32;
        let height = 2u32;
        let stride = width as usize * BYTES_PER_PIXEL + 5;
        let mut pixels = vec![0xAAu8; stride * height as usize];
        let colors = [
            [1, 2, 3, 255],
            [4, 5, 6, 255],
            [7, 8, 9, 255],
            [10, 11, 12, 255],
            [13, 14, 15, 255],
            [16, 17, 18, 255],
        ];
        for (index, color) in colors.iter().enumerate() {
            let x = index % width as usize;
            let y = index / width as usize;
            let offset = y * stride + x * BYTES_PER_PIXEL;
            pixels[offset..offset + BYTES_PER_PIXEL].copy_from_slice(color);
        }
        let monitors = [monitor(-8, -4, width, height)];
        let frame = FrameView::new(
            &pixels,
            width,
            height,
            stride,
            Point::new(-8, -4),
            &monitors,
        )
        .expect("кадр");

        assert_eq!(frame.sample(-8, -4), Ok(Rgb::new(3, 2, 1)));
        assert_eq!(frame.sample(-7, -4), Ok(Rgb::new(6, 5, 4)));
        assert_eq!(frame.sample(-8, -3), Ok(Rgb::new(12, 11, 10)));
        assert_eq!(frame.sample(-6, -3), Ok(Rgb::new(18, 17, 16)));
        // Байты отступа 0xAA не должны попадать в выборку.
        assert_eq!(frame.sample(-6, -4), Ok(Rgb::new(9, 8, 7)));
    }

    #[test]
    fn black_and_white_are_exact() {
        let pixels = [
            0, 0, 0, 255, // физическая точка (0, 0)
            255, 255, 255, 255, // (1, 0)
        ];
        let monitors = [monitor(0, 0, 2, 1)];
        let frame = FrameView::new(&pixels, 2, 1, 8, Point::new(0, 0), &monitors).expect("кадр");
        assert_eq!(frame.sample(0, 0).unwrap().to_hex(), "#000000");
        assert_eq!(frame.sample(1, 0).unwrap().to_hex(), "#FFFFFF");
    }

    #[test]
    fn alpha_is_ignored() {
        let pixels = [0x10, 0x20, 0x30, 0x00, 0x10, 0x20, 0x30, 0xFF];
        let monitors = [monitor(0, 0, 2, 1)];
        let frame = FrameView::new(&pixels, 2, 1, 8, Point::new(0, 0), &monitors).expect("кадр");
        assert_eq!(frame.sample(0, 0), frame.sample(1, 0));
        assert_eq!(frame.sample(0, 0).unwrap().to_hex(), "#302010");
    }

    #[test]
    fn gap_between_monitors_is_rejected() {
        let pixels = vec![0u8; 40 * 4];
        let monitors = [monitor(-10, 0, 4, 4), monitor(-4, 0, 4, 4)];
        let frame =
            FrameView::new(&pixels, 10, 4, 40, Point::new(-10, 0), &monitors).expect("кадр");
        assert_eq!(
            frame.sample(-5, 1),
            Err(SampleError::OutsideMonitors { x: -5, y: 1 })
        );
        assert!(frame.sample(-7, 1).is_ok());
        assert!(frame.sample(-4, 1).is_ok());
    }

    #[test]
    fn monitor_point_outside_frame_is_rejected() {
        let pixels = vec![0u8; 20 * 4];
        let monitors = [monitor(-5, 0, 10, 4)];
        let frame = FrameView::new(&pixels, 5, 4, 20, Point::new(0, 0), &monitors).expect("кадр");
        assert_eq!(
            frame.sample(-1, 0),
            Err(SampleError::OutsideFrame { x: -1, y: 0 })
        );
    }

    #[test]
    fn frame_point_outside_monitors_is_rejected() {
        let pixels = vec![0u8; 10 * 10 * 4];
        let monitors = [monitor(0, 0, 4, 4)];
        let frame = FrameView::new(&pixels, 10, 10, 40, Point::new(0, 0), &monitors).expect("кадр");
        assert_eq!(
            frame.sample(8, 8),
            Err(SampleError::OutsideMonitors { x: 8, y: 8 })
        );
    }

    #[test]
    fn incomplete_frame_and_short_stride_are_rejected() {
        assert_eq!(
            FrameView::new(&[0; 4], 2, 1, 4, Point::new(0, 0), &[monitor(0, 0, 2, 1)]),
            Err(FrameError::InvalidStride {
                stride: 4,
                minimum: 8
            })
        );
        assert_eq!(
            FrameView::new(&[0; 7], 2, 1, 8, Point::new(0, 0), &[monitor(0, 0, 2, 1)]),
            Err(FrameError::IncompleteFrame {
                needed: 8,
                actual: 7
            })
        );
    }

    #[test]
    fn missing_geometry_is_unavailable() {
        let frame_ref = FrameRef {
            buffer_id: "frame-1".to_owned(),
            format: FRAME_FORMAT_BGRA8.to_owned(),
            width: 1,
            height: 1,
            stride: 4,
            origin: FrameOrigin { x: 0, y: 0 },
            scale_factor: 1.0,
            expires_in_ms: 5_000,
            pixel_path: None,
            geometry: None,
        };
        assert_eq!(
            FrameView::from_frame_ref(&frame_ref, &[0, 0, 0, 255]),
            Err(FrameError::GeometryUnavailable)
        );
    }

    #[test]
    fn frame_ref_geometry_is_used() {
        let frame_ref = FrameRef {
            buffer_id: "frame-2".to_owned(),
            format: FRAME_FORMAT_BGRA8.to_owned(),
            width: 1,
            height: 1,
            stride: 4,
            origin: FrameOrigin { x: -5, y: 7 },
            scale_factor: 1.0,
            expires_in_ms: 5_000,
            geometry: Some(ScreenGeometry {
                virtual_screen: FrameRegion {
                    x: -5,
                    y: 7,
                    width: 1,
                    height: 1,
                },
                monitors: vec![MonitorGeometry {
                    rect: FrameRegion {
                        x: -5,
                        y: 7,
                        width: 1,
                        height: 1,
                    },
                    scale_factor: 1.0,
                    primary: true,
                }],
            }),
            pixel_path: None,
        };
        let frame = FrameView::from_frame_ref(&frame_ref, &[0xED, 0x80, 0x2F, 0xFF])
            .expect("кадр из FrameRef");
        assert_eq!(frame.sample(-5, 7).unwrap().to_hex(), "#2F80ED");
    }

    #[test]
    fn unsupported_format_is_rejected() {
        let frame_ref = FrameRef {
            buffer_id: "frame-3".to_owned(),
            format: "rgba8".to_owned(),
            width: 1,
            height: 1,
            stride: 4,
            origin: FrameOrigin { x: 0, y: 0 },
            scale_factor: 1.0,
            expires_in_ms: 5_000,
            geometry: Some(ScreenGeometry {
                virtual_screen: FrameRegion {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                monitors: vec![],
            }),
            pixel_path: None,
        };
        assert_eq!(
            FrameView::from_frame_ref(&frame_ref, &[0, 0, 0, 255]),
            Err(FrameError::UnsupportedFormat {
                actual: "rgba8".to_owned()
            })
        );
    }

    #[test]
    fn extreme_coordinates_do_not_panic() {
        let frame = one_pixel_frame([0, 0, 0, 255]);
        assert_eq!(
            frame.sample(i32::MAX, i32::MAX),
            Err(SampleError::OutsideMonitors {
                x: i32::MAX,
                y: i32::MAX
            })
        );
    }

    #[test]
    fn format_setting_is_case_insensitive_and_does_not_hide_typos() {
        assert_eq!(ColorFormat::default(), ColorFormat::Hex);
        assert_eq!(ColorFormat::from_setting("HEX"), Some(ColorFormat::Hex));
        assert_eq!(
            ColorFormat::from_setting(" rgb "),
            Some(ColorFormat::CssRgb)
        );
        assert_eq!(ColorFormat::from_setting("hsl"), None);
    }

    #[test]
    fn reference_json_matches_sampler() {
        let reference: Value =
            serde_json::from_str(include_str!("../reference/sample.json")).expect("эталон JSON");
        let width = reference["width"].as_u64().unwrap() as u32;
        let height = reference["height"].as_u64().unwrap() as u32;
        let stride = reference["stride"].as_u64().unwrap() as usize;
        let origin_x = reference["origin"]["x"].as_i64().unwrap() as i32;
        let origin_y = reference["origin"]["y"].as_i64().unwrap() as i32;
        let monitors = reference["monitors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| {
                monitor(
                    item["x"].as_i64().unwrap() as i32,
                    item["y"].as_i64().unwrap() as i32,
                    item["width"].as_u64().unwrap() as u32,
                    item["height"].as_u64().unwrap() as u32,
                )
            })
            .collect::<Vec<_>>();
        let mut pixels = vec![0u8; stride * height as usize];

        for point in reference["points"].as_array().unwrap() {
            let x = point["physical"]["x"].as_i64().unwrap() as i32;
            let y = point["physical"]["y"].as_i64().unwrap() as i32;
            let bgra = point["bgra"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item.as_u64().unwrap() as u8)
                .collect::<Vec<_>>();
            let local_x = (x - origin_x) as usize;
            let local_y = (y - origin_y) as usize;
            let offset = local_y * stride + local_x * BYTES_PER_PIXEL;
            pixels[offset..offset + BYTES_PER_PIXEL].copy_from_slice(&bgra);
        }

        let frame = FrameView::new(
            &pixels,
            width,
            height,
            stride,
            Point::new(origin_x, origin_y),
            &monitors,
        )
        .expect("эталонный кадр");
        for point in reference["points"].as_array().unwrap() {
            let name = point["name"].as_str().unwrap();
            let x = point["physical"]["x"].as_i64().unwrap() as i32;
            let y = point["physical"]["y"].as_i64().unwrap() as i32;
            let color = frame.sample(x, y).unwrap_or_else(|error| {
                panic!("точка {name} ({x}, {y}) не читается: {error}");
            });
            assert_eq!(color.to_hex(), point["hex"].as_str().unwrap(), "{name}");
            assert_eq!(
                color.to_css_rgb(),
                point["cssRgb"].as_str().unwrap(),
                "{name}"
            );
            assert_eq!(
                point["image"]["x"].as_i64().unwrap(),
                i64::from(x - origin_x),
                "{name}: X на PNG"
            );
            assert_eq!(
                point["image"]["y"].as_i64().unwrap(),
                i64::from(y - origin_y),
                "{name}: Y на PNG"
            );
        }
    }
}
