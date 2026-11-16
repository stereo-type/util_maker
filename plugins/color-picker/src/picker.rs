//! Состояние выбора пикселя и лупы для плагина «Пипетка цвета».
//!
//! Снимок копируется в память до открытия overlay, а ссылка хоста освобождается
//! сразу. Дальнейшие движения мыши и стрелки читают только эту копию и не
//! выполняют повторный захват. Модуль не создаёт окон: он отдаёт состояние,
//! которое overlay хоста рисует без сглаживания.

use std::time::{Duration, Instant};

use util_maker_sdk::{FrameRef, ScreenGeometry, FRAME_FORMAT_BGRA8};

use crate::{
    ColorFormat, FrameError, FrameLayout, MonitorRect, Point, Rgb, SampleError, BYTES_PER_PIXEL,
};

/// Прямоугольник в физических пикселях кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl FrameRect {
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Содержит ли прямоугольник точку. Правая и нижняя границы не входят.
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
}

/// Проверенный снимок BGRA8, которым владеет плагин.
///
/// Геометрия мониторов хранится вместе с пикселями, поэтому выбор не зависит от
/// времени жизни ссылки `FrameRef` и продолжает работать после `expiresInMs`.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameData {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
    stride: usize,
    origin: Point,
    geometry: ScreenGeometry,
    monitors: Vec<MonitorRect>,
}

impl FrameData {
    /// Проверяет буфер и геометрию так же, как [`crate::FrameView`].
    pub fn new(
        pixels: Vec<u8>,
        width: u32,
        height: u32,
        stride: usize,
        origin: Point,
        geometry: ScreenGeometry,
    ) -> Result<Self, FrameError> {
        let monitors = geometry
            .monitors
            .iter()
            .map(MonitorRect::from_sdk)
            .collect::<Vec<_>>();
        crate::FrameView::new(&pixels, width, height, stride, origin, &monitors)?;
        Ok(Self {
            pixels,
            width,
            height,
            stride,
            origin,
            geometry,
            monitors,
        })
    }

    /// Создаёт владеющую копию из ответа `frame.capture`.
    pub fn from_frame_ref(frame: &FrameRef, pixels: Vec<u8>) -> Result<Self, FrameError> {
        if frame.format != FRAME_FORMAT_BGRA8 {
            return Err(FrameError::UnsupportedFormat {
                actual: frame.format.clone(),
            });
        }
        let geometry = frame
            .geometry
            .clone()
            .ok_or(FrameError::GeometryUnavailable)?;
        Self::new(
            pixels,
            frame.width,
            frame.height,
            usize::try_from(frame.stride).unwrap_or(usize::MAX),
            Point::new(frame.origin.x, frame.origin.y),
            geometry,
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

    pub fn geometry(&self) -> &ScreenGeometry {
        &self.geometry
    }

    pub fn monitors(&self) -> &[MonitorRect] {
        &self.monitors
    }

    /// Границы самого снимка в координатах рабочего стола.
    pub fn bounds(&self) -> FrameRect {
        FrameRect::new(self.origin.x, self.origin.y, self.width, self.height)
    }

    /// Читает цвет из уже скопированного кадра.
    pub fn sample(&self, point: Point) -> Result<Rgb, SampleError> {
        FrameLayout {
            pixels: &self.pixels,
            width: self.width,
            height: self.height,
            stride: self.stride,
            origin: self.origin,
            monitors: &self.monitors,
        }
        .sample(point.x, point.y)
    }

    /// Пиксели кадра в порядке RGBA для отрисовки в окне выбора. Промежуток
    /// `stride` в конце строки отбрасывается, верхняя строка идёт первой,
    /// поэтому буфер соответствует `width * height * 4`. Альфа непрозрачная:
    /// окно показывает снимок, а не прозрачность окна пользователя.
    pub fn rgba_rows(&self) -> Vec<u8> {
        let row_len = self.width as usize * BYTES_PER_PIXEL;
        let mut pixels = Vec::with_capacity(row_len * self.height as usize);
        for row in 0..self.height as usize {
            let start = row * self.stride;
            let Some(line) = self.pixels.get(start..start + row_len) else {
                break;
            };
            for pixel in line.as_chunks::<BYTES_PER_PIXEL>().0 {
                pixels.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 0xFF]);
            }
        }
        pixels
    }
}

