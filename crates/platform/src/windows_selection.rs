//! Чтение выделения Windows через UI Automation.
//!
//! Захват фиксирует корневое окно, процесс и точку внутри исходного элемента.
//! Поэтому меню трея и окна хоста не подменяют источник: чтение продолжает
//! работать с тем же окном и той же точкой. Пароль проверяется по свойству
//! `IsPassword` и его предкам до любого `GetText`. Ожидание подписывается на
//! `TextSelectionChanged` и снимает обработчик на любом исходе.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use windows::core::{implement, Ref};
use windows::Win32::Foundation::{HWND, POINT, RECT, RPC_E_CHANGED_MODE, VARIANT_BOOL};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::Ole::{
    SafeArrayAccessData, SafeArrayDestroy, SafeArrayGetLBound, SafeArrayGetUBound,
    SafeArrayUnaccessData,
};
use windows::Win32::System::Variant::{VARIANT, VT_BOOL};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationCacheRequest, IUIAutomationElement,
    IUIAutomationEventHandler, IUIAutomationEventHandler_Impl, IUIAutomationTextPattern,
    IUIAutomationTextPattern2, IUIAutomationTextRange, TreeScope_Descendants, TreeScope_Element,
    UIA_IsTextPatternAvailablePropertyId, UIA_TextPattern2Id, UIA_TextPatternId,
    UIA_Text_TextSelectionChangedEventId, UIA_EVENT_ID,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetCursorPos, GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId,
    IsWindow, GA_ROOT,
};

use crate::native_capture::ensure_dpi_awareness;
use crate::selection::{
    NativeCapture, ScreenPoint, SelectionBackend, SelectionError, SelectionEvent, SelectionState,
};
use util_maker_sdk::FrameRegion;

const MAX_PARENT_WALK: usize = 12;
const WAIT_POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsSelection;

#[derive(Debug, Clone)]
pub struct WindowsCapture {
    hwnd: HWND,
    process_id: u32,
    focus_point: POINT,
}

impl SelectionBackend for WindowsSelection {
    type Capture = WindowsCapture;

    fn capture_foreground(&mut self) -> Result<NativeCapture<Self::Capture>, SelectionError> {
        ensure_dpi_awareness();
        let _com = ComGuard::new()?;
        let automation = create_automation()?;
        let hwnd = safe_root(unsafe { GetForegroundWindow() });
        if hwnd.0.is_null() || unsafe { IsWindow(Some(hwnd)) }.0 == 0 {
            return Err(SelectionError::SourceLost);
        }
        let window = window_rect(hwnd).ok_or(SelectionError::SourceLost)?;
        let mut process_id = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut process_id)) };
        if process_id == 0 {
            return Err(SelectionError::SourceLost);
        }
        let focus_point = focus_point(&automation, process_id, window);
        Ok(NativeCapture {
            window,
            process_id,
            native: WindowsCapture {
                hwnd,
                process_id,
                focus_point,
            },
        })
    }

    fn read(&mut self, capture: &mut Self::Capture) -> Result<SelectionState, SelectionError> {
        let _com = ComGuard::new()?;
        let automation = create_automation()?;
        if !window_alive(capture) {
            return Ok(SelectionState::SourceLost);
        }
        let Some(element) = source_element(&automation, capture)? else {
            return Ok(SelectionState::Unsupported {
                bounds: capture_window(capture),
            });
        };
        let bounds = element_bounds(&element).unwrap_or_else(|| capture_window(capture));
        if password_near(&automation, &element) {
            return Ok(SelectionState::Password { bounds });
        }
        let Some(pattern) = text_pattern(&element) else {
            return Ok(SelectionState::Unsupported { bounds });
        };
        let ranges = unsafe { pattern.GetSelection() }
            .map_err(|error| automation_error("GetSelection", error))?;
        let count = unsafe { ranges.Length() }.unwrap_or(0);
        for index in 0..count.max(0) {
            let range = unsafe { ranges.GetElement(index) }
                .map_err(|error| automation_error("GetElement", error))?;
            let text = unsafe { range.GetText(-1) }
                .map_err(|error| automation_error("GetText", error))?
                .to_string();
            if text.is_empty() {
                continue;
            }
            let bounds = range_bounds(&range)
                .filter(valid_region)
                .or_else(|| element_bounds(&element))
                .unwrap_or_else(|| capture_window(capture));
            return Ok(SelectionState::Text { text, bounds });
        }
        Ok(SelectionState::Empty {
            caret: caret_point(&element).unwrap_or_else(|| region_center(bounds)),
        })
    }

    fn wait_for_change(
        &mut self,
        capture: &mut Self::Capture,
        cancel: &AtomicBool,
        timeout: Duration,
    ) -> Result<SelectionEvent, SelectionError> {
        let _com = ComGuard::new()?;
        let automation = create_automation()?;
        if !window_alive(capture) {
            return Ok(SelectionEvent::SourceLost);
        }
        let Some(target) = source_element(&automation, capture)? else {
            return Ok(SelectionEvent::SourceChanged);
        };
        let subscribed = if text_pattern(&target).is_some() {
            let (sender, receiver) = mpsc::channel();
            let handler: IUIAutomationEventHandler = SelectionEventHandler::new(sender).into();
            let added = unsafe {
                automation.AddAutomationEventHandler(
                    UIA_Text_TextSelectionChangedEventId,
                    &target,
                    TreeScope_Element,
                    None::<&IUIAutomationCacheRequest>,
                    &handler,
                )
            };
            added.map(|_| (receiver, handler)).ok()
        } else {
            None
        };
        let Some((receiver, handler)) = subscribed else {
            return Ok(poll_change(capture, cancel, timeout));
        };

        let deadline = Instant::now() + timeout;
        let outcome = loop {
            if cancel.load(Ordering::SeqCst) || escape_pressed() {
                break SelectionEvent::Cancelled;
            }
            if !window_alive(capture) {
                break SelectionEvent::SourceLost;
            }
            if safe_root(unsafe { GetForegroundWindow() }) != capture.hwnd {
                break SelectionEvent::SourceChanged;
            }
            let now = Instant::now();
            if now >= deadline {
                break SelectionEvent::TimedOut;
            }
            match receiver.recv_timeout((deadline - now).min(WAIT_POLL)) {
                Ok(()) => break SelectionEvent::Changed,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(SelectionError::Automation(
                        "обработчик UI Automation отключился".to_owned(),
                    ))
                }
            }
        };
        let _ = unsafe {
            automation.RemoveAutomationEventHandler(
                UIA_Text_TextSelectionChangedEventId,
                &target,
                &handler,
            )
        };
        Ok(outcome)
    }

    fn release(&mut self, _capture: Self::Capture) {
        // Подписки живут только внутри wait_for_change и снимаются до возврата.
    }
}

