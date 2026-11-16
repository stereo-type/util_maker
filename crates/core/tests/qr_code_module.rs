//! Граница процесса плагина «QR-коды»: манифест, выделение и окно показа.

mod support;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use util_maker_core::ipc::ModuleProcess;
use util_maker_core::{CoreError, Manifest};
use util_maker_sdk::{
    error_codes, host_methods, HostCall, HostCallContext, HostHandler, InvocationSource, RpcError,
};

const MODULE_ID: &str = "org.utilmaker.qr-code";
const SHOW_QR: &str = "show-qr";
const READ_QR: &str = "read-qr-region";

fn manifest_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/qr-code/manifest.json")
        .canonicalize()
        .expect("манифест плагина «QR-коды»")
}

fn spawn(manifest: &Manifest) -> ModuleProcess {
    let executable = support::binary("qr-code-module");
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
    assert_eq!(initialized.api_version, "1.1.0");
    assert_eq!(initialized.capabilities, manifest.capabilities);
    process
}

struct SelectionHost {
    response: Value,
    calls: Arc<Mutex<Vec<String>>>,
}

impl SelectionHost {
    fn new(response: Value) -> Self {
        Self {
            response,
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("журнал вызовов").clone()
    }
}

impl HostHandler for SelectionHost {
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
            host_methods::SELECTION_GET => Ok(self.response.clone()),
            other => Err(RpcError::new(
                error_codes::METHOD_NOT_FOUND,
                format!("неизвестная операция хоста: {other}"),
            )),
        }
    }
}

fn text_selection(text: &str) -> Value {
    json!({
        "source": {
            "sourceRef": "selection-1",
            "window": {"x": 10, "y": 20, "width": 800, "height": 600},
            "processId": 42
        },
        "state": {
            "kind": "text",
            "text": text,
            "bounds": {"x": 30, "y": 40, "width": 400, "height": 30}
        }
    })
}

#[test]
fn manifest_declares_both_actions_and_capabilities() {
    let manifest = Manifest::load(&manifest_path()).expect("манифест");
    assert_eq!(manifest.id, MODULE_ID);
    assert_eq!(manifest.publisher, "org.utilmaker");
    assert_eq!(
        manifest.capabilities,
        ["selection.read", "screen.capture", "clipboard.write"]
    );
    assert_eq!(
        manifest.action(SHOW_QR).expect("показ QR").name,
        "Показать QR из текста"
    );
    assert_eq!(
        manifest.action(READ_QR).expect("чтение QR").name,
        "Прочитать QR с экрана"
    );
    assert_eq!(
        manifest.resolved_entrypoint("linux-x64").unwrap(),
        PathBuf::from("qr-code-module")
    );
    assert_eq!(
        manifest.resolved_entrypoint("windows-x64").unwrap(),
        PathBuf::from("qr-code-module.exe")
    );
}

#[test]
fn process_without_host_opens_manual_input_and_reports_unknown_action() {
    let manifest = Manifest::load(&manifest_path()).expect("манифест");
    let mut process = spawn(&manifest);

    let result = process
        .invoke(
            "qr-call-1",
            SHOW_QR,
            InvocationSource::Hotkey,
            Duration::from_secs(5),
        )
        .expect("без хоста окно ручного ввода всё равно открывается");
    let data = result.data.expect("данные окна");
    assert_eq!(data["outcome"], "manual-input");
    assert_eq!(data["input"], "manual");
    assert_eq!(data["manualReason"], "selection-unavailable");
    assert_eq!(data["text"], "");
    assert!(data["qr"].is_null());
    assert_eq!(data["clipboardChanged"], false);

    let error = process
        .invoke(
            "qr-call-2",
            "unknown-action",
            InvocationSource::Tray,
            Duration::from_secs(5),
        )
        .expect_err("неизвестное действие");
    match error {
        CoreError::Module { code, .. } => assert_eq!(code, error_codes::ACTION_NOT_FOUND),
        other => panic!("ожидалась ошибка ACTION_NOT_FOUND, получено: {other}"),
    }

    process.shutdown();
}

