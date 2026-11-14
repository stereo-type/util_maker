//! Каталог активных пакетов: снимки версий, отключение и постоянное состояние.

mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use util_maker_core::registry::ActionRef;
use util_maker_core::{FailureCategory, Manifest, ModuleEntry, Registry};
use util_maker_sdk::{Action, InvocationSource, InvokeStatus};

const CONTROL_ID: &str = "org.utilmaker.control";
const FIXTURE_ID: &str = "org.utilmaker.fixture";

fn manifest_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/control/manifest.json")
}

fn manifest(id: &str, version: &str, action: &str) -> Manifest {
    let mut manifest = Manifest::load(&manifest_path()).expect("манифест");
    manifest.id = id.to_owned();
    manifest.name = format!("Тестовый модуль {id}");
    manifest.version = version.to_owned();
    manifest.actions = vec![Action {
        id: action.to_owned(),
        name: format!("Действие {action}"),
        description: None,
        recommended_hotkeys: Vec::new(),
    }];
    manifest
}

fn control() -> ModuleEntry {
    ModuleEntry::new(
        manifest(CONTROL_ID, "1.0.0", "ping"),
        support::binary("control-module"),
        true,
    )
}

fn fixture(version: &str, arguments: &[&str]) -> ModuleEntry {
    ModuleEntry::new(
        manifest(FIXTURE_ID, version, "any"),
        support::binary("fixture-module"),
        true,
    )
    .with_arguments(arguments.iter().map(|value| (*value).to_owned()).collect())
}

fn wait_until_ready(logger_lines: &mpsc::Receiver<String>) {
    for _ in 0..100 {
        let line = logger_lines
            .recv_timeout(Duration::from_secs(2))
            .expect("журнал запуска");
        if line.contains("[ipc] модуль") && line.contains("готов") {
            return;
        }
    }
    panic!("модуль не подтвердил готовность");
}

#[test]
fn catalog_lists_actions_without_starting_processes_and_routes_both_modules() {
    let registry = Registry::new();
    registry
        .replace_packages(vec![control(), fixture("1.0.0", &["ok"])])
        .expect("каталог");
    let actions = registry.actions();
    assert!(actions.iter().any(|entry| entry.module_id == CONTROL_ID));
    assert!(actions.iter().any(|entry| entry.module_id == FIXTURE_ID));

    let control_outcome = registry
        .invoke(
            &ActionRef::new(CONTROL_ID, "ping"),
            InvocationSource::Hotkey,
            Duration::from_secs(5),
            None,
        )
        .expect("контрольный модуль");
    assert_eq!(control_outcome.status, InvokeStatus::Ok);
    assert_eq!(control_outcome.module_version, "1.0.0");

    let fixture_outcome = registry
        .invoke(
            &ActionRef::new(FIXTURE_ID, "any"),
            InvocationSource::Tray,
            Duration::from_secs(5),
            None,
        )
        .expect("второй модуль");
    assert_eq!(fixture_outcome.status, InvokeStatus::Ok);
    assert!(fixture_outcome.cold);
}

#[test]
fn started_call_keeps_old_version_until_it_finishes() {
    let registry = Arc::new(Registry::new());
    registry
        .replace_packages(vec![fixture("1.0.0", &["late-invoke"])])
        .expect("старая версия");
    let (sender, lines) = mpsc::channel();
    let logger: util_maker_core::Logger = Arc::new(move |line: &str| {
        let _ = sender.send(line.to_owned());
    });
    let worker = {
        let registry = registry.clone();
        std::thread::spawn(move || {
            registry.invoke_detailed(
                &ActionRef::new(FIXTURE_ID, "any"),
                InvocationSource::Hotkey,
                Duration::from_secs(5),
                Some(logger),
            )
        })
    };
    wait_until_ready(&lines);
    assert!(!registry.can_switch(FIXTURE_ID));
    registry
        .replace_packages(vec![fixture("2.0.0", &["late-invoke"])])
        .expect("новая версия");
    let outcome = worker.join().expect("вызов").expect("исход");
    assert_eq!(outcome.module_version, "1.0.0");
    assert!(registry.can_switch(FIXTURE_ID));
}

