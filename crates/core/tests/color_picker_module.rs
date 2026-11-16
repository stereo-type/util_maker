//! Граница процесса плагина «Пипетка цвета»: манифест, возможности и вызов.

mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use util_maker_core::ipc::ModuleProcess;
use util_maker_core::{CoreError, Manifest};
use util_maker_sdk::{
    error_codes, host_methods, FrameOrigin, FrameRef, FrameRegion, HostCall, HostCallContext,
    HostHandler, InvocationSource, MonitorGeometry, RpcError, ScreenGeometry, FRAME_FORMAT_BGRA8,
};

const MODULE_ID: &str = "org.utilmaker.color-picker";
const FRAME_ID: &str = "color-picker-frame-1";
const PIXEL_SIZE: u32 = 1024;
const PIXEL_STRIDE: u32 = PIXEL_SIZE * 4 + 64;
/// Точка с эталонным цветом `ED 80 2F FF` в локальных координатах кадра.
const SAMPLE_POINT: (i32, i32) = (3, 2);
const SAMPLE_DESKTOP: (i32, i32) = (-1917, 2);
const SAMPLE_RGB: &str = "rgb(47, 128, 237)";

static FRAME_FILE: AtomicUsize = AtomicUsize::new(0);

fn manifest_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/color-picker/manifest.json")
        .canonicalize()
        .expect("манифест плагина «Пипетка цвета»")
}

fn manifest() -> Manifest {
    Manifest::load(&manifest_path()).expect("манифест")
}

fn frame_file_path() -> PathBuf {
    let index = FRAME_FILE.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!(
        "util-maker-color-picker-{}-{index}.bgra",
        std::process::id()
    ))
}

/// Кадр 1024×1024 с отступом в конце строки и точкой `ED 80 2F FF` в
/// `SAMPLE_POINT`. Начало кадра отрицательное, как у монитора слева от
/// основного.
fn padded_pixels() -> Vec<u8> {
    let mut pixels = vec![0u8; PIXEL_STRIDE as usize * PIXEL_SIZE as usize];
    let offset = SAMPLE_POINT.1 as usize * PIXEL_STRIDE as usize + SAMPLE_POINT.0 as usize * 4;
    pixels[offset..offset + 4].copy_from_slice(&[0xED, 0x80, 0x2F, 0xFF]);
    pixels
}

fn geometry(monitors: &[(i32, i32, u32, u32)]) -> ScreenGeometry {
    let items = monitors
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
    let first = items.first().expect("монитор").rect;
    ScreenGeometry {
        virtual_screen: first,
        monitors: items,
    }
}

struct PickerHost {
    calls: Arc<Mutex<Vec<String>>>,
    frame: FrameRef,
    pixel_path: PathBuf,
    copied: Arc<Mutex<Option<String>>>,
}

impl PickerHost {
    fn with_pixels(frame: FrameRef, pixels: &[u8]) -> Self {
        let pixel_path = frame_file_path();
        std::fs::write(&pixel_path, pixels).expect("файл пикселей кадра");
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            frame: FrameRef {
                pixel_path: Some(pixel_path.to_string_lossy().into_owned()),
                ..frame
            },
            pixel_path,
            copied: Arc::new(Mutex::new(None)),
        }
    }

    fn standard() -> Self {
        Self::with_pixels(
            FrameRef {
                buffer_id: FRAME_ID.to_owned(),
                format: FRAME_FORMAT_BGRA8.to_owned(),
                width: PIXEL_SIZE,
                height: PIXEL_SIZE,
                stride: PIXEL_STRIDE,
                origin: FrameOrigin { x: -1920, y: 0 },
                scale_factor: 1.0,
                expires_in_ms: 5_000,
                geometry: Some(geometry(&[(-1920, 0, PIXEL_SIZE, PIXEL_SIZE)])),
                pixel_path: None,
            },
            &padded_pixels(),
        )
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("журнал вызовов").clone()
    }
}

impl Drop for PickerHost {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.pixel_path);
    }
}