#[implement(IUIAutomationEventHandler)]
struct SelectionEventHandler {
    sender: mpsc::Sender<()>,
}

impl SelectionEventHandler {
    fn new(sender: mpsc::Sender<()>) -> Self {
        Self { sender }
    }
}

impl IUIAutomationEventHandler_Impl for SelectionEventHandler_Impl {
    fn HandleAutomationEvent(
        &self,
        _sender: Ref<IUIAutomationElement>,
        _event_id: UIA_EVENT_ID,
    ) -> windows::core::Result<()> {
        let _ = self.sender.send(());
        Ok(())
    }
}

struct ComGuard {
    owns: bool,
}

impl ComGuard {
    fn new() -> Result<Self, SelectionError> {
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if result.is_ok() {
            Ok(Self { owns: true })
        } else if result == RPC_E_CHANGED_MODE {
            // Поток уже инициализирован в другом apartment-режиме. UI Automation
            // можно использовать и в STA, главное не вызывать CoUninitialize.
            Ok(Self { owns: false })
        } else {
            Err(SelectionError::Automation(format!(
                "CoInitializeEx: {}",
                result.message()
            )))
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.owns {
            unsafe { CoUninitialize() };
        }
    }
}

fn create_automation() -> Result<IUIAutomation, SelectionError> {
    let automation: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
            .map_err(|error| automation_error("CoCreateInstance", error))?;
    Ok(automation)
}

fn automation_error(operation: &str, error: windows::core::Error) -> SelectionError {
    SelectionError::Automation(format!("{operation}: {error}"))
}

fn safe_root(hwnd: HWND) -> HWND {
    if hwnd.0.is_null() {
        return hwnd;
    }
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    if root.0.is_null() {
        hwnd
    } else {
        root
    }
}

fn window_alive(capture: &WindowsCapture) -> bool {
    !capture.hwnd.0.is_null()
        && unsafe { IsWindow(Some(capture.hwnd)) }.0 != 0
        && safe_root(capture.hwnd) == capture.hwnd
}

fn window_rect(hwnd: HWND) -> Option<FrameRegion> {
    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }.ok()?;
    rect_region(rect)
}

fn capture_window(capture: &WindowsCapture) -> FrameRegion {
    window_rect(capture.hwnd).unwrap_or(FrameRegion {
        x: capture.focus_point.x,
        y: capture.focus_point.y,
        width: 1,
        height: 1,
    })
}