#[test]
fn process_reads_selection_before_showing_text_and_qr() {
    let manifest = Manifest::load(&manifest_path()).expect("манифест");
    let mut process = spawn(&manifest);
    let text = "  Привет, мир! 👋\nСтрока 2\t— конец  ";
    let mut host = SelectionHost::new(text_selection(text));

    let result = process
        .invoke_with_host(
            "qr-call-3",
            SHOW_QR,
            InvocationSource::Hotkey,
            Duration::from_secs(5),
            &mut host,
            &manifest.capabilities,
        )
        .expect("показ выделения");
    let data = result.data.expect("данные окна");
    assert_eq!(data["outcome"], "showing");
    assert_eq!(data["input"], "selection");
    assert_eq!(data["text"], text);
    assert_eq!(data["clipboardChanged"], false);
    let qr_size = data["qr"]["size"].as_u64().expect("размер матрицы");
    assert_eq!(
        data["qr"]["modules"].as_array().expect("модули").len() as u64,
        qr_size * qr_size
    );
    assert!(data["qr"]["layout"]["scale"].as_u64().unwrap() >= 1);
    assert_eq!(host.calls(), ["selection.read:selection.get"]);
    assert!(
        !host.calls().iter().any(|call| call.contains("buffer.copy")),
        "показ QR не трогает буфер обмена"
    );

    process.shutdown();
}

#[test]
fn password_selection_does_not_read_or_show_value() {
    let manifest = Manifest::load(&manifest_path()).expect("манифест");
    let mut process = spawn(&manifest);
    let mut host = SelectionHost::new(json!({
        "source": {"sourceRef": "selection-2", "window": {"x": 0, "y": 0, "width": 1, "height": 1}, "processId": 7},
        "state": {
            "kind": "password",
            "text": "секрет",
            "bounds": {"x": 0, "y": 0, "width": 100, "height": 20}
        }
    }));

    let result = process
        .invoke_with_host(
            "qr-call-4",
            SHOW_QR,
            InvocationSource::Tray,
            Duration::from_secs(5),
            &mut host,
            &manifest.capabilities,
        )
        .expect("парольное поле");
    let data = result.data.expect("данные окна");
    assert_eq!(data["outcome"], "manual-input");
    assert_eq!(data["manualReason"], "password");
    assert_eq!(data["text"], "");
    assert!(data["qr"].is_null());
    assert!(!serde_json::to_string(&data).unwrap().contains("секрет"));
    assert_eq!(host.calls(), ["selection.read:selection.get"]);

    process.shutdown();
}

#[test]
fn manual_text_skips_selection_and_read_qr_requires_selection() {
    let manifest = Manifest::load(&manifest_path()).expect("манифест");
    let mut process = spawn(&manifest);
    let mut host = SelectionHost::new(text_selection("не читать"));

    let result = process
        .invoke_cancellable_with_host_params(
            "qr-call-5",
            SHOW_QR,
            InvocationSource::Hotkey,
            json!({"manualText": "https://example.com/?q=1", "windowWidth": 600, "windowHeight": 600}),
            Duration::from_secs(5),
            &std::sync::atomic::AtomicBool::new(false),
            &mut host,
            &manifest.capabilities,
        )
        .expect("ручной ввод");
    let data = result.data.expect("данные окна");
    assert_eq!(data["outcome"], "showing");
    assert_eq!(data["input"], "manual");
    assert_eq!(data["text"], "https://example.com/?q=1");
    assert_eq!(data["width"], 600);
    assert_eq!(data["height"], 600);
    assert!(data["qr"].is_object());
    assert!(
        host.calls().is_empty(),
        "ручной ввод не запрашивает выделение"
    );

    let error = process
        .invoke_with_host(
            "qr-call-6",
            READ_QR,
            InvocationSource::Hotkey,
            Duration::from_secs(5),
            &mut host,
            &manifest.capabilities,
        )
        .expect_err("без области чтение невозможно");
    match error {
        CoreError::Module { code, message } => {
            assert_eq!(code, error_codes::INVALID_PARAMS);
            assert!(message.contains("область"));
        }
        other => panic!("ожидалась ошибка процесса, получено: {other}"),
    }

    process.shutdown();
}

struct ReadHost {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    path: PathBuf,
    calls: Arc<Mutex<Vec<String>>>,
}

