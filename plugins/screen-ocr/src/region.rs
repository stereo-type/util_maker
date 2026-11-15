//! Состояние выбора области OCR без привязки к окну.
//!
//! Кадр копируется в память до открытия выбора, поэтому истечение ссылки
//! `expiresInMs` не мешает продолжить работу. Все координаты — физические
//! пиксели виртуального рабочего стола; геометрия мониторов хранится рядом с
//! пикселями и используется для проверки положения указателя.

use util_maker_sdk::{FrameOrigin, FrameRef, MonitorGeometry, ScreenGeometry, FRAME_FORMAT_BGRA8};

use crate::{BgraFrame, CropError, GrayImage, Selection};

/// Точка в физических координатах виртуального рабочего стола.
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

/// Фаза выбора области.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionPhase {
    Idle,
    Selecting,
    Selected,
    Cancelled,
}

/// Результат обновления указателя. У вырожденного прямоугольника `selection`
/// равен `None`, а фаза остаётся `Selecting`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionUpdate {
    pub phase: SelectionPhase,
    pub selection: Option<Selection>,
}

/// Исход повторного вызова действия.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartOutcome {
    Started,
    AlreadyActive,
}

/// Ошибка подготовки кадра или выбора области.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegionError {
    #[error(transparent)]
    Frame(#[from] CropError),
    #[error("в ответе кадра нет геометрии мониторов")]
    GeometryUnavailable,
    #[error("кадр ещё не получен")]
    NoFrame,
    #[error("выделение ещё не начато")]
    NoSelection,
    #[error("выделение пустое")]
    EmptySelection,
    #[error("координата выходит за пределы i32")]
    PointOverflow,
}

/// Проверенный снимок BGRA8, которым владеет плагин.
#[derive(Debug, Clone, PartialEq)]
pub struct RegionFrame {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
    stride: usize,
    origin: Point,
    geometry: ScreenGeometry,
    expires_in_ms: u32,
}

impl RegionFrame {
    pub fn new(
        pixels: Vec<u8>,
        width: u32,
        height: u32,
        stride: usize,
        origin: Point,
        geometry: ScreenGeometry,
        expires_in_ms: u32,
    ) -> Result<Self, RegionError> {
        if geometry.monitors.is_empty() {
            return Err(RegionError::GeometryUnavailable);
        }
        BgraFrame::new(&pixels, width, height, stride, sdk_origin(origin))?;
        Ok(Self {
            pixels,
            width,
            height,
            stride,
            origin,
            geometry,
            expires_in_ms,
        })
    }