impl HostHandler for PickerHost {
    fn handle(&mut self, context: &HostCallContext, call: &HostCall) -> Result<Value, RpcError> {
        assert_eq!(call.call_id, context.call_id);
        assert!(
            context.allows(&call.capability),
            "возможность {} не объявлена",
            call.capability
        );
        self.calls
            .lock()
            .expect("журнал вызовов")
            .push(format!("{}:{}", call.capability, call.method));
        match call.method.as_str() {
            host_methods::SETTINGS_READ => Ok(json!({"format": "rgb"})),
            host_methods::FRAME_CAPTURE => Ok(serde_json::to_value(&self.frame).expect("кадр")),
            host_methods::FRAME_RELEASE => {
                assert_eq!(call.params["bufferId"], json!(FRAME_ID));
                let _ = std::fs::remove_file(&self.pixel_path);
                Ok(Value::Null)
            }
            host_methods::BUFFER_COPY => {
                assert_eq!(call.capability, "clipboard.write");
                let text = call.params["text"]
                    .as_str()
                    .expect("текст буфера")
                    .to_owned();
                *self.copied.lock().expect("буфер") = Some(text);
                Ok(json!({"copied": true}))
            }
            other => Err(RpcError::new(
                error_codes::METHOD_NOT_FOUND,
                format!("неизвестная операция хоста: {other}"),
            )),
        }
    }
}

fn spawn_process() -> ModuleProcess {
    let executable = support::binary("color-picker-module");
    assert!(
        executable.is_file(),
        "не найден процесс {}",
        executable.display()
    );
    let manifest = manifest();
    let mut process = ModuleProcess::spawn(&executable).expect("запуск плагина");
    process
        .initialize_with_capabilities(MODULE_ID, &manifest.capabilities)
        .expect("рукопожатие");
    process
}

fn pick(
    process: &mut ModuleProcess,
    call_id: &str,
    params: Value,
    host: &mut PickerHost,
) -> Result<util_maker_sdk::InvokeResult, CoreError> {
    let manifest = manifest();
    let cancel = AtomicBool::new(false);
    process.invoke_cancellable_with_host_params(
        call_id,
        "pick-color",
        InvocationSource::Hotkey,
        params,
        Duration::from_secs(10),
        &cancel,
        host,
        &manifest.capabilities,
    )
}

#[test]
fn manifest_declares_capabilities_action_and_hex_default() {
    let manifest = manifest();
    assert_eq!(manifest.id, MODULE_ID);
    assert_eq!(manifest.publisher, "org.utilmaker");
    assert_eq!(
        manifest.capabilities,
        ["screen.capture", "clipboard.write", "settings.read"]
    );
    let action = manifest.action("pick-color").expect("действие");
    assert_eq!(action.name, "Скопировать цвет пикселя");
    assert_eq!(
        manifest.resolved_entrypoint("linux-x64").unwrap(),
        PathBuf::from("color-picker-module")
    );
    let settings = manifest.settings.as_ref().expect("настройки");
    let format = settings.top_field("format").expect("поле формата");
    assert_eq!(format.default, Some(json!("hex")));
    assert_eq!(format.options.len(), 2);
    assert_eq!(format.options[0].value, "hex");
    assert_eq!(format.options[1].value, "rgb");
}

#[test]
fn process_reports_missing_host_and_unknown_action() {
    let mut process = spawn_process();
    let error = process
        .invoke(
            "color-picker-call-1",
            "pick-color",
            InvocationSource::Hotkey,
            Duration::from_secs(5),
        )
        .expect_err("без моста возможностей действие должно вернуть ошибку");
    match error {
        CoreError::Module { code, message } => {
            assert_eq!(code, 2001);
            assert!(message.contains("источник кадра"));
        }
        other => panic!("ожидалась ошибка плагина, получено: {other}"),
    }

    let error = process
        .invoke(
            "color-picker-call-2",
            "unknown-action",
            InvocationSource::Tray,
            Duration::from_secs(5),
        )
        .expect_err("неизвестное действие");
    match error {
        CoreError::Module { code, .. } => {
            assert_eq!(code, error_codes::ACTION_NOT_FOUND);
        }
        other => panic!("ожидалась ошибка ACTION_NOT_FOUND, получено: {other}"),
    }

    process.shutdown();
}