impl ReadHost {
    fn new(reference: &PathBuf) -> Self {
        let (width, height, gray) = read_pgm(reference);
        let mut pixels = Vec::with_capacity(gray.len() * 4);
        for value in gray {
            pixels.extend_from_slice(&[value, value, value, 255]);
        }
        Self {
            width,
            height,
            pixels,
            path: std::env::temp_dir().join(format!(
                "qr-read-test-{}-{}.bgra",
                std::process::id(),
                width
            )),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("журнал вызовов").clone()
    }
}

impl HostHandler for ReadHost {
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
            host_methods::FRAME_CAPTURE => {
                assert!(call.params.get("region").is_some(), "область кадра");
                std::fs::write(&self.path, &self.pixels).expect("файл пикселей");
                Ok(json!({
                    "bufferId": "frame-1",
                    "format": "bgra8",
                    "width": self.width,
                    "height": self.height,
                    "stride": self.width * 4,
                    "origin": {"x": 0, "y": 0},
                    "scaleFactor": 1.0,
                    "expiresInMs": 5000,
                    "pixelPath": self.path.to_string_lossy(),
                }))
            }
            host_methods::FRAME_RELEASE => {
                assert_eq!(call.params["bufferId"], "frame-1");
                let _ = std::fs::remove_file(&self.path);
                Ok(json!({}))
            }
            other => Err(RpcError::new(
                error_codes::METHOD_NOT_FOUND,
                format!("неизвестная операция хоста: {other}"),
            )),
        }
    }
}

fn read_pgm(path: &PathBuf) -> (u32, u32, Vec<u8>) {
    let bytes = std::fs::read(path).expect("эталон PGM");
    let mut parts = bytes.splitn(4, |byte| *byte == b'\n');
    assert_eq!(parts.next(), Some(b"P5".as_slice()), "формат PGM");
    let size = std::str::from_utf8(parts.next().expect("размер PGM")).expect("размер PGM");
    let mut numbers = size.split_whitespace();
    let width: u32 = numbers.next().expect("ширина PGM").parse().expect("ширина");
    let height: u32 = numbers.next().expect("высота PGM").parse().expect("высота");
    let max = std::str::from_utf8(parts.next().expect("максимум PGM")).expect("максимум PGM");
    assert_eq!(max.trim(), "255");
    let gray = parts.next().expect("пиксели PGM").to_vec();
    assert_eq!(gray.len(), width as usize * height as usize);
    (width, height, gray)
}

#[test]
fn read_qr_decodes_selected_frame_and_releases_it() {
    let manifest = Manifest::load(&manifest_path()).expect("манифест");
    let mut process = spawn(&manifest);
    let reference = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/qr-code/reference/url.pgm")
        .canonicalize()
        .expect("эталон url.pgm");
    let mut host = ReadHost::new(&reference);
    let selection = json!({"x": 0, "y": 0, "width": host.width, "height": host.height});

    let result = process
        .invoke_cancellable_with_host_params(
            "qr-call-7",
            READ_QR,
            InvocationSource::Hotkey,
            json!({"selection": selection}),
            Duration::from_secs(5),
            &std::sync::atomic::AtomicBool::new(false),
            &mut host,
            &manifest.capabilities,
        )
        .expect("чтение QR");
    let data = result.data.expect("данные чтения");
    assert_eq!(data["outcome"], "decoded");
    let text = data["text"].as_str().expect("текст QR");
    assert!(text.starts_with("http"), "неожиданный текст: {text}");
    assert_eq!(result.message.as_deref(), Some(text));
    assert!(data["version"].as_u64().unwrap() > 0);
    assert_eq!(data["clipboardChanged"], false);
    assert_eq!(
        host.calls(),
        [
            "screen.capture:frame.capture",
            "screen.capture:frame.release",
        ]
    );
    assert!(!host.path.exists(), "файл кадра удалён после release");

    process.shutdown();
}

#[test]
fn read_qr_reports_missing_code_for_blank_area() {
    let manifest = Manifest::load(&manifest_path()).expect("манифест");
    let mut process = spawn(&manifest);
    let reference = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/qr-code/reference/ascii.pgm")
        .canonicalize()
        .expect("эталон ascii.pgm");
    let mut host = ReadHost::new(&reference);

    let error = process
        .invoke_cancellable_with_host_params(
            "qr-call-8",
            READ_QR,
            InvocationSource::Tray,
            json!({"selection": {"x": 0, "y": 0, "width": 20, "height": 20}}),
            Duration::from_secs(5),
            &std::sync::atomic::AtomicBool::new(false),
            &mut host,
            &manifest.capabilities,
        )
        .expect_err("в пустой области нет кода");
    match error {
        CoreError::Module { code, message } => {
            assert_eq!(code, 2002);
            assert!(message.contains("не найден"), "сообщение: {message}");
        }
        other => panic!("ожидалась ошибка чтения, получено: {other}"),
    }
    assert_eq!(
        host.calls(),
        [
            "screen.capture:frame.capture",
            "screen.capture:frame.release",
        ]
    );

    process.shutdown();
}
