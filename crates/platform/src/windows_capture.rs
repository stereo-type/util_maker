//! Нативный захват виртуального рабочего стола Windows через GDI.
//!
//! Процесс должен быть DPI-aware: тогда `EnumDisplayMonitors`, `GetSystemMetrics`
//! и `BitBlt` работают в физических пикселях, включая отрицательные координаты
//! монитора слева от основного. Кадр всегда BGRA8, верхняя строка идёт первой.

use std::mem::size_of;
use std::sync::Once;

use util_maker_sdk::{FrameRegion, MonitorGeometry, ScreenGeometry};
use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
    EnumDisplayMonitors, GetDC, GetDIBits, GetMonitorInfoW, ReleaseDC, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, HMONITOR,
    MONITORINFO, MONITORINFOEXW, SRCCOPY,
};
use windows::Win32::UI::HiDpi::{
    GetDpiForMonitor, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    MDT_EFFECTIVE_DPI,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, MONITORINFOF_PRIMARY, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use crate::capture::{RawFrame, ScreenSource, ScreenSourceError};

static DPI_AWARENESS: Once = Once::new();

/// Источник кадров рабочего стола Windows.
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsScreen;

impl ScreenSource for WindowsScreen {
    fn geometry(&self) -> Result<ScreenGeometry, ScreenSourceError> {
        ensure_dpi_awareness();
        virtual_geometry()
    }

    fn capture(&self, rect: FrameRegion) -> Result<RawFrame, ScreenSourceError> {
        ensure_dpi_awareness();
        capture_bgra(rect)
    }
}

fn ensure_dpi_awareness() {
    DPI_AWARENESS.call_once(|| {
        // Tauri обычно уже задаёт per-monitor v2 в манифесте. Повторный вызов
        // возвращает ERROR_ACCESS_DENIED и не должен ломать захват.
        let _ =
            unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    });
}

fn virtual_geometry() -> Result<ScreenGeometry, ScreenSourceError> {
    let x = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let y = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    let height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
    if width <= 0 || height <= 0 {
        return Err(ScreenSourceError::capture(
            "Windows вернул пустой виртуальный рабочий стол",
        ));
    }

    let mut monitors = Vec::new();
    let mut context = MonitorContext {
        monitors: &mut monitors,
        error: None,
    };
    let enum_result = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(monitor_callback),
            LPARAM(&mut context as *mut _ as isize),
        )
    };
    if enum_result.0 == 0 {
        return Err(ScreenSourceError::capture(
            "EnumDisplayMonitors не вернул мониторы",
        ));
    }
    if let Some(error) = context.error {
        return Err(ScreenSourceError::capture(error));
    }
    if monitors.is_empty() {
        return Err(ScreenSourceError::capture(
            "Windows не вернул ни одного монитора",
        ));
    }
    Ok(ScreenGeometry {
        virtual_screen: FrameRegion {
            x,
            y,
            width: width as u32,
            height: height as u32,
        },
        monitors,
    })
}

struct MonitorContext<'a> {
    monitors: &'a mut Vec<MonitorGeometry>,
    error: Option<String>,
}