fn focus_point(automation: &IUIAutomation, process_id: u32, window: FrameRegion) -> POINT {
    if let Ok(focused) = unsafe { automation.GetFocusedElement() } {
        if element_process(&focused) == Some(process_id) {
            if let Ok(rect) = unsafe { focused.CurrentBoundingRectangle() } {
                if let Some(region) = rect_region(rect) {
                    let center = region_center(region);
                    if contains(window, center) {
                        return POINT {
                            x: center.x,
                            y: center.y,
                        };
                    }
                }
            }
        }
    }
    let mut cursor = POINT::default();
    if unsafe { GetCursorPos(&mut cursor) }.is_ok()
        && contains(
            window,
            ScreenPoint {
                x: cursor.x,
                y: cursor.y,
            },
        )
    {
        return cursor;
    }
    let center = region_center(window);
    POINT {
        x: center.x,
        y: center.y,
    }
}

fn source_element(
    automation: &IUIAutomation,
    capture: &WindowsCapture,
) -> Result<Option<IUIAutomationElement>, SelectionError> {
    let foreground = safe_root(unsafe { GetForegroundWindow() });
    if foreground == capture.hwnd {
        if let Ok(focused) = unsafe { automation.GetFocusedElement() } {
            if element_process(&focused) == Some(capture.process_id) {
                if let Some(element) = text_in_chain(automation, &focused) {
                    return Ok(Some(element));
                }
                return Ok(Some(focused));
            }
        }
    }

    if let Ok(pointed) = unsafe { automation.ElementFromPoint(capture.focus_point) } {
        if element_process(&pointed) == Some(capture.process_id) {
            if let Some(element) = text_in_chain(automation, &pointed) {
                return Ok(Some(element));
            }
            return Ok(Some(pointed));
        }
    }

    let root = match unsafe { automation.ElementFromHandle(capture.hwnd) } {
        Ok(element) => element,
        Err(_) => return Ok(None),
    };
    if element_process(&root) != Some(capture.process_id) {
        return Ok(None);
    }
    if let Some(element) = text_in_chain(automation, &root) {
        return Ok(Some(element));
    }
    let condition = unsafe {
        automation
            .CreatePropertyCondition(UIA_IsTextPatternAvailablePropertyId, &variant_bool(true))
    }
    .map_err(|error| automation_error("CreatePropertyCondition", error))?;
    match unsafe { root.FindFirst(TreeScope_Descendants, &condition) } {
        Ok(element) => Ok(Some(element)),
        Err(_) => Ok(Some(root)),
    }
}

fn text_in_chain(
    automation: &IUIAutomation,
    element: &IUIAutomationElement,
) -> Option<IUIAutomationElement> {
    let mut current = element.clone();
    let walker = unsafe { automation.ControlViewWalker() }.ok()?;
    for _ in 0..MAX_PARENT_WALK {
        if text_pattern(&current).is_some() {
            return Some(current);
        }
        current = unsafe { walker.GetParentElement(&current) }.ok()?;
    }
    None
}

fn text_pattern(element: &IUIAutomationElement) -> Option<IUIAutomationTextPattern> {
    unsafe { element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId) }.ok()
}

fn password_near(automation: &IUIAutomation, element: &IUIAutomationElement) -> bool {
    if is_password(element) {
        return true;
    }
    let Ok(walker) = (unsafe { automation.ControlViewWalker() }) else {
        return false;
    };
    let mut current = element.clone();
    for _ in 0..MAX_PARENT_WALK {
        let Ok(parent) = (unsafe { walker.GetParentElement(&current) }) else {
            break;
        };
        if is_password(&parent) {
            return true;
        }
        current = parent;
    }
    false
}

fn is_password(element: &IUIAutomationElement) -> bool {
    unsafe { element.CurrentIsPassword() }
        .map(|value| value.as_bool())
        .unwrap_or(false)
}

fn element_process(element: &IUIAutomationElement) -> Option<u32> {
    unsafe { element.CurrentProcessId() }
        .ok()
        .and_then(|value| u32::try_from(value).ok())
}

fn element_bounds(element: &IUIAutomationElement) -> Option<FrameRegion> {
    unsafe { element.CurrentBoundingRectangle() }
        .ok()
        .and_then(rect_region)
        .filter(valid_region)
}

fn caret_point(element: &IUIAutomationElement) -> Option<ScreenPoint> {
    let pattern =
        unsafe { element.GetCurrentPatternAs::<IUIAutomationTextPattern2>(UIA_TextPattern2Id) }
            .ok()?;
    let mut active = windows::core::BOOL(0);
    let range = unsafe { pattern.GetCaretRange(&mut active) }.ok()?;
    range_bounds(&range).map(region_center)
}