/// Ссылка хоста на временный буфер кадра. Плагин освобождает её сразу после
/// копирования пикселей в [`FrameData`].
pub trait FrameLease {
    fn release(&mut self) -> Result<(), String>;
}

/// Кадр, полученный от источника, и необязательная ссылка хоста.
pub struct CapturedFrame {
    pub data: FrameData,
    pub lease: Option<Box<dyn FrameLease>>,
    pub expires_in_ms: u32,
}

impl CapturedFrame {
    pub fn new(data: FrameData, expires_in_ms: u32) -> Self {
        Self {
            data,
            lease: None,
            expires_in_ms,
        }
    }

    pub fn with_lease(data: FrameData, expires_in_ms: u32, lease: Box<dyn FrameLease>) -> Self {
        Self {
            data,
            lease: Some(lease),
            expires_in_ms,
        }
    }
}

impl std::fmt::Debug for CapturedFrame {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CapturedFrame")
            .field("data", &self.data)
            .field("lease", &self.lease.as_ref().map(|_| "<lease>"))
            .field("expires_in_ms", &self.expires_in_ms)
            .finish()
    }
}

/// Источник снимка. В тестах и на Linux используется тестовый адаптер.
pub trait FrameSource {
    fn capture(&mut self) -> Result<CapturedFrame, CaptureError>;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CaptureError {
    #[error("захват кадра недоступен: {0}")]
    Unavailable(String),
    #[error("ошибка захвата кадра: {0}")]
    Failed(String),
}

/// Приёмник строки для буфера обмена.
pub trait ClipboardSink {
    fn write_text(&mut self, text: &str) -> Result<(), ClipboardError>;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClipboardError {
    #[error("буфер обмена занят")]
    Busy,
    #[error("{0}")]
    Other(String),
}

/// Причина, по которой копирование недостоверного значения запрещено.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockReason {
    NoFrame,
    OutsideMonitors,
    OutsideFrame,
    OutOfBounds,
    GeometryChanged,
    GeometryUnavailable,
}

impl From<SampleError> for BlockReason {
    fn from(error: SampleError) -> Self {
        match error {
            SampleError::OutsideMonitors { .. } => Self::OutsideMonitors,
            SampleError::OutsideFrame { .. } => Self::OutsideFrame,
            SampleError::OutOfBounds => Self::OutOfBounds,
        }
    }
}

/// Исход попытки скопировать показанное значение.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyOutcome {
    Copied {
        text: String,
    },
    ClipboardBusy {
        text: String,
    },
    ClipboardFailed {
        text: String,
        message: String,
    },
    Blocked {
        text: Option<String>,
        reason: BlockReason,
    },
    NoSelection,
}

/// Фаза выбора для overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerPhase {
    Idle,
    Choosing,
    ClipboardBusy,
    ClipboardFailed,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrow {
    Left,
    Right,
    Up,
    Down,
}