unsafe extern "system" fn monitor_callback(
    monitor: HMONITOR,
    _device_context: HDC,
    _monitor_rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    let context = unsafe { &mut *(data.0 as *mut MonitorContext<'_>) };
    if context.error.is_some() {
        return BOOL(0);
    }
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    if unsafe { GetMonitorInfoW(monitor, &mut info as *mut _ as *mut MONITORINFO) }.0 == 0 {
        context.error = Some("GetMonitorInfoW не вернул данные монитора".to_owned());
        return BOOL(0);
    }
    let mut dpi_x = 96u32;
    let mut dpi_y = 96u32;
    if let Err(error) =
        unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }
    {
        context.error = Some(format!("GetDpiForMonitor: {error}"));
        return BOOL(0);
    }
    let rect = info.monitorInfo.rcMonitor;
    let width = rect.right.saturating_sub(rect.left);
    let height = rect.bottom.saturating_sub(rect.top);
    if width <= 0 || height <= 0 {
        context.error = Some("монитор имеет нулевой размер".to_owned());
        return BOOL(0);
    }
    context.monitors.push(MonitorGeometry {
        rect: FrameRegion {
            x: rect.left,
            y: rect.top,
            width: width as u32,
            height: height as u32,
        },
        scale_factor: f64::from(dpi_x) / 96.0,
        primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
    });
    BOOL(1)
}

fn capture_bgra(rect: FrameRegion) -> Result<RawFrame, ScreenSourceError> {
    if rect.width == 0 || rect.height == 0 {
        return Err(ScreenSourceError::capture(
            "нельзя захватить область нулевого размера",
        ));
    }
    let width = i32::try_from(rect.width)
        .map_err(|_| ScreenSourceError::capture("область кадра слишком широкая"))?;
    let height = i32::try_from(rect.height)
        .map_err(|_| ScreenSourceError::capture("область кадра слишком высокая"))?;
    let stride = rect
        .width
        .checked_mul(4)
        .ok_or_else(|| ScreenSourceError::capture("шаг строки кадра переполнен"))?;
    let byte_len = usize::try_from(u64::from(stride) * u64::from(rect.height))
        .map_err(|_| ScreenSourceError::capture("размер кадра не помещается в память"))?;
    let mut pixels = vec![0u8; byte_len];

    let screen = ScreenDc::acquire()?;
    let memory = MemoryDc::create(screen.0)?;
    let bitmap = Bitmap::create(screen.0, width, height)?;
    let old = unsafe { SelectObject(memory.0, HGDIOBJ(bitmap.0 .0)) };
    let copied = unsafe {
        BitBlt(
            memory.0,
            0,
            0,
            width,
            height,
            Some(screen.0),
            rect.x,
            rect.y,
            windows::Win32::Graphics::Gdi::ROP_CODE(SRCCOPY.0 | CAPTUREBLT.0),
        )
    };
    // GetDIBits требует, чтобы BITMAP не был выбран в HDC. Возвращаем прежний
    // объект даже при ошибке BitBlt, затем читаем пиксели.
    unsafe { SelectObject(memory.0, old) };
    copied.map_err(|error| ScreenSourceError::capture(format!("BitBlt: {error}")))?;

    let mut info = BITMAPINFO::default();
    info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
    info.bmiHeader.biWidth = width;
    info.bmiHeader.biHeight = -height;
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    info.bmiHeader.biCompression = BI_RGB.0;
    let lines = unsafe {
        GetDIBits(
            memory.0,
            bitmap.0,
            0,
            height as u32,
            Some(pixels.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        )
    };
    if lines != height {
        return Err(ScreenSourceError::capture(format!(
            "GetDIBits вернул {lines} строк вместо {height}"
        )));
    }

    // Экранный GDI-буфер обычно отдаёт нулевой альфа-канал. Фиксируем
    // непрозрачность, чтобы BGRA8 одинаково читался всеми потребителями.
    for pixel in pixels.chunks_exact_mut(4) {
        pixel[3] = u8::MAX;
    }
    Ok(RawFrame {
        rect,
        stride,
        pixels,
    })
}

struct ScreenDc(HDC);

impl ScreenDc {
    fn acquire() -> Result<Self, ScreenSourceError> {
        let handle = unsafe { GetDC(None) };
        if handle.is_invalid() {
            return Err(ScreenSourceError::capture("GetDC вернул пустой HDC"));
        }
        Ok(Self(handle))
    }
}

impl Drop for ScreenDc {
    fn drop(&mut self) {
        unsafe {
            ReleaseDC(None, self.0);
        }
    }
}

struct MemoryDc(HDC);

impl MemoryDc {
    fn create(source: HDC) -> Result<Self, ScreenSourceError> {
        let handle = unsafe { CreateCompatibleDC(Some(source)) };
        if handle.is_invalid() {
            return Err(ScreenSourceError::capture(
                "CreateCompatibleDC вернул пустой HDC",
            ));
        }
        Ok(Self(handle))
    }
}

impl Drop for MemoryDc {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}

struct Bitmap(HBITMAP);

impl Bitmap {
    fn create(source: HDC, width: i32, height: i32) -> Result<Self, ScreenSourceError> {
        let handle = unsafe { CreateCompatibleBitmap(source, width, height) };
        if handle.is_invalid() {
            return Err(ScreenSourceError::capture(
                "CreateCompatibleBitmap вернул пустой HBITMAP",
            ));
        }
        Ok(Self(handle))
    }
}

impl Drop for Bitmap {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.0 .0));
        }
    }
}