    /// Создаёт владеющую копию из ответа `frame.capture`.
    pub fn from_frame_ref(frame: &FrameRef, pixels: Vec<u8>) -> Result<Self, RegionError> {
        if frame.format != FRAME_FORMAT_BGRA8 {
            return Err(CropError::UnsupportedFormat {
                actual: frame.format.clone(),
            }
            .into());
        }
        let geometry = frame
            .geometry
            .clone()
            .ok_or(RegionError::GeometryUnavailable)?;
        Self::new(
            pixels,
            frame.width,
            frame.height,
            usize::try_from(frame.stride).unwrap_or(usize::MAX),
            Point::new(frame.origin.x, frame.origin.y),
            geometry,
            frame.expires_in_ms,
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

    pub fn expires_in_ms(&self) -> u32 {
        self.expires_in_ms
    }

    pub fn monitors(&self) -> &[MonitorGeometry] {
        &self.geometry.monitors
    }

    pub fn bounds(&self) -> Selection {
        Selection::new(self.origin.x, self.origin.y, self.width, self.height)
    }

    /// Монитор, которому принадлежит физическая точка.
    pub fn monitor_at(&self, point: Point) -> Option<&MonitorGeometry> {
        self.geometry
            .monitors
            .iter()
            .find(|monitor| contains(monitor, point))
    }

    /// Прижимает точку к границам снимка. Указатель может уйти за край при
    /// быстром движении; выбор всё равно остаётся внутри кадра.
    pub fn clamp(&self, point: Point) -> Point {
        let max_x = i64::from(self.origin.x) + i64::from(self.width) - 1;
        let max_y = i64::from(self.origin.y) + i64::from(self.height) - 1;
        Point::new(
            i64::from(point.x).clamp(i64::from(self.origin.x), max_x) as i32,
            i64::from(point.y).clamp(i64::from(self.origin.y), max_y) as i32,
        )
    }

    /// Вырезает область по сохранённой копии кадра.
    pub fn crop(&self, selection: Selection) -> Result<GrayImage, CropError> {
        BgraFrame::new(
            &self.pixels,
            self.width,
            self.height,
            self.stride,
            sdk_origin(self.origin),
        )?
        .crop(selection)
    }
}

/// Один активный выбор области.
#[derive(Debug, Clone, PartialEq)]
pub struct RegionSession {
    frame: RegionFrame,
    call_id: String,
    phase: SelectionPhase,
    anchor: Option<Point>,
    cursor: Option<Point>,
}

impl RegionSession {
    pub fn new(frame: RegionFrame, call_id: impl Into<String>) -> Self {
        Self {
            frame,
            call_id: call_id.into(),
            phase: SelectionPhase::Selecting,
            anchor: None,
            cursor: None,
        }
    }

    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    pub fn frame(&self) -> &RegionFrame {
        &self.frame
    }

    pub fn phase(&self) -> SelectionPhase {
        self.phase
    }

    /// Начинает жест в физической точке. Начальная точка ещё не образует
    /// прямоугольник, поэтому фаза остаётся `Selecting`.
    pub fn begin(&mut self, anchor: Point) -> Result<SelectionUpdate, RegionError> {
        let anchor = self.frame.clamp(anchor);
        self.anchor = Some(anchor);
        self.cursor = Some(anchor);
        self.phase = SelectionPhase::Selecting;
        Ok(self.update())
    }

    /// Двигает указатель и нормализует прямоугольник независимо от направления.
    pub fn move_to(&mut self, cursor: Point) -> Result<SelectionUpdate, RegionError> {
        let anchor = self.anchor.ok_or(RegionError::NoSelection)?;
        let cursor = self.frame.clamp(cursor);
        self.cursor = Some(cursor);
        self.refresh(anchor, cursor)
    }

    pub fn set_selection(
        &mut self,
        anchor: Point,
        cursor: Point,
    ) -> Result<SelectionUpdate, RegionError> {
        let anchor = self.frame.clamp(anchor);
        let cursor = self.frame.clamp(cursor);
        self.anchor = Some(anchor);
        self.cursor = Some(cursor);
        self.refresh(anchor, cursor)
    }

    /// Принимает готовый прямоугольник от overlay и проверяет его по кадру.
    pub fn select_rect(&mut self, selection: Selection) -> Result<SelectionUpdate, RegionError> {
        if selection.width == 0 || selection.height == 0 {
            self.anchor = Some(Point::new(selection.x, selection.y));
            self.cursor = Some(Point::new(selection.x, selection.y));
            self.phase = SelectionPhase::Selecting;
            return Ok(self.update());
        }
        self.frame.crop(selection)?;
        let right = i64::from(selection.x) + i64::from(selection.width);
        let bottom = i64::from(selection.y) + i64::from(selection.height);
        self.anchor = Some(Point::new(selection.x, selection.y));
        self.cursor = Some(Point::new(
            i32::try_from(right).map_err(|_| RegionError::PointOverflow)?,
            i32::try_from(bottom).map_err(|_| RegionError::PointOverflow)?,
        ));
        self.phase = SelectionPhase::Selected;
        Ok(self.update())
    }

    pub fn raw_selection(&self) -> Option<Selection> {
        let anchor = self.anchor?;
        let cursor = self.cursor?;
        Some(normalized(anchor, cursor))
    }

    /// Невырожденный прямоугольник, готовый к распознаванию.
    pub fn selection(&self) -> Option<Selection> {
        self.raw_selection()
            .filter(|selection| selection.width > 0 && selection.height > 0)
    }

    /// Подтверждает выбор. Вырожденный прямоугольник не закрывает сессию.
    pub fn finish(&mut self) -> Result<Selection, RegionError> {
        let selection = self.selection().ok_or(RegionError::EmptySelection)?;
        self.frame.crop(selection)?;
        self.phase = SelectionPhase::Selected;
        Ok(selection)
    }

    pub fn cancel(&mut self) {
        self.phase = SelectionPhase::Cancelled;
        self.anchor = None;
        self.cursor = None;
    }

    fn refresh(&mut self, anchor: Point, cursor: Point) -> Result<SelectionUpdate, RegionError> {
        let selection = normalized(anchor, cursor);
        if selection.width == 0 || selection.height == 0 {
            self.phase = SelectionPhase::Selecting;
            return Ok(self.update());
        }
        self.frame.crop(selection)?;
        self.phase = SelectionPhase::Selected;
        Ok(self.update())
    }

    fn update(&self) -> SelectionUpdate {
        SelectionUpdate {
            phase: self.phase,
            selection: self.selection(),
        }
    }
}

/// Владелец текущей сессии выбора.
#[derive(Debug, Default, PartialEq)]
pub struct RegionPicker {
    session: Option<RegionSession>,
}

impl RegionPicker {
    pub fn new() -> Self {
        Self { session: None }
    }

    /// Повторный вызов не делает новый снимок и не сбрасывает текущий выбор.
    pub fn start(&mut self, frame: RegionFrame, call_id: impl Into<String>) -> StartOutcome {
        if self.session.is_some() {
            return StartOutcome::AlreadyActive;
        }
        self.session = Some(RegionSession::new(frame, call_id));
        StartOutcome::Started
    }

    pub fn session(&self) -> Option<&RegionSession> {
        self.session.as_ref()
    }

    pub fn session_mut(&mut self) -> Option<&mut RegionSession> {
        self.session.as_mut()
    }

    pub fn repeat_shortcut(&self) -> bool {
        self.session.is_some()
    }

    /// Esc завершает выбор и освобождает владеющую копию кадра.
    pub fn cancel(&mut self, call_id: &str) -> bool {
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.call_id() == call_id)
        {
            self.session = None;
            true
        } else {
            false
        }
    }

    pub fn cancel_any(&mut self) -> bool {
        self.session.take().is_some()
    }

    pub fn finish(&mut self) -> Result<Selection, RegionError> {
        self.session.as_mut().ok_or(RegionError::NoFrame)?.finish()
    }
}

fn sdk_origin(point: Point) -> FrameOrigin {
    FrameOrigin {
        x: point.x,
        y: point.y,
    }
}

fn contains(monitor: &MonitorGeometry, point: Point) -> bool {
    if monitor.rect.width == 0 || monitor.rect.height == 0 {
        return false;
    }
    let x = i64::from(point.x);
    let y = i64::from(point.y);
    let left = i64::from(monitor.rect.x);
    let top = i64::from(monitor.rect.y);
    x >= left
        && y >= top
        && x < left + i64::from(monitor.rect.width)
        && y < top + i64::from(monitor.rect.height)
}

fn normalized(anchor: Point, cursor: Point) -> Selection {
    let left = anchor.x.min(cursor.x);
    let top = anchor.y.min(cursor.y);
    let width = (i64::from(anchor.x) - i64::from(cursor.x)).unsigned_abs();
    let height = (i64::from(anchor.y) - i64::from(cursor.y)).unsigned_abs();
    Selection::new(
        left,
        top,
        u32::try_from(width).unwrap_or(u32::MAX),
        u32::try_from(height).unwrap_or(u32::MAX),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use util_maker_sdk::{FrameRegion, MonitorGeometry};

    fn frame(expires_in_ms: u32) -> RegionFrame {
        let width = 200;
        let height = 100;
        let stride = width as usize * 4 + 8;
        let pixels = vec![0x40u8; stride * height as usize];
        RegionFrame::new(
            pixels,
            width,
            height,
            stride,
            Point::new(-100, -50),
            ScreenGeometry {
                virtual_screen: FrameRegion {
                    x: -100,
                    y: -50,
                    width: 200,
                    height: 100,
                },
                monitors: vec![
                    MonitorGeometry {
                        rect: FrameRegion {
                            x: -100,
                            y: -50,
                            width: 100,
                            height: 100,
                        },
                        scale_factor: 1.0,
                        primary: false,
                    },
                    MonitorGeometry {
                        rect: FrameRegion {
                            x: 0,
                            y: -50,
                            width: 100,
                            height: 100,
                        },
                        scale_factor: 1.5,
                        primary: true,
                    },
                ],
            },
            expires_in_ms,
        )
        .expect("кадр")
    }

    #[test]
    fn reverse_selection_is_normalized_across_two_monitors() {
        let mut picker = RegionPicker::new();
        assert_eq!(picker.start(frame(5_000), "call-1"), StartOutcome::Started);
        let session = picker.session_mut().expect("сессия");

        assert_eq!(session.begin(Point::new(80, 20)).unwrap().selection, None);
        let update = session.move_to(Point::new(-80, -20)).expect("выбор");
        assert_eq!(update.phase, SelectionPhase::Selected);
        assert_eq!(update.selection, Some(Selection::new(-80, -20, 160, 40)));
        assert!(session.frame().monitor_at(Point::new(-80, -20)).is_some());
        assert!(session.frame().monitor_at(Point::new(80, 20)).is_some());
        session
            .frame()
            .crop(update.selection.unwrap())
            .expect("обрезка");
    }

    #[test]
    fn degenerate_selection_keeps_the_gesture_open() {
        let mut picker = RegionPicker::new();
        picker.start(frame(5_000), "call-1");
        let session = picker.session_mut().expect("сессия");

        session.begin(Point::new(10, 10)).expect("начало");
        let update = session.move_to(Point::new(10, 30)).expect("вертикаль");
        assert_eq!(update.phase, SelectionPhase::Selecting);
        assert_eq!(update.selection, None);
        assert_eq!(session.finish(), Err(RegionError::EmptySelection));

        let update = session.move_to(Point::new(30, 30)).expect("прямоугольник");
        assert_eq!(update.phase, SelectionPhase::Selected);
        assert_eq!(update.selection, Some(Selection::new(10, 10, 20, 20)));
    }

    #[test]
    fn explicit_rectangle_from_overlay_is_validated() {
        let mut picker = RegionPicker::new();
        picker.start(frame(5_000), "call-1");
        let session = picker.session_mut().expect("сессия");

        let update = session
            .select_rect(Selection::new(-100, -50, 200, 100))
            .expect("выбор");
        assert_eq!(update.phase, SelectionPhase::Selected);
        assert_eq!(update.selection, Some(Selection::new(-100, -50, 200, 100)));
        assert_eq!(
            session.select_rect(Selection::new(150, 0, 10, 10)),
            Err(RegionError::Frame(CropError::OutsideFrame))
        );
    }

    #[test]
    fn repeat_shortcut_activates_the_same_session() {
        let mut picker = RegionPicker::new();
        assert_eq!(picker.start(frame(5_000), "call-1"), StartOutcome::Started);
        assert_eq!(
            picker.start(frame(5_000), "call-2"),
            StartOutcome::AlreadyActive
        );
        assert_eq!(picker.session().unwrap().call_id(), "call-1");
        assert!(picker.repeat_shortcut());
    }

    #[test]
    fn escape_drops_the_owned_frame_and_allows_a_new_capture() {
        let mut picker = RegionPicker::new();
        picker.start(frame(5_000), "call-1");
        assert!(picker.cancel("call-1"));
        assert!(!picker.repeat_shortcut());
        assert_eq!(picker.start(frame(5_000), "call-2"), StartOutcome::Started);
        assert_eq!(picker.session().unwrap().call_id(), "call-2");
    }

    #[test]
    fn selection_still_works_after_the_lease_expires() {
        let mut picker = RegionPicker::new();
        picker.start(frame(1), "call-1");
        std::thread::sleep(std::time::Duration::from_millis(10));
        let session = picker.session_mut().expect("сессия");
        session.begin(Point::new(0, 0)).expect("начало");
        let update = session.move_to(Point::new(50, 50)).expect("выбор");
        assert_eq!(update.phase, SelectionPhase::Selected);
        // Нижняя граница кадра — y = 49, поэтому указатель прижат к ней.
        assert_eq!(update.selection, Some(Selection::new(0, 0, 50, 49)));
        assert!(picker.finish().is_ok());
    }

    #[test]
    fn frame_requires_monitor_geometry_and_bgra8() {
        let mut frame_ref = FrameRef {
            buffer_id: "frame-1".to_owned(),
            format: FRAME_FORMAT_BGRA8.to_owned(),
            width: 2,
            height: 1,
            stride: 8,
            origin: FrameOrigin { x: 0, y: 0 },
            scale_factor: 1.0,
            expires_in_ms: 5_000,
            geometry: None,
            pixel_path: None,
        };
        assert_eq!(
            RegionFrame::from_frame_ref(&frame_ref, vec![0; 8]),
            Err(RegionError::GeometryUnavailable)
        );
        frame_ref.format = "rgba8".to_owned();
        frame_ref.geometry = Some(ScreenGeometry {
            virtual_screen: FrameRegion {
                x: 0,
                y: 0,
                width: 2,
                height: 1,
            },
            monitors: vec![MonitorGeometry {
                rect: FrameRegion {
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 1,
                },
                scale_factor: 1.0,
                primary: true,
            }],
        });
        assert_eq!(
            RegionFrame::from_frame_ref(&frame_ref, vec![0; 8]),
            Err(RegionError::Frame(CropError::UnsupportedFormat {
                actual: "rgba8".to_owned()
            }))
        );
    }
}