impl Arrow {
    pub const fn delta(self) -> (i32, i32) {
        match self {
            Self::Left => (-1, 0),
            Self::Right => (1, 0),
            Self::Up => (0, -1),
            Self::Down => (0, 1),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PickerError {
    #[error("не удалось получить кадр: {0}")]
    Capture(#[from] CaptureError),
    #[error("не удалось освободить кадр: {0}")]
    Release(String),
    #[error("кадр недоступен")]
    NoFrame,
    #[error("координата выбора вышла за пределы i32")]
    PointOverflow,
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error(transparent)]
    Sample(#[from] SampleError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartOutcome {
    Started,
    AlreadyActive,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LoupeError {
    #[error("масштаб лупы должен быть больше нуля")]
    ZeroZoom,
    #[error("кадр уже освобождён")]
    NoFrame,
}

/// Окно лупы в координатах исходного снимка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loupe {
    source: FrameRect,
    selected: Point,
    zoom: u32,
}

impl Loupe {
    /// Строит окно вокруг выбранной точки. У края кадра окно сдвигается внутрь,
    /// поэтому выбранный пиксель остаётся в перекрестии.
    pub fn for_point(
        frame: &FrameData,
        selected: Point,
        radius: u32,
        zoom: u32,
    ) -> Result<Self, LoupeError> {
        if zoom == 0 {
            return Err(LoupeError::ZeroZoom);
        }
        let wanted_w = (u64::from(radius) * 2 + 1).min(u64::from(frame.width()));
        let wanted_h = (u64::from(radius) * 2 + 1).min(u64::from(frame.height()));
        let width = wanted_w as u32;
        let height = wanted_h as u32;
        let min_x = i64::from(frame.origin().x);
        let min_y = i64::from(frame.origin().y);
        let max_x = min_x + i64::from(frame.width()) - i64::from(width);
        let max_y = min_y + i64::from(frame.height()) - i64::from(height);
        let left = (i64::from(selected.x) - i64::from(radius)).clamp(min_x, max_x);
        let top = (i64::from(selected.y) - i64::from(radius)).clamp(min_y, max_y);
        Ok(Self {
            source: FrameRect::new(left as i32, top as i32, width, height),
            selected,
            zoom,
        })
    }

    pub fn source(&self) -> FrameRect {
        self.source
    }

    pub fn selected(&self) -> Point {
        self.selected
    }

    pub fn zoom(&self) -> u32 {
        self.zoom
    }

    pub fn width(&self) -> u32 {
        self.source.width.saturating_mul(self.zoom)
    }

    pub fn height(&self) -> u32 {
        self.source.height.saturating_mul(self.zoom)
    }

    /// Координаты перекрестия внутри увеличенного изображения.
    pub fn crosshair(&self) -> Point {
        let offset_x = i64::from(self.selected.x) - i64::from(self.source.x);
        let offset_y = i64::from(self.selected.y) - i64::from(self.source.y);
        let step = i64::from(self.zoom);
        Point::new(
            (offset_x * step + step / 2) as i32,
            (offset_y * step + step / 2) as i32,
        )
    }

    /// Увеличенные пиксели без сглаживания. Каждый исходный пиксель повторяется
    /// `zoom × zoom` раз. Пиксель вне монитора показывается чёрным.
    pub fn render(&self, frame: &FrameData) -> Vec<[u8; 4]> {
        let width = self.width();
        let height = self.height();
        let mut pixels = Vec::with_capacity(width as usize * height as usize);
        for y in 0..height {
            for x in 0..width {
                let source_x = self.source.x + (x / self.zoom) as i32;
                let source_y = self.source.y + (y / self.zoom) as i32;
                let pixel = frame
                    .sample(Point::new(source_x, source_y))
                    .map_or([0, 0, 0, 255], |color| [color.r, color.g, color.b, 255]);
                pixels.push(pixel);
            }
        }
        pixels
    }
}

/// Один открытый выбор. Кадр уже скопирован в память, ссылка хоста освобождена.
pub struct PickerSession {
    frame: Option<FrameData>,
    format: ColorFormat,
    point: Point,
    phase: PickerPhase,
    value: Option<String>,
    sample_error: Option<SampleError>,
    copy_error: Option<String>,
    geometry_changed: bool,
    opened_at: Instant,
    expires_in_ms: u32,
}

impl std::fmt::Debug for PickerSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PickerSession")
            .field("frame_held", &self.frame.is_some())
            .field("format", &self.format)
            .field("point", &self.point)
            .field("phase", &self.phase)
            .field("value", &self.value)
            .field("geometry_changed", &self.geometry_changed)
            .finish()
    }
}

impl PickerSession {
    /// Кадр должен быть уже скопирован, а ссылка `frame.release` — выполнена.
    pub fn new(frame: FrameData, format: ColorFormat, point: Point, expires_in_ms: u32) -> Self {
        let mut session = Self {
            frame: Some(frame),
            format,
            point,
            phase: PickerPhase::Choosing,
            value: None,
            sample_error: None,
            copy_error: None,
            geometry_changed: false,
            opened_at: Instant::now(),
            expires_in_ms,
        };
        session.refresh_value();
        session
    }

    pub fn point(&self) -> Point {
        self.point
    }

    pub fn format(&self) -> ColorFormat {
        self.format
    }

    pub fn value(&self) -> Option<&str> {
        self.value.as_deref()
    }

    pub fn sample_error(&self) -> Option<&SampleError> {
        self.sample_error.as_ref()
    }

    pub fn phase(&self) -> PickerPhase {
        self.phase
    }

    pub fn copy_error(&self) -> Option<&str> {
        self.copy_error.as_deref()
    }

    pub fn geometry_changed(&self) -> bool {
        self.geometry_changed
    }

    pub fn frame_held(&self) -> bool {
        self.frame.is_some()
    }

    /// Копия кадра в памяти. `None` после успешного копирования, Esc или
    /// завершения вызова, когда кадр уже освобождён.
    pub fn frame(&self) -> Option<&FrameData> {
        self.frame.as_ref()
    }

    /// Ссылка хоста освобождена до открытия выбора, поэтому срок её действия
    /// больше не влияет на чтение локальной копии.
    pub fn lease_expired(&self) -> bool {
        self.lease_expired_at(Instant::now())
    }

    pub fn lease_expired_at(&self, now: Instant) -> bool {
        now.duration_since(self.opened_at) >= Duration::from_millis(u64::from(self.expires_in_ms))
    }

    pub fn set_format(&mut self, format: ColorFormat) {
        self.format = format;
        self.refresh_value();
    }

    /// Движение мыши: точка меняется, новый захват не выполняется.
    pub fn set_point(&mut self, point: Point) {
        self.point = point;
        self.copy_error = None;
        if matches!(
            self.phase,
            PickerPhase::ClipboardBusy | PickerPhase::ClipboardFailed
        ) {
            self.phase = PickerPhase::Choosing;
        }
        self.refresh_value();
    }

    /// Стрелка смещает выбор ровно на один физический пиксель.
    pub fn move_arrow(&mut self, arrow: Arrow) -> Result<(), PickerError> {
        let (dx, dy) = arrow.delta();
        let x = self
            .point
            .x
            .checked_add(dx)
            .ok_or(PickerError::PointOverflow)?;
        let y = self
            .point
            .y
            .checked_add(dy)
            .ok_or(PickerError::PointOverflow)?;
        self.set_point(Point::new(x, y));
        Ok(())
    }

    /// Лупа вокруг текущей точки. Окно сдвигается у края снимка.
    pub fn loupe(&self, radius: u32, zoom: u32) -> Result<Loupe, LoupeError> {
        let frame = self.frame.as_ref().ok_or(LoupeError::NoFrame)?;
        Loupe::for_point(frame, self.point, radius, zoom)
    }

    /// Сравнивает актуальную конфигурацию дисплеев с сохранённой. После первого
    /// изменения копирование остаётся запрещённым, даже если геометрия вернулась.
    pub fn refresh_geometry(&mut self, geometry: &ScreenGeometry) -> bool {
        if !self.geometry_changed {
            if let Some(frame) = &self.frame {
                if frame.geometry() != geometry {
                    self.geometry_changed = true;
                }
            }
        }
        self.geometry_changed
    }

    /// Копирует ровно ту строку, которая показана в overlay. При занятом или
    /// недоступном буфере кадр и точка сохраняются для повторной попытки.
    pub fn copy(&mut self, sink: &mut dyn ClipboardSink) -> CopyOutcome {
        let Some(frame) = self.frame.as_ref() else {
            return CopyOutcome::NoSelection;
        };
        if self.geometry_changed {
            return CopyOutcome::Blocked {
                text: self.value.clone(),
                reason: BlockReason::GeometryChanged,
            };
        }
        let color = match frame.sample(self.point) {
            Ok(color) => color,
            Err(error) => {
                self.sample_error = Some(error.clone());
                self.value = None;
                return CopyOutcome::Blocked {
                    text: None,
                    reason: error.into(),
                };
            }
        };
        let text = color.format(self.format);
        self.value = Some(text.clone());
        match sink.write_text(&text) {
            Ok(()) => {
                self.frame = None;
                self.phase = PickerPhase::Completed;
                self.copy_error = None;
                CopyOutcome::Copied { text }
            }
            Err(ClipboardError::Busy) => {
                self.phase = PickerPhase::ClipboardBusy;
                self.copy_error = Some(ClipboardError::Busy.to_string());
                CopyOutcome::ClipboardBusy { text }
            }
            Err(ClipboardError::Other(message)) => {
                self.phase = PickerPhase::ClipboardFailed;
                self.copy_error = Some(message.clone());
                CopyOutcome::ClipboardFailed { text, message }
            }
        }
    }

    /// Esc: буфер не меняется, кадр освобождается.
    pub fn escape(&mut self) -> bool {
        let active = self.frame.take().is_some();
        self.phase = PickerPhase::Cancelled;
        self.copy_error = None;
        active
    }

    /// Завершение вызова: освобождает кадр и сбрасывает состояние.
    pub fn finish(&mut self) -> bool {
        let active = self.frame.take().is_some();
        self.phase = PickerPhase::Idle;
        self.copy_error = None;
        active
    }

    fn refresh_value(&mut self) {
        let Some(frame) = self.frame.as_ref() else {
            self.value = None;
            self.sample_error = None;
            return;
        };
        match frame.sample(self.point) {
            Ok(color) => {
                self.value = Some(color.format(self.format));
                self.sample_error = None;
            }
            Err(error) => {
                self.value = None;
                self.sample_error = Some(error);
            }
        }
    }
}

/// Владелец одного активного выбора. Повторный шорткей во время открытого
/// overlay не делает новый захват.
#[derive(Debug)]
pub struct Picker {
    format: ColorFormat,
    session: Option<PickerSession>,
}

impl Picker {
    pub fn new(format: ColorFormat) -> Self {
        Self {
            format,
            session: None,
        }
    }

    pub fn format(&self) -> ColorFormat {
        self.format
    }

    pub fn set_format(&mut self, format: ColorFormat) {
        self.format = format;
        if let Some(session) = self.session.as_mut() {
            session.set_format(format);
        }
    }

    pub fn session(&self) -> Option<&PickerSession> {
        self.session.as_ref()
    }

    pub fn session_mut(&mut self) -> Option<&mut PickerSession> {
        self.session.as_mut()
    }

    /// Получает один кадр, освобождает ссылку хоста и открывает выбор.
    pub fn start(
        &mut self,
        source: &mut dyn FrameSource,
        cursor: Point,
    ) -> Result<StartOutcome, PickerError> {
        if self.session.as_ref().is_some_and(PickerSession::frame_held) {
            return Ok(StartOutcome::AlreadyActive);
        }
        let mut captured = source.capture()?;
        if let Some(lease) = captured.lease.as_mut() {
            lease.release().map_err(PickerError::Release)?;
        }
        self.session = Some(PickerSession::new(
            captured.data,
            self.format,
            cursor,
            captured.expires_in_ms,
        ));
        Ok(StartOutcome::Started)
    }

    /// Повторная попытка после отказа захвата.
    pub fn retry_capture(
        &mut self,
        source: &mut dyn FrameSource,
        cursor: Point,
    ) -> Result<StartOutcome, PickerError> {
        self.start(source, cursor)
    }

    pub fn copy(&mut self, sink: &mut dyn ClipboardSink) -> CopyOutcome {
        let outcome = match self.session.as_mut() {
            Some(session) => session.copy(sink),
            None => CopyOutcome::NoSelection,
        };
        if matches!(outcome, CopyOutcome::Copied { .. }) {
            self.session = None;
        }
        outcome
    }

    /// Esc закрывает выбор и не трогает буфер. Следующий шорткей начнёт новый
    /// выбор с новым снимком.
    pub fn escape(&mut self) -> bool {
        self.session
            .take()
            .is_some_and(|mut session| session.escape())
    }

    /// Освобождает кадр при завершении вызова или процесса.
    pub fn finish(&mut self) -> bool {
        self.session
            .take()
            .is_some_and(|mut session| session.finish())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use util_maker_sdk::{FrameOrigin, FrameRegion, MonitorGeometry, ScreenGeometry};

    use super::*;
    use crate::BYTES_PER_PIXEL;

    struct TestLease {
        releases: Arc<AtomicUsize>,
    }

    impl FrameLease for TestLease {
        fn release(&mut self) -> Result<(), String> {
            self.releases.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct TestSource {
        captures: Arc<AtomicUsize>,
        releases: Arc<AtomicUsize>,
        results: VecDeque<Result<CapturedFrame, CaptureError>>,
    }

    impl TestSource {
        fn with_results(results: VecDeque<Result<CapturedFrame, CaptureError>>) -> Self {
            Self {
                captures: Arc::new(AtomicUsize::new(0)),
                releases: Arc::new(AtomicUsize::new(0)),
                results,
            }
        }
    }

    impl FrameSource for TestSource {
        fn capture(&mut self) -> Result<CapturedFrame, CaptureError> {
            self.captures.fetch_add(1, Ordering::SeqCst);
            self.results
                .pop_front()
                .unwrap_or(Err(CaptureError::Unavailable("нет кадра".to_owned())))
        }
    }

    #[derive(Default)]
    struct TestClipboard {
        writes: Vec<String>,
        busy: bool,
        fail: Option<String>,
    }

    impl TestClipboard {
        fn ready() -> Self {
            Self::default()
        }

        fn busy() -> Self {
            Self {
                busy: true,
                ..Self::default()
            }
        }
    }

    impl ClipboardSink for TestClipboard {
        fn write_text(&mut self, text: &str) -> Result<(), ClipboardError> {
            if self.busy {
                return Err(ClipboardError::Busy);
            }
            if let Some(message) = &self.fail {
                return Err(ClipboardError::Other(message.clone()));
            }
            self.writes.push(text.to_owned());
            Ok(())
        }
    }

    fn geometry(monitors: &[(i32, i32, u32, u32)]) -> ScreenGeometry {
        let monitor_geometry = monitors
            .iter()
            .map(|(x, y, width, height)| MonitorGeometry {
                rect: FrameRegion {
                    x: *x,
                    y: *y,
                    width: *width,
                    height: *height,
                },
                scale_factor: 1.0,
                primary: false,
            })
            .collect::<Vec<_>>();
        let first = monitor_geometry.first().expect("монитор");
        ScreenGeometry {
            virtual_screen: first.rect,
            monitors: monitor_geometry,
        }
    }

    /// Кадр 5×5 с красным каналом по X, зелёным по Y и синим в правом нижнем
    /// углу. Так видно, что стрелка берёт ровно соседний физический пиксель.
    fn sample_frame() -> FrameData {
        let width = 5u32;
        let height = 5u32;
        let stride = width as usize * BYTES_PER_PIXEL;
        let mut pixels = vec![0u8; stride * height as usize];
        for y in 0..height {
            for x in 0..width {
                let offset = y as usize * stride + x as usize * BYTES_PER_PIXEL;
                pixels[offset] = (x * 10) as u8; // B
                pixels[offset + 1] = (y * 10) as u8; // G
                pixels[offset + 2] = 0x2F; // R
                pixels[offset + 3] = 0xFF;
            }
        }
        FrameData::new(
            pixels,
            width,
            height,
            stride,
            Point::new(0, 0),
            geometry(&[(0, 0, width, height)]),
        )
        .expect("кадр")
    }

    fn frame_with_two_monitors() -> FrameData {
        let width = 8u32;
        let height = 4u32;
        let stride = width as usize * BYTES_PER_PIXEL;
        let pixels = vec![0x7Fu8; stride * height as usize];
        FrameData::new(
            pixels,
            width,
            height,
            stride,
            Point::new(0, 0),
            geometry(&[(0, 0, 4, 4), (5, 0, 3, 4)]),
        )
        .expect("кадр с промежутком")
    }

    fn frame_ref() -> FrameRef {
        FrameRef {
            buffer_id: "frame-1".to_owned(),
            format: FRAME_FORMAT_BGRA8.to_owned(),
            width: 1,
            height: 1,
            stride: 4,
            origin: FrameOrigin { x: 0, y: 0 },
            scale_factor: 1.0,
            expires_in_ms: 5_000,
            pixel_path: None,
            geometry: Some(geometry(&[(0, 0, 1, 1)])),
        }
    }

    fn picker_with_lease() -> (Picker, TestSource) {
        let releases = Arc::new(AtomicUsize::new(0));
        let lease = TestLease {
            releases: releases.clone(),
        };
        let captured = CapturedFrame::with_lease(sample_frame(), 5_000, Box::new(lease));
        let source = TestSource {
            captures: Arc::new(AtomicUsize::new(0)),
            releases,
            results: VecDeque::from([Ok(captured)]),
        };
        (Picker::new(ColorFormat::Hex), source)
    }

    #[test]
    fn capture_once_releases_lease_and_pointer_moves_do_not_capture() {
        let (mut picker, mut source) = picker_with_lease();
        assert_eq!(
            picker.start(&mut source, Point::new(1, 1)).unwrap(),
            StartOutcome::Started
        );
        assert_eq!(source.captures.load(Ordering::SeqCst), 1);
        assert_eq!(source.releases.load(Ordering::SeqCst), 1);
        let session = picker.session().expect("сессия");
        assert_eq!(session.value(), Some("#2F0A0A"));
        assert_eq!(session.point(), Point::new(1, 1));

        picker.session_mut().unwrap().set_point(Point::new(3, 2));
        assert_eq!(source.captures.load(Ordering::SeqCst), 1);
        assert_eq!(picker.session().unwrap().value(), Some("#2F141E"));
        assert_eq!(
            picker.start(&mut source, Point::new(0, 0)).unwrap(),
            StartOutcome::AlreadyActive
        );
        assert_eq!(source.captures.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn rgba_rows_drop_stride_padding_and_start_from_top_row() {
        let width = 2u32;
        let height = 2u32;
        let stride = width as usize * BYTES_PER_PIXEL + 2;
        let mut pixels = vec![0u8; stride * height as usize];
        // (0, 0): B=1 G=2 R=3, (1, 0): B=4 G=5 R=6.
        pixels[0..4].copy_from_slice(&[1, 2, 3, 0]);
        pixels[4..8].copy_from_slice(&[4, 5, 6, 0]);
        pixels[8..10].copy_from_slice(&[0xAA, 0xBB]);
        let second = stride;
        pixels[second..second + 4].copy_from_slice(&[7, 8, 9, 0]);
        pixels[second + 4..second + 8].copy_from_slice(&[10, 11, 12, 0]);
        let frame = FrameData::new(
            pixels,
            width,
            height,
            stride,
            Point::new(0, 0),
            geometry(&[(0, 0, width, height)]),
        )
        .expect("кадр");

        assert_eq!(
            frame.rgba_rows(),
            vec![3, 2, 1, 255, 6, 5, 4, 255, 9, 8, 7, 255, 12, 11, 10, 255]
        );
    }

    #[test]
    fn arrows_move_exactly_one_physical_pixel() {
        let mut session =
            PickerSession::new(sample_frame(), ColorFormat::Hex, Point::new(1, 1), 5_000);
        session.move_arrow(Arrow::Right).unwrap();
        assert_eq!(session.point(), Point::new(2, 1));
        session.move_arrow(Arrow::Down).unwrap();
        assert_eq!(session.point(), Point::new(2, 2));
        session.move_arrow(Arrow::Left).unwrap();
        assert_eq!(session.point(), Point::new(1, 2));
        session.move_arrow(Arrow::Up).unwrap();
        assert_eq!(session.point(), Point::new(1, 1));
        assert_eq!(session.value(), Some("#2F0A0A"));
    }

    #[test]
    fn loupe_uses_nearest_neighbor_and_clamps_at_edge() {
        let frame = sample_frame();
        let loupe = Loupe::for_point(&frame, Point::new(0, 0), 2, 4).expect("лупа");
        assert_eq!(loupe.source(), FrameRect::new(0, 0, 5, 5));
        assert_eq!(loupe.crosshair(), Point::new(2, 2));
        let pixels = loupe.render(&frame);
        assert_eq!(pixels.len(), 20 * 20);
        // Один исходный пиксель занимает блок 4×4 без смешивания.
        assert_eq!(pixels[0], [0x2F, 0, 0, 255]);
        assert_eq!(pixels[3], [0x2F, 0, 0, 255]);
        assert_eq!(pixels[4], [0x2F, 0, 0x0A, 255]);

        let edge = Loupe::for_point(&frame, Point::new(4, 4), 2, 4).expect("лупа у края");
        assert_eq!(edge.source(), FrameRect::new(0, 0, 5, 5));
        assert_eq!(edge.crosshair(), Point::new(18, 18));
    }

    #[test]
    fn copy_success_closes_selection_and_uses_displayed_text() {
        let (mut picker, mut source) = picker_with_lease();
        picker.start(&mut source, Point::new(1, 1)).unwrap();
        let mut clipboard = TestClipboard::ready();
        assert_eq!(
            picker.copy(&mut clipboard),
            CopyOutcome::Copied {
                text: "#2F0A0A".to_owned()
            }
        );
        assert_eq!(clipboard.writes, ["#2F0A0A"]);
        assert!(picker.session().is_none());
    }

    #[test]
    fn busy_clipboard_keeps_point_and_retry_copies_same_value() {
        let (mut picker, mut source) = picker_with_lease();
        picker.start(&mut source, Point::new(1, 1)).unwrap();
        let mut clipboard = TestClipboard::busy();
        assert_eq!(
            picker.copy(&mut clipboard),
            CopyOutcome::ClipboardBusy {
                text: "#2F0A0A".to_owned()
            }
        );
        let session = picker.session().expect("сессия сохранена");
        assert_eq!(session.point(), Point::new(1, 1));
        assert_eq!(session.value(), Some("#2F0A0A"));
        assert!(session.frame_held());
        assert_eq!(session.phase(), PickerPhase::ClipboardBusy);

        clipboard.busy = false;
        assert_eq!(
            picker.copy(&mut clipboard),
            CopyOutcome::Copied {
                text: "#2F0A0A".to_owned()
            }
        );
        assert_eq!(clipboard.writes, ["#2F0A0A"]);
    }

    #[test]
    fn escape_preserves_clipboard_and_frees_frame() {
        let (mut picker, mut source) = picker_with_lease();
        picker.start(&mut source, Point::new(1, 1)).unwrap();
        let clipboard = TestClipboard::ready();
        assert!(picker.escape());
        assert!(clipboard.writes.is_empty());
        assert!(picker.session().is_none());
    }

    #[test]
    fn selection_survives_lease_expiration() {
        let mut session = PickerSession::new(sample_frame(), ColorFormat::Hex, Point::new(1, 1), 0);
        assert!(session.lease_expired_at(session.opened_at + Duration::from_millis(1)));
        let mut clipboard = TestClipboard::ready();
        assert_eq!(
            session.copy(&mut clipboard),
            CopyOutcome::Copied {
                text: "#2F0A0A".to_owned()
            }
        );
    }

    #[test]
    fn geometry_change_blocks_copy_until_finish() {
        let mut session =
            PickerSession::new(sample_frame(), ColorFormat::Hex, Point::new(1, 1), 5_000);
        let changed = geometry(&[(0, 0, 5, 5), (5, 0, 5, 5)]);
        assert!(session.refresh_geometry(&changed));
        let mut clipboard = TestClipboard::ready();
        assert_eq!(
            session.copy(&mut clipboard),
            CopyOutcome::Blocked {
                text: Some("#2F0A0A".to_owned()),
                reason: BlockReason::GeometryChanged
            }
        );
        assert!(clipboard.writes.is_empty());
        assert!(session.refresh_geometry(&geometry(&[(0, 0, 5, 5)])));
        assert!(session.geometry_changed());
    }

    #[test]
    fn missing_geometry_or_gap_blocks_copy() {
        let mut no_geometry = frame_ref();
        no_geometry.geometry = None;
        assert_eq!(
            FrameData::from_frame_ref(&no_geometry, vec![0, 0, 0, 255]),
            Err(FrameError::GeometryUnavailable)
        );

        let mut session = PickerSession::new(
            frame_with_two_monitors(),
            ColorFormat::Hex,
            Point::new(4, 1),
            5_000,
        );
        let mut clipboard = TestClipboard::ready();
        assert_eq!(
            session.copy(&mut clipboard),
            CopyOutcome::Blocked {
                text: None,
                reason: BlockReason::OutsideMonitors
            }
        );
        assert!(clipboard.writes.is_empty());
    }

    #[test]
    fn capture_failure_can_be_retried() {
        let mut source = TestSource::with_results(VecDeque::from([
            Err(CaptureError::Unavailable("нет экрана".to_owned())),
            Ok(CapturedFrame::new(sample_frame(), 5_000)),
        ]));
        let mut picker = Picker::new(ColorFormat::Hex);
        assert_eq!(
            picker.start(&mut source, Point::new(1, 1)),
            Err(PickerError::Capture(CaptureError::Unavailable(
                "нет экрана".to_owned()
            )))
        );
        assert_eq!(
            picker.retry_capture(&mut source, Point::new(1, 1)).unwrap(),
            StartOutcome::Started
        );
        assert_eq!(source.captures.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn rgb_format_is_used_for_display_and_copy() {
        let mut session =
            PickerSession::new(sample_frame(), ColorFormat::CssRgb, Point::new(1, 1), 5_000);
        assert_eq!(session.value(), Some("rgb(47, 10, 10)"));
        let mut clipboard = TestClipboard::ready();
        assert_eq!(
            session.copy(&mut clipboard),
            CopyOutcome::Copied {
                text: "rgb(47, 10, 10)".to_owned()
            }
        );
        assert_eq!(clipboard.writes, ["rgb(47, 10, 10)"]);
    }

    #[test]
    fn clipboard_other_error_keeps_selection_for_retry() {
        let mut session =
            PickerSession::new(sample_frame(), ColorFormat::Hex, Point::new(1, 1), 5_000);
        let mut clipboard = TestClipboard {
            fail: Some("буфер занят другим приложением".to_owned()),
            ..TestClipboard::default()
        };
        assert!(matches!(
            session.copy(&mut clipboard),
            CopyOutcome::ClipboardFailed { .. }
        ));
        assert!(session.frame_held());
        assert_eq!(session.phase(), PickerPhase::ClipboardFailed);
        clipboard.fail = None;
        assert!(matches!(
            session.copy(&mut clipboard),
            CopyOutcome::Copied { .. }
        ));
    }
}
