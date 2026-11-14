//! Публичный исход сбоя: адрес модуля, машинная категория и безопасный текст.

mod support;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use util_maker_core::registry::ActionRef;
use util_maker_core::{FailureCategory, Manifest, ModuleEntry, Registry};
use util_maker_sdk::{Action, InvocationSource, InvokeStatus};

const CONTROL_ID: &str = "org.utilmaker.control";
const FIXTURE_ID: &str = "org.utilmaker.fixture";
const CRASH_ID: &str = "org.example.crash";
const RPC_ERROR_ID: &str = "org.example.rpc-error";
const STDERR_ID: &str = "org.example.stderr";
const UNEXPECTED_ID: &str = "org.example.unexpected";

fn manifest_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/control/manifest.json")
}

fn control() -> ModuleEntry {
    let mut manifest = Manifest::load(&manifest_path()).expect("манифест");
    manifest.id = CONTROL_ID.to_owned();
    manifest.actions = vec![Action {
        id: "ping".to_owned(),
        name: "Проверить связь".to_owned(),
        description: None,
        recommended_hotkeys: Vec::new(),
    }];
    ModuleEntry::new(manifest, support::binary("control-module"), true)
}

fn fixture(id: &str, mode: &str) -> ModuleEntry {
    let mut manifest = Manifest::load(&manifest_path()).expect("манифест");
    manifest.id = id.to_owned();
    manifest.name = format!("Тестовый модуль {id}");
    manifest.actions = vec![Action {
        id: "run".to_owned(),
        name: "Выполнить".to_owned(),
        description: None,
        recommended_hotkeys: Vec::new(),
    }];
    let argument = if id == FIXTURE_ID {
        mode.to_owned()
    } else {
        format!("{mode}@{id}")
    };
    ModuleEntry::new(manifest, support::binary("fixture-module"), true)
        .with_arguments(vec![argument])
}

fn registry() -> Registry {
    let registry = Registry::new();
    registry
        .replace_packages(vec![
            control(),
            fixture(FIXTURE_ID, "ok"),
            fixture(CRASH_ID, "crash"),
            fixture(RPC_ERROR_ID, "rpc-error"),
            fixture(STDERR_ID, "stderr-secret-init"),
            fixture(UNEXPECTED_ID, "unexpected-secret"),
        ])
        .expect("каталог");
    registry
}

#[test]
fn crash_during_action_is_addressed_to_exact_module_without_auto_disable() {
    let registry = registry();
    let failure = registry
        .invoke_detailed(
            &ActionRef::new(CRASH_ID, "run"),
            InvocationSource::Hotkey,
            Duration::from_secs(5),
            None,
        )
        .expect_err("падение");
    assert_eq!(failure.category, FailureCategory::ModuleCrashed);
    assert_eq!(failure.module_id, CRASH_ID);
    assert_eq!(failure.action_id, "run");
    assert!(!failure.call_id.is_empty());
    assert!(!failure.message.contains("secret"));

    // Сбой не отключает плагин автоматически: явный повтор снова доходит до
    // процесса и снова даёт конечный исход, а не «плагин отключён».
    let retry = registry
        .invoke_detailed(
            &ActionRef::new(CRASH_ID, "run"),
            InvocationSource::Hotkey,
            Duration::from_secs(5),
            None,
        )
        .expect_err("повторное падение");
    assert_eq!(retry.category, FailureCategory::ModuleCrashed);
    assert!(registry.module(CRASH_ID).expect("модуль").enabled);
}

#[test]
fn rpc_error_is_distinct_from_crash_and_is_sanitized() {
    let registry = registry();
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = lines.clone();
    let logger: util_maker_core::Logger =
        Arc::new(move |line: &str| sink.lock().expect("журнал").push(line.to_owned()));
    let failure = registry
        .invoke_detailed(
            &ActionRef::new(RPC_ERROR_ID, "run"),
            InvocationSource::Tray,
            Duration::from_secs(5),
            Some(logger.clone()),
        )
        .expect_err("ошибка плагина");
    assert_eq!(failure.category, FailureCategory::ModuleReported);
    assert_eq!(failure.rpc_code, Some(2001));
    assert!(!failure.message.contains("secret-key-123"));
    assert!(!failure.message.contains("секрет"));
    let journal = lines.lock().expect("журнал").join("\n");
    assert!(journal.contains("category=module_reported_error"));
    assert!(!journal.contains("secret-key-123"));
}

#[test]
fn stderr_crash_does_not_expose_raw_secret() {
    let registry = registry();
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = lines.clone();
    let logger: util_maker_core::Logger =
        Arc::new(move |line: &str| sink.lock().expect("журнал").push(line.to_owned()));
    let failure = registry
        .invoke_detailed(
            &ActionRef::new(STDERR_ID, "run"),
            InvocationSource::Tray,
            Duration::from_secs(5),
            Some(logger),
        )
        .expect_err("падение при инициализации");
    assert_eq!(failure.category, FailureCategory::ModuleCrashed);
    let journal = lines.lock().expect("журнал").join("\n");
    assert!(!journal.contains("secret-key-123"));
    assert!(journal.contains("скрыто"));
}

#[test]
fn one_crashing_plugin_does_not_block_another() {
    let registry = Arc::new(registry());
    let crashing = {
        let registry = registry.clone();
        std::thread::spawn(move || {
            registry.invoke_detailed(
                &ActionRef::new(CRASH_ID, "run"),
                InvocationSource::Hotkey,
                Duration::from_secs(5),
                None,
            )
        })
    };
    let healthy = registry
        .invoke(
            &ActionRef::new(CONTROL_ID, "ping"),
            InvocationSource::Tray,
            Duration::from_secs(5),
            None,
        )
        .expect("второй плагин отвечает");
    assert_eq!(healthy.status, InvokeStatus::Ok);
    let failure = crashing.join().expect("поток").expect_err("сбой");
    assert_eq!(failure.category, FailureCategory::ModuleCrashed);
    assert_eq!(failure.module_id, CRASH_ID);
    assert!(registry.can_switch(CONTROL_ID));
}

#[test]
fn unexpected_ipc_message_is_sanitized_in_journal() {
    let registry = registry();
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = lines.clone();
    let logger: util_maker_core::Logger =
        Arc::new(move |line: &str| sink.lock().expect("журнал").push(line.to_owned()));
    let failure = registry
        .invoke_detailed(
            &ActionRef::new(UNEXPECTED_ID, "run"),
            InvocationSource::Tray,
            Duration::from_secs(5),
            Some(logger),
        )
        .expect_err("падение после чужого ответа");
    assert_eq!(failure.category, FailureCategory::ModuleCrashed);
    let journal = lines.lock().expect("журнал").join("\n");
    assert!(journal.contains("пропущено сообщение"));
    assert!(journal.contains("скрыто"));
    assert!(!journal.contains("secret-key-123"));
}
