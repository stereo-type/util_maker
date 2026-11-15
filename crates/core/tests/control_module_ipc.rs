//! Проверка контракта IPC без рабочего стола: настоящий процесс контрольного
//! модуля, рукопожатие и handler действия через stdin/stdout.

mod support;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use util_maker_core::ipc::ModuleProcess;
use util_maker_core::registry::{ActionRef, Registry};
use util_maker_core::Manifest;
use util_maker_sdk::{InvocationSource, InvokeStatus, HOST_API_VERSION};

const MODULE_ID: &str = "org.utilmaker.control";

fn module_binary() -> PathBuf {
    support::binary("control-module")
}

fn manifest_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/control/manifest.json")
        .canonicalize()
        .expect("манифест контрольного модуля")
}

#[test]
fn control_handler_is_invoked_over_ipc() {
    let executable = module_binary();
    assert!(
        executable.is_file(),
        "не найден процесс контрольного модуля {}; соберите workspace: cargo test --workspace",
        executable.display()
    );

    let mut process = ModuleProcess::spawn(&executable).expect("запуск контрольного модуля");
    let initialized = process
        .initialize(MODULE_ID)
        .expect("рукопожатие по протоколу v1");
    assert_eq!(initialized.module_id, MODULE_ID);
    assert_eq!(initialized.api_version, "1.1.0");

    let result = process
        .invoke(
            "test-call-1",
            "ping",
            InvocationSource::Hotkey,
            Duration::from_secs(5),
        )
        .expect("handler контрольного модуля");
    assert_eq!(result.status, InvokeStatus::Ok);
    let message = result.message.expect("сообщение результата");
    assert!(
        message.contains("Контрольный модуль на связи"),
        "неожиданный ответ модуля: {message}"
    );
    let data = result.data.expect("данные результата");
    assert_eq!(data["source"], "hotkey");
    assert!(data["pid"].as_u64().is_some());

    process.shutdown();
}

#[test]
fn unknown_action_returns_machine_readable_error() {
    let executable = module_binary();
    if !executable.is_file() {
        return;
    }
    let mut process = ModuleProcess::spawn(&executable).expect("запуск контрольного модуля");
    process.initialize(MODULE_ID).expect("рукопожатие");
    let error = process
        .invoke(
            "test-call-2",
            "unknown-action",
            InvocationSource::Tray,
            Duration::from_secs(5),
        )
        .expect_err("неизвестное действие должно вернуть ошибку");
    match error {
        util_maker_core::CoreError::Module { code, .. } => {
            assert_eq!(code, util_maker_sdk::error_codes::ACTION_NOT_FOUND);
        }
        other => panic!("ожидалась ошибка модуля, получено: {other}"),
    }
    process.shutdown();
}

#[test]
fn registry_routes_action_from_manifest_without_starting_module() {
    let manifest = Manifest::load(&manifest_path()).expect("манифест");
    let executable = module_binary();
    assert!(
        executable.is_file(),
        "не найден процесс контрольного модуля {}",
        executable.display()
    );

    let mut registry = Registry::new();
    registry
        .add(manifest, executable)
        .expect("регистрация контрольного модуля");
    let target = ActionRef::new(MODULE_ID, "ping");
    let (_entry, action) = registry.action(&target).expect("действие ping");
    assert_eq!(action.name, "Проверить связь");
    assert!(registry
        .action(&ActionRef::new(MODULE_ID, "missing"))
        .is_none());

    let outcome = registry
        .invoke(
            &target,
            InvocationSource::Hotkey,
            Duration::from_secs(5),
            None,
        )
        .expect("вызов через реестр");
    assert_eq!(outcome.status, InvokeStatus::Ok);
    assert!(outcome.message.contains("на связи"));
    assert_eq!(outcome.module_id, MODULE_ID);
    assert!(outcome.elapsed > Duration::ZERO);
}

#[test]
fn host_api_version_is_advertised() {
    assert_eq!(HOST_API_VERSION, "1.1.0");
}

#[test]
fn control_action_can_be_invoked_repeatedly() {
    let manifest = Manifest::load(&manifest_path()).expect("манифест");
    let executable = module_binary();
    assert!(
        executable.is_file(),
        "не найден процесс контрольного модуля {}",
        executable.display()
    );

    let mut registry = Registry::new();
    registry
        .add(manifest, executable)
        .expect("регистрация контрольного модуля");

    let mut call_ids = BTreeSet::new();
    let mut previous_pid = None;
    for attempt in 1..=3 {
        let outcome = registry
            .invoke(
                &ActionRef::new(MODULE_ID, "ping"),
                InvocationSource::Tray,
                Duration::from_secs(5),
                None,
            )
            .unwrap_or_else(|error| panic!("вызов {attempt}: {error}"));
        assert_eq!(outcome.status, InvokeStatus::Ok);
        assert!(
            call_ids.insert(outcome.call_id.clone()),
            "идентификатор вызова {} повторился",
            outcome.call_id
        );

        let data = outcome.data.expect("данные результата");
        assert_eq!(data["callId"], outcome.call_id);
        assert_eq!(data["source"], "tray");
        let pid =
            u32::try_from(data["pid"].as_u64().expect("PID процесса")).expect("PID в пределах u32");
        if let Some(previous) = previous_pid {
            assert_ne!(
                previous, pid,
                "повторный вызов должен запускать новый процесс"
            );
        }
        previous_pid = Some(pid);

        #[cfg(target_os = "linux")]
        assert!(
            !PathBuf::from(format!("/proc/{pid}")).exists(),
            "процесс {pid} остался после завершения вызова"
        );
    }
}