#[test]
fn disabling_plugin_cancels_active_call_and_is_idempotent() {
    let registry = Arc::new(Registry::new());
    registry
        .replace_packages(vec![fixture("1.0.0", &["hang"])])
        .expect("каталог");
    let (sender, lines) = mpsc::channel();
    let logger: util_maker_core::Logger = Arc::new(move |line: &str| {
        let _ = sender.send(line.to_owned());
    });
    let worker = {
        let registry = registry.clone();
        std::thread::spawn(move || {
            registry.invoke_detailed(
                &ActionRef::new(FIXTURE_ID, "any"),
                InvocationSource::Hotkey,
                Duration::from_secs(30),
                Some(logger),
            )
        })
    };
    wait_until_ready(&lines);
    assert_eq!(registry.active_calls(FIXTURE_ID), 1);
    assert!(registry.set_enabled(FIXTURE_ID, false).expect("отключение"));
    assert!(!registry.set_enabled(FIXTURE_ID, false).expect("повтор"));
    let failure = worker.join().expect("вызов").expect_err("отменён");
    assert_eq!(failure.category, FailureCategory::Cancelled);
    assert!(
        registry
            .invoke_detailed(
                &ActionRef::new(FIXTURE_ID, "any"),
                InvocationSource::Hotkey,
                Duration::from_secs(1),
                None,
            )
            .is_err(),
        "отключённый пакет не должен запускаться"
    );

    registry
        .replace_packages(vec![fixture("1.0.0", &["ok"])])
        .expect("каталог после отключения");
    registry.set_enabled(FIXTURE_ID, true).expect("включение");
    let outcome = registry
        .invoke(
            &ActionRef::new(FIXTURE_ID, "any"),
            InvocationSource::Hotkey,
            Duration::from_secs(5),
            None,
        )
        .expect("вызов после включения");
    assert_eq!(outcome.status, InvokeStatus::Ok);
}

#[test]
fn disabled_state_survives_registry_reopen() {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let directory = std::env::temp_dir().join(format!(
        "util-maker-core-install-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("каталог");
    let state = directory.join("packages.json");

    let registry = Registry::with_state_path(&state).expect("реестр");
    registry
        .replace_packages(vec![control(), fixture("1.0.0", &["ok"])])
        .expect("каталог");
    registry.set_enabled(FIXTURE_ID, false).expect("отключение");

    let reopened = Registry::with_state_path(&state).expect("повторное открытие");
    reopened
        .replace_packages(vec![control(), fixture("1.0.0", &["ok"])])
        .expect("каталог после перезапуска");
    let actions = reopened.actions();
    assert!(actions.iter().any(|entry| entry.module_id == CONTROL_ID));
    assert!(!actions.iter().any(|entry| entry.module_id == FIXTURE_ID));
    assert_eq!(
        reopened
            .invoke_detailed(
                &ActionRef::new(FIXTURE_ID, "any"),
                InvocationSource::Hotkey,
                Duration::from_secs(1),
                None,
            )
            .expect_err("отключён")
            .category,
        FailureCategory::ModuleDisabled
    );

    reopened.set_enabled(FIXTURE_ID, true).expect("включение");
    assert!(reopened
        .actions()
        .iter()
        .any(|entry| entry.module_id == FIXTURE_ID));
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn missing_action_and_unknown_module_are_reported_before_spawn() {
    let registry = Registry::new();
    registry
        .replace_packages(vec![control()])
        .expect("контрольный модуль");
    let missing = registry
        .invoke_detailed(
            &ActionRef::new(CONTROL_ID, "missing"),
            InvocationSource::Tray,
            Duration::from_secs(1),
            None,
        )
        .expect_err("нет действия");
    assert_eq!(missing.category, FailureCategory::ActionUnavailable);
    assert_eq!(missing.module_id, CONTROL_ID);
    assert_eq!(missing.action_id, "missing");

    let unknown = registry
        .invoke_detailed(
            &ActionRef::new("org.example.unknown", "any"),
            InvocationSource::Tray,
            Duration::from_secs(1),
            None,
        )
        .expect_err("нет модуля");
    assert_eq!(unknown.category, FailureCategory::ModuleNotFound);
    assert_eq!(unknown.module_id, "org.example.unknown");
}