#[test]
fn process_reads_padded_frame_and_waits_for_point() {
    let mut process = spawn_process();
    let mut host = PickerHost::standard();

    let result = pick(&mut process, "color-picker-call-3", Value::Null, &mut host)
        .expect("кадр прочитан, выбор ждёт точку");
    let data = result.data.expect("данные вызова");
    assert_eq!(data["outcome"], "awaiting-selection");
    assert_eq!(data["format"], "rgb");
    assert_eq!(data["frame"]["width"], PIXEL_SIZE);
    assert_eq!(data["frame"]["height"], PIXEL_SIZE);
    assert_eq!(data["frame"]["stride"], PIXEL_STRIDE);
    assert_eq!(data["frame"]["origin"]["x"], -1920);
    assert_eq!(data["frame"]["monitorCount"], 1);
    assert_eq!(data["frameReleased"], true);
    assert_eq!(
        host.calls(),
        [
            "settings.read:settings.read",
            "screen.capture:frame.capture",
            "screen.capture:frame.release"
        ]
    );
    assert!(
        !host.pixel_path.exists(),
        "локальная копия кадра освобождена вместе со ссылкой"
    );
    assert!(host.copied.lock().expect("буфер").is_none());

    process.shutdown();
}

#[test]
fn process_copies_picked_pixel_in_requested_format() {
    let mut process = spawn_process();
    let mut host = PickerHost::standard();

    let result = pick(
        &mut process,
        "color-picker-call-4",
        json!({"point": {"x": SAMPLE_DESKTOP.0, "y": SAMPLE_DESKTOP.1}}),
        &mut host,
    )
    .expect("цвет выбранной точки");
    let data = result.data.expect("данные вызова");
    assert_eq!(data["outcome"], "copied");
    assert_eq!(data["value"], SAMPLE_RGB);
    assert_eq!(data["format"], "rgb");
    assert_eq!(data["point"]["x"], SAMPLE_DESKTOP.0);
    assert_eq!(data["point"]["y"], SAMPLE_DESKTOP.1);
    assert_eq!(
        *host.copied.lock().expect("буфер"),
        Some(SAMPLE_RGB.to_owned())
    );
    assert_eq!(
        host.calls(),
        [
            "settings.read:settings.read",
            "screen.capture:frame.capture",
            "screen.capture:frame.release",
            "clipboard.write:buffer.copy"
        ]
    );

    process.shutdown();
}

#[test]
fn process_blocks_point_outside_monitors_or_frame() {
    let monitors = geometry(&[(-1920, 0, 512, 1080), (1024, 0, 512, 1080)]);
    let mut process = spawn_process();
    let mut host = PickerHost::with_pixels(
        FrameRef {
            buffer_id: FRAME_ID.to_owned(),
            format: FRAME_FORMAT_BGRA8.to_owned(),
            width: PIXEL_SIZE,
            height: PIXEL_SIZE,
            stride: PIXEL_STRIDE,
            origin: FrameOrigin { x: -1920, y: 0 },
            scale_factor: 1.0,
            expires_in_ms: 5_000,
            geometry: Some(monitors),
            pixel_path: None,
        },
        &padded_pixels(),
    );

    let error = pick(
        &mut process,
        "color-picker-call-5",
        json!({"point": {"x": 400, "y": 10}}),
        &mut host,
    )
    .expect_err("точка в промежутке между мониторами");
    match error {
        CoreError::Module { code, message } => {
            assert_eq!(code, error_codes::INVALID_PARAMS);
            assert!(
                message.contains("не принадлежит ни одному монитору"),
                "причина точки: {message}"
            );
        }
        other => panic!("ожидалась ошибка точки, получено: {other}"),
    }

    // Кадр уже освобождён, повторный вызов читает новый файл.
    std::fs::write(&host.pixel_path, padded_pixels()).expect("файл пикселей");
    let error = pick(
        &mut process,
        "color-picker-call-6",
        json!({"point": {"x": -1917, "y": 1050}}),
        &mut host,
    )
    .expect_err("точка внутри монитора справа, но вне кадра");
    match error {
        CoreError::Module { code, message } => {
            assert_eq!(code, error_codes::INVALID_PARAMS);
            assert!(message.contains("вне кадра"), "причина кадра: {message}");
        }
        other => panic!("ожидалась ошибка кадра, получено: {other}"),
    }
    assert!(host.copied.lock().expect("буфер").is_none());

    process.shutdown();
}

