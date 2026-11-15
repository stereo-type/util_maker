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
fn manual_text_skips_selection_and_read_qr_is_not_implemented_yet() {
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
        .expect_err("чтение QR появится в следующей карточке");
    match error {
        CoreError::Module { code, message } => {
            assert_eq!(code, 2001);
            assert!(message.contains("ещё не подключено"));
        }
        other => panic!("ожидалась ошибка процесса, получено: {other}"),
    }
    assert!(
        host.calls().is_empty(),
        "чтение QR не обращается к кадру до реализации"
    );

    process.shutdown();
}