fn range_bounds(range: &IUIAutomationTextRange) -> Option<FrameRegion> {
    let array = unsafe { range.GetBoundingRectangles() }.ok()?;
    if array.is_null() {
        return None;
    }
    let mut data = std::ptr::null_mut();
    if unsafe { SafeArrayAccessData(array, &mut data) }.is_err() {
        let _ = unsafe { SafeArrayDestroy(array) };
        return None;
    }
    let lower = unsafe { SafeArrayGetLBound(array, 1) }.unwrap_or(0);
    let upper = unsafe { SafeArrayGetUBound(array, 1) }.unwrap_or(-1);
    let len = if upper >= lower {
        (upper - lower + 1) as usize
    } else {
        0
    };
    let mut result: Option<FrameRegion> = None;
    if len >= 4 && !data.is_null() {
        let values = unsafe { std::slice::from_raw_parts(data.cast::<f64>(), len) };
        for chunk in values.as_chunks::<4>().0 {
            let left = chunk[0];
            let top = chunk[1];
            let width = chunk[2];
            let height = chunk[3];
            if !left.is_finite()
                || !top.is_finite()
                || !width.is_finite()
                || !height.is_finite()
                || width <= 0.0
                || height <= 0.0
            {
                continue;
            }
            let region = FrameRegion {
                x: left.round() as i32,
                y: top.round() as i32,
                width: width.round().max(1.0) as u32,
                height: height.round().max(1.0) as u32,
            };
            result = Some(match result {
                Some(current) => union(current, region),
                None => region,
            });
        }
    }
    let _ = unsafe { SafeArrayUnaccessData(array) };
    let _ = unsafe { SafeArrayDestroy(array) };
    result.filter(valid_region)
}

fn poll_change(capture: &WindowsCapture, cancel: &AtomicBool, timeout: Duration) -> SelectionEvent {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cancel.load(Ordering::SeqCst) || escape_pressed() {
            return SelectionEvent::Cancelled;
        }
        if !window_alive(capture) {
            return SelectionEvent::SourceLost;
        }
        if safe_root(unsafe { GetForegroundWindow() }) != capture.hwnd {
            return SelectionEvent::SourceChanged;
        }
        std::thread::sleep((deadline - Instant::now()).min(WAIT_POLL));
    }
    SelectionEvent::TimedOut
}

fn escape_pressed() -> bool {
    unsafe { GetAsyncKeyState(i32::from(VK_ESCAPE.0)) as u16 & 0x8000 != 0 }
}

fn variant_bool(value: bool) -> VARIANT {
    let mut variant = VARIANT::default();
    unsafe {
        let inner = &mut *variant.Anonymous.Anonymous;
        inner.vt = VT_BOOL;
        inner.Anonymous.boolVal = VARIANT_BOOL(if value { -1 } else { 0 });
    }
    variant
}

fn rect_region(rect: RECT) -> Option<FrameRegion> {
    let width = rect.right.checked_sub(rect.left)?;
    let height = rect.bottom.checked_sub(rect.top)?;
    if width <= 0 || height <= 0 {
        return None;
    }
    Some(FrameRegion {
        x: rect.left,
        y: rect.top,
        width: width as u32,
        height: height as u32,
    })
}

fn valid_region(region: &FrameRegion) -> bool {
    region.width > 0 && region.height > 0
}

fn contains(region: FrameRegion, point: ScreenPoint) -> bool {
    let right = i64::from(region.x) + i64::from(region.width);
    let bottom = i64::from(region.y) + i64::from(region.height);
    i64::from(point.x) >= i64::from(region.x)
        && i64::from(point.y) >= i64::from(region.y)
        && i64::from(point.x) < right
        && i64::from(point.y) < bottom
}

fn region_center(region: FrameRegion) -> ScreenPoint {
    ScreenPoint {
        x: region.x.saturating_add((region.width / 2) as i32),
        y: region.y.saturating_add((region.height / 2) as i32),
    }
}

fn union(left: FrameRegion, right: FrameRegion) -> FrameRegion {
    let x = left.x.min(right.x);
    let y = left.y.min(right.y);
    let right_edge =
        i64::from(left.x + left.width as i32).max(i64::from(right.x + right.width as i32));
    let bottom_edge =
        i64::from(left.y + left.height as i32).max(i64::from(right.y + right.height as i32));
    FrameRegion {
        x,
        y,
        width: (right_edge - i64::from(x)).max(1) as u32,
        height: (bottom_edge - i64::from(y)).max(1) as u32,
    }
}
