//! Граница процесса плагина «Текст с экрана»: манифест, языки, кадр и выбор.

mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use util_maker_core::ipc::ModuleProcess;
use util_maker_core::{CoreError, Manifest};
use util_maker_sdk::{
    error_codes, host_methods, FrameOrigin, FrameRef, FrameRegion, HostCall, HostCallContext,
    HostHandler, InvocationSource, MonitorGeometry, RpcError, ScreenGeometry, FRAME_FORMAT_BGRA8,
};

const MODULE_ID: &str = "org.utilmaker.screen-ocr";
const ACTION_ID: &str = "recognize-region";
const FRAME_ID: &str = "screen-ocr-frame-1";

fn manifest_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/screen-ocr/manifest.json")
        .canonicalize()
        .expect("манифест плагина «Текст с экрана»")
}

fn manifest() -> Manifest {
    Manifest::load(&manifest_path()).expect("манифест")
}

struct ScreenOcrHost {
    calls: Arc<Mutex<Vec<String>>>,
    frame: FrameRef,
    pixels: Vec<u8>,
    pixel_path: PathBuf,
    language: String,
    fail_pixels: bool,
}

impl ScreenOcrHost {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let pixel_path = std::env::temp_dir().join(format!(
            "util-maker-screen-ocr-{}-{}.bgra",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let width = 4;
        let height = 3;
        let stride = width * 4 + 4;
        let pixels: Vec<u8> = (0..(stride * height) as u8).collect();
        std::fs::write(&pixel_path, &pixels).expect("файл кадра");
        let frame = FrameRef {
            buffer_id: FRAME_ID.to_owned(),
            format: FRAME_FORMAT_BGRA8.to_owned(),
            width,
            height,
            stride,
            origin: FrameOrigin { x: -2, y: 5 },
            scale_factor: 1.0,
            expires_in_ms: 5_000,
            geometry: Some(ScreenGeometry {
                virtual_screen: FrameRegion {
                    x: -2,
                    y: 5,
                    width,
                    height,
                },
                monitors: vec![
                    MonitorGeometry {
                        rect: FrameRegion {
                            x: -2,
                            y: 5,
                            width: 2,
                            height,
                        },
                        scale_factor: 1.0,
                        primary: false,
                    },
                    MonitorGeometry {
                        rect: FrameRegion {
                            x: 0,
                            y: 5,
                            width: 2,
                            height,
                        },
                        scale_factor: 1.5,
                        primary: true,
                    },
                ],
            }),
            pixel_path: Some(pixel_path.to_string_lossy().into_owned()),
        };
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            frame,
            pixels,
            pixel_path,
            language: "rus+eng".to_owned(),
            fail_pixels: false,
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("журнал вызовов").clone()
    }

    fn set_language(&mut self, language: &str) {
        self.language = language.to_owned();
    }
}

impl Drop for ScreenOcrHost {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.pixel_path);
    }
}

impl HostHandler for ScreenOcrHost {
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
            host_methods::SETTINGS_READ => Ok(json!({"languages": self.language})),
            host_methods::FRAME_CAPTURE => {
                std::fs::write(&self.pixel_path, &self.pixels).expect("файл кадра");
                let mut frame = self.frame.clone();
                if self.fail_pixels {
                    frame.pixel_path = Some("/definitely/not/a/frame.bgra".to_owned());
                }
                Ok(serde_json::to_value(&frame).expect("кадр"))
            }
            host_methods::FRAME_RELEASE => {
                assert_eq!(call.params["bufferId"], json!(FRAME_ID));
                let _ = std::fs::remove_file(&self.pixel_path);
                Ok(Value::Null)
            }
            other => Err(RpcError::new(
                error_codes::METHOD_NOT_FOUND,
                format!("неизвестная операция хоста: {other}"),
            )),
        }
    }
}

fn spawn(manifest: &Manifest) -> ModuleProcess {
    let executable = support::binary("screen-ocr-module");
    assert!(
        executable.is_file(),
        "не найден процесс {}",
        executable.display()
    );
    let mut process = ModuleProcess::spawn(&executable).expect("запуск плагина");
    let initialized = process
        .initialize_with_capabilities(MODULE_ID, &manifest.capabilities)
        .expect("рукопожатие");
    assert_eq!(initialized.module_id, MODULE_ID);
    assert_eq!(initialized.api_version, "1.0.0");
    assert_eq!(initialized.capabilities, manifest.capabilities);
    process
}

#[test]
fn manifest_declares_action_settings_and_capabilities() {
    let manifest = manifest();
    assert_eq!(manifest.id, MODULE_ID);
    assert_eq!(manifest.publisher, "org.utilmaker");
    assert_eq!(
        manifest.capabilities,
        ["screen.capture", "settings.read", "clipboard.write"]
    );
    let action = manifest.action(ACTION_ID).expect("действие");
    assert_eq!(action.name, "Распознать текст в области");
    assert_eq!(
        manifest.resolved_entrypoint("linux-x64").unwrap(),
        PathBuf::from("screen-ocr-module")
    );
    let settings = manifest.settings.as_ref().expect("настройки");
    let field = settings.top_field("languages").expect("поле языков");
    assert_eq!(field.default, Some(json!("rus+eng")));
    assert_eq!(field.options.len(), 3);
    assert_eq!(field.options[0].value, "rus+eng");
    assert_eq!(field.options[0].label, "Русский и английский");
    assert_eq!(field.options[1].value, "rus");
    assert_eq!(field.options[2].value, "eng");
}

