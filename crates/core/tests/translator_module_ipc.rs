//! Граница процесса плагина «Переводчик»: манифест, рукопожатие и вызов
//! действия через настоящий stdio-процесс без UI Automation и сети.

mod support;

use std::path::PathBuf;
use std::time::Duration;

use util_maker_core::ipc::ModuleProcess;
use util_maker_core::{CoreError, Manifest};
use util_maker_sdk::{error_codes, InvocationSource, InvokeStatus};

const MODULE_ID: &str = "org.utilmaker.translator";
const ACTION_ID: &str = "translate";

fn module_binary() -> PathBuf {
    support::binary("translator-module")
}

fn manifest_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/translator/manifest.json")
        .canonicalize()
        .expect("манифест переводчика")
}

fn spawn() -> ModuleProcess {
    let executable = module_binary();
    assert!(
        executable.is_file(),
        "не найден процесс переводчика {}; соберите workspace: cargo build --workspace --bins",
        executable.display()
    );
    ModuleProcess::spawn(&executable).expect("запуск процесса переводчика")
}

#[test]
fn manifest_declares_translate_action_and_entrypoints() {
    let manifest = Manifest::load(&manifest_path()).expect("манифест проходит проверку SDK");
    assert_eq!(manifest.id, MODULE_ID);
    assert_eq!(manifest.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(
        manifest.entrypoint_for("linux-x64"),
        Some("translator-module")
    );
    assert_eq!(
        manifest.entrypoint_for("windows-x64"),
        Some("translator-module.exe")
    );
    let action = manifest.action(ACTION_ID).expect("действие translate");
    assert_eq!(action.name, "Перевести текст");
}

#[test]
fn initialize_confirms_translator_identity() {
    let mut process = spawn();
    let initialized = process
        .initialize(MODULE_ID)
        .expect("рукопожатие по протоколу v1");
    assert_eq!(initialized.module_id, MODULE_ID);
    assert_eq!(initialized.api_version, "1.0.0");
    process.shutdown();
}

#[test]
fn translate_without_source_reports_reason_and_no_http_requests() {
    let mut process = spawn();
    process.initialize(MODULE_ID).expect("рукопожатие");
    let result = process
        .invoke(
            "translator-call-1",
            ACTION_ID,
            InvocationSource::Hotkey,
            Duration::from_secs(5),
        )
        .expect("обработчик translate");

    assert_eq!(result.status, InvokeStatus::Ok);
    let message = result.message.clone().expect("объяснение для пользователя");
    assert!(
        message.contains("потеряло контекст"),
        "неожиданное объяснение: {message}"
    );
    let data = result.data.expect("данные результата");
    assert_eq!(data["callId"], "translator-call-1");
    assert_eq!(data["outcome"], "blocked");
    assert_eq!(data["reason"], "context-lost");
    assert_eq!(data["httpRequests"], 0);
    assert_eq!(data["source"], "hotkey");
    process.shutdown();
}

#[test]
fn unknown_action_returns_action_not_found() {
    let mut process = spawn();
    process.initialize(MODULE_ID).expect("рукопожатие");
    let error = process
        .invoke(
            "translator-call-2",
            "missing-action",
            InvocationSource::Tray,
            Duration::from_secs(5),
        )
        .expect_err("неизвестное действие должно вернуть ошибку");
    match error {
        CoreError::Module { code, .. } => assert_eq!(code, error_codes::ACTION_NOT_FOUND),
        other => panic!("ожидалась ошибка модуля, получено: {other}"),
    }
    process.shutdown();
}

#[test]
fn process_accepts_shutdown_after_action() {
    let mut process = spawn();
    process.initialize(MODULE_ID).expect("рукопожатие");
    let result = process
        .invoke(
            "translator-call-3",
            ACTION_ID,
            InvocationSource::Tray,
            Duration::from_secs(5),
        )
        .expect("обработчик translate");
    assert_eq!(result.status, InvokeStatus::Ok);
    process.shutdown();
}
