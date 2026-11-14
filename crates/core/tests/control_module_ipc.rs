//! Проверка контракта IPC без рабочего стола: настоящий процесс контрольного
//! модуля, рукопожатие и handler действия через stdin/stdout.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use util_maker_core::ipc::ModuleProcess;
use util_maker_core::registry::{ActionRef, Registry};
use util_maker_core::Manifest;
use util_maker_sdk::{InvocationSource, InvokeStatus, HOST_API_VERSION};

const MODULE_ID: &str = "org.utilmaker.control";

fn module_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("UTIL_MAKER_CONTROL_MODULE") {
        return PathBuf::from(path);
    }
    let executable = std::env::current_exe().expect("путь к тестовому исполняемому файлу");
    let target_root = executable
        .ancestors()
        .find(|path| path.file_name().is_some_and(|name| name == "target"))
        .map(PathBuf::from)
        .expect("каталог target в пути тестового файла");
    let name = if cfg!(windows) {
        "control-module.exe"
    } else {
        "control-module"
    };
    let mut found = Vec::new();
    collect_binaries(&target_root, name, 0, &mut found);
    found
        .into_iter()
        .max_by_key(|path| fs::metadata(path).and_then(|meta| meta.modified()).ok())
        .unwrap_or_else(|| target_root.join(name))
}

/// Ищет бинарник по всему target: раскладка Cargo может отличаться между версиями.
fn collect_binaries(directory: &Path, name: &str, depth: usize, found: &mut Vec<PathBuf>) {
    if depth > 8 {
        return;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if entry.file_name().to_string_lossy() == "incremental" {
                continue;
            }
            collect_binaries(&entry.path(), name, depth + 1, found);
        } else if file_type.is_file() && entry.file_name().to_string_lossy() == name {
            found.push(entry.path());
        }
    }
}

fn manifest_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../control-module/manifest.json")
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
    assert_eq!(initialized.api_version, "1.0.0");

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
    assert_eq!(HOST_API_VERSION, "1.0.0");
}
