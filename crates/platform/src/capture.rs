//! Переносимый контракт захвата рабочего стола и хранилище кадров.
//!
//! Адаптер получает физический прямоугольник виртуального рабочего стола,
//! возвращает пиксели BGRA8 и геометрию мониторов. Пиксели остаются внутри
//! адаптера, а модуль получает только непрозрачную ссылку [`FrameRef`] со
//! сроком жизни. [`ScreenService`] дополнительно проверяет владельца ссылки,
//! поэтому чужой или завершённый вызов не может прочитать кадр.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use util_maker_core::{CallSession, PlatformHost};
use util_maker_sdk::{
    error_codes, FrameOrigin, FrameRef, FrameRegion, FrameRequest, MonitorGeometry, RpcError,
    ScreenGeometry, FRAME_FORMAT_BGRA8,
};

/// Время жизни ссылки по умолчанию, если источник не задал иное.
pub const DEFAULT_FRAME_TTL: Duration = Duration::from_millis(5_000);

/// Пиксели и геометрия, полученные от платформенного источника.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFrame {
    /// Прямоугольник в физических пикселях виртуального рабочего стола.
    pub rect: FrameRegion,
    /// Шаг строки в байтах; для BGRA8 не меньше `width * 4`.
    pub stride: u32,
    /// Верхняя строка идёт первой.
    pub pixels: Vec<u8>,
}

/// Причина отказа платформенного источника.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenSourceError {
    /// Операция недоступна на текущей ОС.
    Unavailable(String),
    /// Системный вызов или чтение пикселей завершились ошибкой.
    Capture(String),
}

impl ScreenSourceError {
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::Unavailable(message.into())
    }

    pub fn capture(message: impl Into<String>) -> Self {
        Self::Capture(message.into())
    }
}