#[test]
fn process_reports_missing_host_and_unknown_action() {
    let manifest = manifest();
    let mut process = spawn(&manifest);

    let error = process
        .invoke(
            "screen-ocr-call-1",
            ACTION_ID,
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
            "screen-ocr-call-2",
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
fn process_reads_languages_captures_pixels_and_releases_frame() {
    let manifest = manifest();
    let mut process = spawn(&manifest);
    let mut host = ScreenOcrHost::new();

    let result = process
        .invoke_with_host(
            "screen-ocr-call-3",
            ACTION_ID,
            InvocationSource::Hotkey,
            Duration::from_secs(5),
            &mut host,
            &manifest.capabilities,
        )
        .expect("выбор области");
    let data = result.data.expect("данные выбора");
    assert_eq!(data["outcome"], "awaiting-selection");
    assert_eq!(data["phase"], "selecting");
    assert_eq!(data["language"], "rus+eng");
    assert_eq!(data["frameReleased"], true);
    assert_eq!(data["frame"]["width"], 4);
    assert_eq!(data["frame"]["origin"]["x"], -2);
    assert_eq!(
        host.calls(),
        [
            "settings.read:settings.read",
            "screen.capture:frame.capture",
            "screen.capture:frame.release"
        ]
    );

    let result = process
        .invoke_with_host(
            "screen-ocr-call-4",
            ACTION_ID,
            InvocationSource::Tray,
            Duration::from_secs(5),
            &mut host,
            &manifest.capabilities,
        )
        .expect("повторный вызов");
    let data = result.data.expect("данные активной сессии");
    assert_eq!(data["outcome"], "already-active");
    assert_eq!(
        host.calls().len(),
        3,
        "повторный вызов не делает новый захват"
    );

    host.set_language("eng");
    process.cancel("screen-ocr-call-3").expect("отмена");
    let result = process
        .invoke_with_host(
            "screen-ocr-call-5",
            ACTION_ID,
            InvocationSource::Hotkey,
            Duration::from_secs(5),
            &mut host,
            &manifest.capabilities,
        )
        .expect("новый выбор после Esc");
    assert_eq!(result.data.expect("данные")["language"], "eng");
    assert_eq!(host.calls().len(), 6);

    process.shutdown();
}

#[test]
fn process_applies_pointer_updates_without_a_second_capture() {
    let manifest = manifest();
    let mut process = spawn(&manifest);
    let mut host = ScreenOcrHost::new();
    process
        .invoke_with_host(
            "screen-ocr-call-6",
            ACTION_ID,
            InvocationSource::Hotkey,
            Duration::from_secs(5),
            &mut host,
            &manifest.capabilities,
        )
        .expect("старт выбора");
    let never = AtomicBool::new(false);
    let result = process
        .invoke_cancellable_with_host_params(
            "screen-ocr-call-6",
            ACTION_ID,
            InvocationSource::Hotkey,
            json!({
                "anchor": {"x": -2, "y": 5},
                "cursor": {"x": 2, "y": 8}
            }),
            Duration::from_secs(5),
            &never,
            &mut host,
            &manifest.capabilities,
        )
        .expect("обратное выделение");
    let data = result.data.expect("данные выбора");
    assert_eq!(data["outcome"], "selection-update");
    assert_eq!(data["phase"], "selected");
    assert_eq!(
        data["selection"],
        json!({"x": -2, "y": 5, "width": 3, "height": 2})
    );
    assert_eq!(host.calls().len(), 3, "выбор идёт по копии кадра");

    let result = process
        .invoke_cancellable_with_host_params(
            "screen-ocr-call-6",
            ACTION_ID,
            InvocationSource::Hotkey,
            json!({
                "anchor": {"x": 0, "y": 5},
                "cursor": {"x": 0, "y": 7}
            }),
            Duration::from_secs(5),
            &never,
            &mut host,
            &manifest.capabilities,
        )
        .expect("вырожденный прямоугольник");
    let data = result.data.expect("данные");
    assert_eq!(data["phase"], "selecting");
    assert_eq!(data["selection"], Value::Null);

    process.shutdown();
}

#[test]
fn expired_pixels_can_be_requested_again() {
    let manifest = manifest();
    let mut process = spawn(&manifest);
    let mut host = ScreenOcrHost::new();
    host.fail_pixels = true;

    let error = process
        .invoke_with_host(
            "screen-ocr-call-7",
            ACTION_ID,
            InvocationSource::Hotkey,
            Duration::from_secs(5),
            &mut host,
            &manifest.capabilities,
        )
        .expect_err("без файла пикселей кадр недоступен");
    match error {
        CoreError::Module { code, message } => {
            assert_eq!(code, 2002);
            assert!(message.contains("прочитать пиксели"));
        }
        other => panic!("ожидалась машинная ошибка кадра, получено: {other}"),
    }
    assert_eq!(
        host.calls(),
        [
            "settings.read:settings.read",
            "screen.capture:frame.capture",
            "screen.capture:frame.release"
        ],
        "файл освобождается даже при ошибке чтения"
    );

    host.fail_pixels = false;
    let result = process
        .invoke_with_host(
            "screen-ocr-call-8",
            ACTION_ID,
            InvocationSource::Hotkey,
            Duration::from_secs(5),
            &mut host,
            &manifest.capabilities,
        )
        .expect("повторный захват");
    assert_eq!(
        result.data.expect("данные")["outcome"],
        "awaiting-selection"
    );
    assert_eq!(host.calls().len(), 6);

    process.shutdown();
}
