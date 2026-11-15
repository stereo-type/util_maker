//! Граница процесса плагина «Пипетка цвета»: манифест, возможности и вызов.

mod support;

use std::path::PathBuf;
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

fn manifest_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/color-picker/manifest.json")
        .canonicalize()
        .expect("манифест плагина «Пипетка цвета»")
}

fn manifest() -> Manifest {
    Manifest::load(&manifest_path()).expect("манифест")
}

struct PickerHost {
    calls: Arc<Mutex<Vec<String>>>,
    frame: FrameRef,
}

impl PickerHost {
    fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            frame: FrameRef {
                buffer_id: FRAME_ID.to_owned(),
                format: FRAME_FORMAT_BGRA8.to_owned(),
                width: 4,
                height: 3,
                stride: 16,
                origin: FrameOrigin { x: -2, y: 5 },
                scale_factor: 1.0,
                expires_in_ms: 5_000,
                geometry: Some(ScreenGeometry {
                    virtual_screen: FrameRegion {
                        x: -2,
                        y: 5,
                        width: 4,
                        height: 3,
                    },
                    monitors: vec![MonitorGeometry {
                        rect: FrameRegion {
                            x: -2,
                            y: 5,
                            width: 4,
                            height: 3,
                        },
                        scale_factor: 1.0,
                        primary: true,
                    }],
                }),
            },
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("журнал вызовов").clone()
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
                Ok(Value::Null)
            }
            other => Err(RpcError::new(
                error_codes::METHOD_NOT_FOUND,
                format!("неизвестная операция хоста: {other}"),
            )),
        }
    }
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
    let executable = support::binary("color-picker-module");
    assert!(
        executable.is_file(),
        "не найден процесс {}",
        executable.display()
    );
    let manifest = manifest();
    let mut process = ModuleProcess::spawn(&executable).expect("запуск плагина");
    let initialized = process
        .initialize_with_capabilities(MODULE_ID, &manifest.capabilities)
        .expect("рукопожатие");
    assert_eq!(initialized.module_id, MODULE_ID);
    assert_eq!(initialized.api_version, "1.0.0");
    assert_eq!(initialized.capabilities, manifest.capabilities);

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
fn process_reads_format_captures_and_releases_frame() {
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
    let mut host = PickerHost::new();

    let error = process
        .invoke_with_host(
            "color-picker-call-3",
            "pick-color",
            InvocationSource::Hotkey,
            Duration::from_secs(5),
            &mut host,
            &manifest.capabilities,
        )
        .expect_err("передача байтов кадра ещё не реализована");
    match error {
        CoreError::Module { code, message } => {
            assert_eq!(code, 2002);
            assert!(message.contains("передача пикселей"));
        }
        other => panic!("ожидалась машинная ошибка SDK, получено: {other}"),
    }
    assert_eq!(
        host.calls(),
        [
            "settings.read:settings.read",
            "screen.capture:frame.capture",
            "screen.capture:frame.release"
        ]
    );

    process.shutdown();
}