impl fmt::Display for ScreenSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(message) | Self::Capture(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ScreenSourceError {}

/// Источник геометрии и пикселей. На Linux используется синтетическая
/// реализация в тестах, а нативная возвращает [`ScreenSourceError::Unavailable`].
pub trait ScreenSource {
    fn geometry(&self) -> Result<ScreenGeometry, ScreenSourceError>;
    fn capture(&self, rect: FrameRegion) -> Result<RawFrame, ScreenSourceError>;
}

/// Заглушка для ОС без нативного захвата.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnsupportedScreen;

impl ScreenSource for UnsupportedScreen {
    fn geometry(&self) -> Result<ScreenGeometry, ScreenSourceError> {
        Err(ScreenSourceError::unavailable(
            "геометрия рабочего стола поддерживается только в Windows",
        ))
    }

    fn capture(&self, _rect: FrameRegion) -> Result<RawFrame, ScreenSourceError> {
        Err(ScreenSourceError::unavailable(
            "захват рабочего стола поддерживается только в Windows",
        ))
    }
}

struct StoredFrame {
    call_id: String,
    frame: FrameRef,
    pixels: Vec<u8>,
    pixel_path: PathBuf,
    expires_at: Instant,
}

/// Служба захвата и закрепления кадров за вызовом.
pub struct ScreenService<S: ScreenSource> {
    source: S,
    frames: BTreeMap<String, StoredFrame>,
    next_frame: u64,
    ttl: Duration,
}

impl<S: ScreenSource> ScreenService<S> {
    pub fn new(source: S) -> Self {
        Self::with_ttl(source, DEFAULT_FRAME_TTL)
    }

    /// Создаёт службу с явным сроком жизни для детерминированных проверок.
    pub fn with_ttl(source: S, ttl: Duration) -> Self {
        Self {
            source,
            frames: BTreeMap::new(),
            next_frame: 0,
            ttl,
        }
    }

    pub fn source(&self) -> &S {
        &self.source
    }

    pub fn geometry(&self) -> Result<ScreenGeometry, RpcError> {
        self.source.geometry().map_err(source_error)
    }

    /// Захватывает кадр и закрепляет его за `call_id`.
    pub fn capture_for_call(
        &mut self,
        call_id: &str,
        request: &FrameRequest,
    ) -> Result<FrameRef, RpcError> {
        self.expire(Instant::now());
        let geometry = self.source.geometry().map_err(source_error)?;
        let (rect, scale_factor) = select_rect(request, &geometry)?;
        let raw = self.source.capture(rect).map_err(source_error)?;
        validate_raw(&raw, rect)?;

        self.next_frame = self.next_frame.saturating_add(1);
        let buffer_id = format!("{call_id}-frame-{}", self.next_frame);
        let pixel_path = frame_pixel_path(&buffer_id);
        std::fs::write(&pixel_path, &raw.pixels).map_err(|error| {
            RpcError::new(error_codes::INTERNAL_ERROR, format!("файл кадра: {error}"))
        })?;
        let frame = FrameRef {
            buffer_id: buffer_id.clone(),
            format: FRAME_FORMAT_BGRA8.to_owned(),
            width: rect.width,
            height: rect.height,
            stride: raw.stride,
            origin: FrameOrigin {
                x: rect.x,
                y: rect.y,
            },
            scale_factor,
            expires_in_ms: duration_ms(self.ttl),
            geometry: Some(geometry),
            pixel_path: Some(pixel_path.to_string_lossy().into_owned()),
        };
        self.frames.insert(
            buffer_id,
            StoredFrame {
                call_id: call_id.to_owned(),
                frame: frame.clone(),
                pixels: raw.pixels,
                pixel_path,
                expires_at: Instant::now() + self.ttl,
            },
        );
        Ok(frame)
    }

    /// Возвращает описание кадра только владельцу ссылки.
    pub fn frame(&mut self, call_id: &str, buffer_id: &str) -> Result<&FrameRef, RpcError> {
        self.expire(Instant::now());
        self.owned(call_id, buffer_id).map(|stored| &stored.frame)
    }

    /// Возвращает пиксели только владельцу ссылки.
    pub fn frame_bytes(&mut self, call_id: &str, buffer_id: &str) -> Result<&[u8], RpcError> {
        self.expire(Instant::now());
        self.owned(call_id, buffer_id)
            .map(|stored| stored.pixels.as_slice())
    }

    /// Освобождает ссылку. Повторное освобождение сообщает об истечении.
    pub fn release(&mut self, call_id: &str, buffer_id: &str) -> Result<(), RpcError> {
        self.expire(Instant::now());
        let stored = self.frames.get(buffer_id).ok_or_else(expired)?;
        if stored.call_id != call_id {
            return Err(RpcError::new(
                error_codes::CAPABILITY_DENIED,
                "кадр принадлежит другому вызову",
            ));
        }
        if let Some(stored) = self.frames.remove(buffer_id) {
            let _ = std::fs::remove_file(stored.pixel_path);
        }
        Ok(())
    }

    /// Количество живых буферов; просроченные удаляются перед подсчётом.
    pub fn buffered_frames(&mut self) -> usize {
        self.expire(Instant::now());
        self.frames.len()
    }

    fn owned(&self, call_id: &str, buffer_id: &str) -> Result<&StoredFrame, RpcError> {
        let stored = self.frames.get(buffer_id).ok_or_else(expired)?;
        if stored.call_id != call_id {
            return Err(RpcError::new(
                error_codes::CAPABILITY_DENIED,
                "кадр принадлежит другому вызову",
            ));
        }
        Ok(stored)
    }

    fn expire(&mut self, now: Instant) {
        let expired: Vec<_> = self
            .frames
            .iter()
            .filter(|(_, stored)| now >= stored.expires_at)
            .map(|(buffer_id, _)| buffer_id.clone())
            .collect();
        for buffer_id in expired {
            if let Some(stored) = self.frames.remove(&buffer_id) {
                let _ = std::fs::remove_file(stored.pixel_path);
            }
        }
    }
}

impl<S: ScreenSource> PlatformHost for ScreenService<S> {
    fn capture_frame(
        &mut self,
        context: &CallSession,
        request: &FrameRequest,
    ) -> Result<FrameRef, RpcError> {
        self.capture_for_call(&context.call_id, request)
    }

    fn release_frame(&mut self, context: &CallSession, buffer_id: &str) -> Result<(), RpcError> {
        self.release(&context.call_id, buffer_id)
    }
}

impl<S: ScreenSource> Drop for ScreenService<S> {
    fn drop(&mut self) {
        for stored in self.frames.values() {
            let _ = std::fs::remove_file(&stored.pixel_path);
        }
    }
}

fn frame_pixel_path(buffer_id: &str) -> PathBuf {
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
    std::env::temp_dir().join(format!("util-maker-frame-{safe}.bgra"))
}

fn source_error(error: ScreenSourceError) -> RpcError {
    match error {
        ScreenSourceError::Unavailable(message) => {
            RpcError::new(error_codes::CAPABILITY_DENIED, message)
        }
        ScreenSourceError::Capture(message) => RpcError::new(error_codes::INTERNAL_ERROR, message),
    }
}

fn expired() -> RpcError {
    RpcError::new(error_codes::BUFFER_EXPIRED, "ссылка на кадр истекла")
}

fn duration_ms(duration: Duration) -> u32 {
    let millis = duration.as_millis();
    millis.clamp(1, u128::from(u32::MAX)) as u32
}

fn invalid(message: impl Into<String>) -> RpcError {
    RpcError::new(error_codes::INVALID_PARAMS, message)
}

fn select_rect(
    request: &FrameRequest,
    geometry: &ScreenGeometry,
) -> Result<(FrameRegion, f64), RpcError> {
    let virtual_screen = checked_rect(
        geometry.virtual_screen,
        "виртуальный рабочий стол имеет нулевой размер",
    )?;
    if geometry.monitors.is_empty() {
        return Err(source_error(ScreenSourceError::capture(
            "Windows не вернул ни одного монитора",
        )));
    }
    let explicit_monitor = request
        .monitor
        .as_deref()
        .map(|name| monitor_index(name, geometry))
        .transpose()?;

    let rect = if let Some(region) = request.region {
        let region = checked_rect(region, "область кадра имеет нулевой размер")?;
        if !contains(virtual_screen, region) {
            return Err(invalid("область кадра выходит за виртуальный рабочий стол"));
        }
        if let Some(index) = explicit_monitor {
            if !contains(geometry.monitors[index].rect, region) {
                return Err(invalid("область кадра выходит за выбранный монитор"));
            }
        }
        region
    } else if let Some(index) = explicit_monitor {
        checked_rect(
            geometry.monitors[index].rect,
            "выбранный монитор имеет нулевой размер",
        )?
    } else {
        virtual_screen
    };

    let scale_factor = scale_for(rect, geometry);
    Ok((rect, scale_factor))
}

fn checked_rect(rect: FrameRegion, message: &str) -> Result<FrameRegion, RpcError> {
    if rect.width == 0 || rect.height == 0 {
        return Err(invalid(message));
    }
    Ok(rect)
}

fn monitor_index(name: &str, geometry: &ScreenGeometry) -> Result<usize, RpcError> {
    if name.eq_ignore_ascii_case("primary") {
        return geometry
            .monitors
            .iter()
            .position(|monitor| monitor.primary)
            .ok_or_else(|| invalid("в геометрии нет основного монитора"));
    }
    name.parse::<usize>()
        .ok()
        .filter(|index| *index < geometry.monitors.len())
        .ok_or_else(|| {
            invalid(format!(
                "неизвестный монитор {name}: используйте primary или индекс 0..{}",
                geometry.monitors.len().saturating_sub(1)
            ))
        })
}

fn scale_for(rect: FrameRegion, geometry: &ScreenGeometry) -> f64 {
    let mut best: Option<(&MonitorGeometry, u64)> = None;
    for monitor in &geometry.monitors {
        let area = intersection_area(rect, monitor.rect);
        if area == 0 {
            continue;
        }
        let replace = best.is_none_or(|(current, current_area)| {
            area > current_area || (area == current_area && monitor.primary && !current.primary)
        });
        if replace {
            best = Some((monitor, area));
        }
    }
    best.map(|(monitor, _)| monitor.scale_factor)
        .or_else(|| {
            geometry
                .monitors
                .first()
                .map(|monitor| monitor.scale_factor)
        })
        .unwrap_or(1.0)
}

fn contains(outer: FrameRegion, inner: FrameRegion) -> bool {
    let outer_right = i64::from(outer.x) + i64::from(outer.width);
    let outer_bottom = i64::from(outer.y) + i64::from(outer.height);
    let inner_right = i64::from(inner.x) + i64::from(inner.width);
    let inner_bottom = i64::from(inner.y) + i64::from(inner.height);
    i64::from(inner.x) >= i64::from(outer.x)
        && i64::from(inner.y) >= i64::from(outer.y)
        && inner_right <= outer_right
        && inner_bottom <= outer_bottom
}

fn intersection_area(left: FrameRegion, right: FrameRegion) -> u64 {
    let x1 = i64::from(left.x).max(i64::from(right.x));
    let y1 = i64::from(left.y).max(i64::from(right.y));
    let x2 = (i64::from(left.x) + i64::from(left.width))
        .min(i64::from(right.x) + i64::from(right.width));
    let y2 = (i64::from(left.y) + i64::from(left.height))
        .min(i64::from(right.y) + i64::from(right.height));
    if x2 <= x1 || y2 <= y1 {
        0
    } else {
        (x2 - x1) as u64 * (y2 - y1) as u64
    }
}

fn validate_raw(raw: &RawFrame, expected: FrameRegion) -> Result<(), RpcError> {
    if raw.rect != expected {
        return Err(RpcError::new(
            error_codes::INTERNAL_ERROR,
            format!(
                "источник вернул прямоугольник {}x{}+{}+{} вместо {}x{}+{}+{}",
                raw.rect.width,
                raw.rect.height,
                raw.rect.x,
                raw.rect.y,
                expected.width,
                expected.height,
                expected.x,
                expected.y
            ),
        ));
    }
    let minimum_stride = u64::from(expected.width) * 4;
    if u64::from(raw.stride) < minimum_stride {
        return Err(RpcError::new(
            error_codes::INTERNAL_ERROR,
            "источник вернул слишком короткий шаг строки",
        ));
    }
    let required = u64::from(raw.stride) * u64::from(expected.height);
    if raw.pixels.len() as u64 != required {
        return Err(RpcError::new(
            error_codes::INTERNAL_ERROR,
            format!(
                "источник вернул {} байт вместо {}",
                raw.pixels.len(),
                required
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::time::Duration;

    use util_maker_core::{HostDispatcher, HostSession, InvocationContext, SourceRef};
    use util_maker_sdk::{error_codes, host_methods, HostCall, HostCallContext, HostHandler};

    const LEFT: FrameRegion = FrameRegion {
        x: -1920,
        y: 0,
        width: 1920,
        height: 1080,
    };
    const PRIMARY: FrameRegion = FrameRegion {
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
    };

    #[derive(Debug, Default)]
    struct SyntheticScreen {
        captures: Cell<u32>,
        fail_width: Option<u32>,
        short_stride: bool,
    }

    impl SyntheticScreen {
        fn new() -> Self {
            Self::default()
        }
    }

    impl ScreenSource for SyntheticScreen {
        fn geometry(&self) -> Result<ScreenGeometry, ScreenSourceError> {
            Ok(ScreenGeometry {
                virtual_screen: FrameRegion {
                    x: -1920,
                    y: 0,
                    width: 3840,
                    height: 1080,
                },
                monitors: vec![
                    MonitorGeometry {
                        rect: LEFT,
                        scale_factor: 1.0,
                        primary: false,
                    },
                    MonitorGeometry {
                        rect: PRIMARY,
                        scale_factor: 1.5,
                        primary: true,
                    },
                ],
            })
        }

        fn capture(&self, rect: FrameRegion) -> Result<RawFrame, ScreenSourceError> {
            self.captures.set(self.captures.get() + 1);
            if self.fail_width == Some(rect.width) {
                return Err(ScreenSourceError::capture("синтетический отказ"));
            }
            if self.short_stride {
                let stride = rect.width * 3;
                return Ok(RawFrame {
                    rect,
                    stride,
                    pixels: vec![0u8; (stride * rect.height) as usize],
                });
            }
            let stride = rect.width * 4;
            let mut pixels = vec![0u8; (stride * rect.height) as usize];
            for y in 0..rect.height {
                for x in 0..rect.width {
                    let index = ((y * stride) + x * 4) as usize;
                    let desktop_x = rect.x + x as i32;
                    let desktop_y = rect.y + y as i32;
                    // Младшие байты физических координат позволяют проверить,
                    // что кадр не сдвинут и не отмасштабирован.
                    pixels[index] = desktop_x as u8;
                    pixels[index + 1] = (desktop_x >> 8) as u8;
                    pixels[index + 2] = desktop_y as u8;
                    pixels[index + 3] = 0xff;
                }
            }
            Ok(RawFrame {
                rect,
                stride,
                pixels,
            })
        }
    }

    fn session(call_id: &str) -> CallSession {
        CallSession::new(
            call_id,
            "org.utilmaker.screenshot",
            "capture-region",
            InvocationContext::hotkey(Some(SourceRef("window-1".to_owned()))),
        )
    }

    fn context(call_id: &str) -> HostCallContext {
        HostCallContext::new(
            "org.utilmaker.screenshot",
            call_id,
            vec!["screen.frame".to_owned()],
        )
    }

    fn assert_pixel(bytes: &[u8], stride: u32, x: u32, y: u32, desktop_x: i32, desktop_y: i32) {
        let offset = (y * stride + x * 4) as usize;
        assert_eq!(bytes[offset], desktop_x as u8);
        assert_eq!(bytes[offset + 1], (desktop_x >> 8) as u8);
        assert_eq!(bytes[offset + 2], desktop_y as u8);
        assert_eq!(bytes[offset + 3], 0xff);
    }

    #[test]
    fn captures_left_monitor_and_dpi_boundary_in_physical_pixels() {
        let mut service = ScreenService::new(SyntheticScreen::new());
        let left = service
            .capture_for_call(
                "call-left",
                &FrameRequest {
                    monitor: Some("0".to_owned()),
                    region: Some(FrameRegion {
                        x: -1600,
                        y: 100,
                        width: 320,
                        height: 200,
                    }),
                },
            )
            .expect("кадр слева");
        assert_eq!(left.origin, FrameOrigin { x: -1600, y: 100 });
        assert_eq!((left.width, left.height, left.stride), (320, 200, 1280));
        assert_eq!(left.scale_factor, 1.0);
        let geometry = left.geometry.as_ref().expect("геометрия");
        assert_eq!(geometry.virtual_screen.x, -1920);
        assert_eq!(geometry.monitors[0].rect, LEFT);
        assert_eq!(geometry.monitors[1].scale_factor, 1.5);
        let bytes = service
            .frame_bytes("call-left", &left.buffer_id)
            .expect("пиксели");
        assert_pixel(bytes, left.stride, 0, 0, -1600, 100);
        assert_pixel(bytes, left.stride, 319, 199, -1281, 299);

        let boundary = service
            .capture_for_call(
                "call-boundary",
                &FrameRequest {
                    monitor: None,
                    region: Some(FrameRegion {
                        x: -100,
                        y: 100,
                        width: 300,
                        height: 200,
                    }),
                },
            )
            .expect("кадр на стыке");
        assert_eq!(boundary.origin, FrameOrigin { x: -100, y: 100 });
        assert_eq!(boundary.scale_factor, 1.5);
        let bytes = service
            .frame_bytes("call-boundary", &boundary.buffer_id)
            .expect("пиксели стыка");
        assert_pixel(bytes, boundary.stride, 0, 0, -100, 100);
        assert_pixel(bytes, boundary.stride, 299, 199, 199, 299);
    }

    #[test]
    fn default_capture_covers_virtual_screen_and_primary_monitor() {
        let mut service = ScreenService::new(SyntheticScreen::new());
        let full = service
            .capture_for_call("call-full", &FrameRequest::default())
            .expect("виртуальный экран");
        assert_eq!(full.origin, FrameOrigin { x: -1920, y: 0 });
        assert_eq!((full.width, full.height), (3840, 1080));
        assert_eq!(full.stride, 3840 * 4);
        // При равной площади стыка выбирается основной монитор.
        assert_eq!(full.scale_factor, 1.5);
        assert_eq!(
            full.geometry.as_ref().expect("геометрия").virtual_screen,
            FrameRegion {
                x: -1920,
                y: 0,
                width: 3840,
                height: 1080
            }
        );

        let primary = service
            .capture_for_call(
                "call-primary",
                &FrameRequest {
                    monitor: Some("primary".to_owned()),
                    region: None,
                },
            )
            .expect("основной монитор");
        assert_eq!(primary.origin, FrameOrigin { x: 0, y: 0 });
        assert_eq!((primary.width, primary.height), (1920, 1080));
        assert_eq!(primary.scale_factor, 1.5);
    }

    #[test]
    fn frame_belongs_to_its_call_and_is_released() {
        let mut service = ScreenService::new(SyntheticScreen::new());
        let frame = service
            .capture_for_call("call-one", &FrameRequest::default())
            .expect("кадр");
        let buffer = frame.buffer_id.clone();
        let pixel_path = frame.pixel_path.clone().expect("файл пикселей");
        assert!(std::path::Path::new(&pixel_path).is_file());
        assert_eq!(
            std::fs::metadata(&pixel_path).expect("метаданные").len(),
            u64::from(frame.stride) * u64::from(frame.height)
        );

        let denied = service
            .frame_bytes("call-two", &buffer)
            .expect_err("чужой вызов");
        assert_eq!(denied.code, error_codes::CAPABILITY_DENIED);
        let denied_release = service
            .release("call-two", &buffer)
            .expect_err("чужое освобождение");
        assert_eq!(denied_release.code, error_codes::CAPABILITY_DENIED);

        service.release("call-one", &buffer).expect("освобождение");
        let expired = service
            .frame_bytes("call-one", &buffer)
            .expect_err("ресурс освобождён");
        assert_eq!(expired.code, error_codes::BUFFER_EXPIRED);
        assert_eq!(service.buffered_frames(), 0);
        assert!(!std::path::Path::new(&pixel_path).exists());
    }

    #[test]
    fn large_frame_file_is_readable_and_cleaned_up() {
        let mut service = ScreenService::new(SyntheticScreen::new());
        let frame = service
            .capture_for_call(
                "call-large",
                &FrameRequest {
                    monitor: None,
                    region: Some(FrameRegion {
                        x: 0,
                        y: 0,
                        width: 1024,
                        height: 1024,
                    }),
                },
            )
            .expect("большой кадр");
        let pixels = frame.read_pixels().expect("файл пикселей читается");
        assert_eq!(pixels.len(), 1024 * 1024 * 4);
        assert!(pixels.len() > 1024 * 1024);
        let path = frame.pixel_path.clone().unwrap();
        service
            .release("call-large", &frame.buffer_id)
            .expect("release");
        assert!(!std::path::Path::new(&path).exists());
    }

    #[test]
    fn malformed_frame_is_rejected_without_stored_buffer() {
        let mut synthetic = SyntheticScreen::new();
        synthetic.short_stride = true;
        let mut service = ScreenService::new(synthetic);
        let error = service
            .capture_for_call(
                "call-short",
                &FrameRequest {
                    monitor: None,
                    region: Some(FrameRegion {
                        x: 0,
                        y: 0,
                        width: 16,
                        height: 16,
                    }),
                },
            )
            .expect_err("короткий шаг строки");
        assert_eq!(error.code, error_codes::INTERNAL_ERROR);
        assert_eq!(service.buffered_frames(), 0);
        assert_eq!(service.source().captures.get(), 1);
        let missing = service
            .frame_bytes("call-short", "call-short-frame-1")
            .expect_err("буфер не создан");
        assert_eq!(missing.code, error_codes::BUFFER_EXPIRED);
    }

    #[test]
    fn expired_frame_is_removed_and_capture_error_keeps_other_frames() {
        let mut synthetic = SyntheticScreen::new();
        synthetic.fail_width = Some(42);
        let mut service = ScreenService::with_ttl(synthetic, Duration::from_millis(5));
        let error = service
            .capture_for_call(
                "call-error",
                &FrameRequest {
                    region: Some(FrameRegion {
                        x: 10,
                        y: 10,
                        width: 42,
                        height: 10,
                    }),
                    monitor: None,
                },
            )
            .expect_err("ошибка захвата");
        assert_eq!(error.code, error_codes::INTERNAL_ERROR);
        assert_eq!(service.buffered_frames(), 0);

        let frame = service
            .capture_for_call(
                "call-live",
                &FrameRequest {
                    monitor: None,
                    region: Some(FrameRegion {
                        x: 0,
                        y: 0,
                        width: 16,
                        height: 16,
                    }),
                },
            )
            .expect("кадр до истечения");
        std::thread::sleep(Duration::from_millis(12));
        let expired = service
            .frame_bytes("call-live", &frame.buffer_id)
            .expect_err("срок истёк");
        assert_eq!(expired.code, error_codes::BUFFER_EXPIRED);
        assert_eq!(service.buffered_frames(), 0);
    }

    #[test]
    fn invalid_region_never_reaches_source() {
        let synthetic = SyntheticScreen::new();
        let mut service = ScreenService::new(synthetic);
        let error = service
            .capture_for_call(
                "call-invalid",
                &FrameRequest {
                    monitor: None,
                    region: Some(FrameRegion {
                        x: -2000,
                        y: 0,
                        width: 10,
                        height: 10,
                    }),
                },
            )
            .expect_err("вне экрана");
        assert_eq!(error.code, error_codes::INVALID_PARAMS);
        assert_eq!(service.buffered_frames(), 0);
        assert_eq!(service.source().captures.get(), 0);
    }

    #[test]
    fn dispatcher_finishes_call_and_invalidates_adapter_reference() {
        let mut dispatcher = HostDispatcher::new(ScreenService::new(SyntheticScreen::new()));
        dispatcher.begin_call(&session("call-dispatch"));
        let frame = dispatcher
            .handle(
                &context("call-dispatch"),
                &HostCall {
                    call_id: "call-dispatch".to_owned(),
                    capability: "screen.frame".to_owned(),
                    method: host_methods::FRAME_CAPTURE.to_owned(),
                    params: serde_json::json!({}),
                },
            )
            .expect("кадр через диспетчер");
        let buffer = frame["bufferId"].as_str().expect("ID").to_owned();
        assert!(frame["geometry"]["monitors"].as_array().is_some());
        dispatcher.finish_call("call-dispatch");
        let error = dispatcher
            .platform_mut()
            .frame_bytes("call-dispatch", &buffer)
            .expect_err("после завершения");
        assert_eq!(error.code, error_codes::BUFFER_EXPIRED);
    }

    #[test]
    fn unsupported_native_source_uses_capability_denied() {
        let mut service = ScreenService::new(UnsupportedScreen);
        let error = service
            .capture_for_call("call-linux", &FrameRequest::default())
            .expect_err("нет Windows");
        assert_eq!(error.code, error_codes::CAPABILITY_DENIED);
    }
}