#[test]
fn process_reports_missing_geometry_and_pixels() {
    let mut process = spawn_process();

    let mut host = PickerHost::with_pixels(
        FrameRef {
            buffer_id: FRAME_ID.to_owned(),
            format: FRAME_FORMAT_BGRA8.to_owned(),
            width: PIXEL_SIZE,
            height: PIXEL_SIZE,
            stride: PIXEL_STRIDE,
            origin: FrameOrigin { x: -1920, y: 0 },
            scale_factor: 1.0,
            expires_in_ms: 5_000,
            geometry: None,
            pixel_path: None,
        },
        &padded_pixels(),
    );
    let error = pick(&mut process, "color-picker-call-7", Value::Null, &mut host)
        .expect_err("без геометрии выбор невозможен");
    match error {
        CoreError::Module { code, message } => {
            assert_eq!(code, 2002);
            assert!(message.contains("геометрия"), "причина: {message}");
        }
        other => panic!("ожидалась ошибка геометрии, получено: {other}"),
    }
    assert!(host
        .calls()
        .contains(&"screen.capture:frame.release".to_owned()));

    let mut host = PickerHost {
        calls: Arc::new(Mutex::new(Vec::new())),
        frame: FrameRef {
            buffer_id: FRAME_ID.to_owned(),
            format: FRAME_FORMAT_BGRA8.to_owned(),
            width: PIXEL_SIZE,
            height: PIXEL_SIZE,
            stride: PIXEL_STRIDE,
            origin: FrameOrigin { x: -1920, y: 0 },
            scale_factor: 1.0,
            expires_in_ms: 5_000,
            geometry: Some(geometry(&[(-1920, 0, PIXEL_SIZE, PIXEL_SIZE)])),
            pixel_path: None,
        },
        pixel_path: frame_file_path(),
        copied: Arc::new(Mutex::new(None)),
    };
    let error = pick(&mut process, "color-picker-call-8", Value::Null, &mut host)
        .expect_err("без файла пикселей кадр недоступен");
    match error {
        CoreError::Module { code, message } => {
            assert_eq!(code, 2002);
            assert!(
                message.contains("не удалось прочитать пиксели"),
                "причина: {message}"
            );
        }
        other => panic!("ожидалась ошибка пикселей, получено: {other}"),
    }
    assert!(host
        .calls()
        .contains(&"screen.capture:frame.release".to_owned()));

    process.shutdown();
}

#[test]
fn process_rejects_malformed_point() {
    let mut process = spawn_process();
    let mut host = PickerHost::standard();
    let error = pick(
        &mut process,
        "color-picker-call-9",
        json!({"point": {"x": "три", "y": 2}}),
        &mut host,
    )
    .expect_err("некорректная координата");
    match error {
        CoreError::Module { code, message } => {
            assert_eq!(code, error_codes::INVALID_PARAMS);
            assert!(message.contains("point.x"), "причина: {message}");
        }
        other => panic!("ожидалась ошибка параметров, получено: {other}"),
    }
    assert!(!host
        .calls()
        .contains(&"clipboard.write:buffer.copy".to_owned()));

    process.shutdown();
}
