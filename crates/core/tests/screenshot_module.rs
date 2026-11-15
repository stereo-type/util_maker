//! Граница процесса плагина «Скриншоты»: рукопожатие, действие и ошибки.

mod support;

use std::path::PathBuf;
use std::time::Duration;

use util_maker_core::ipc::ModuleProcess;
use util_maker_core::{CoreError, Manifest};
use util_maker_sdk::InvocationSource;

const MODULE_ID: &str = "org.utilmaker.screenshot";

fn manifest_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/screenshot/manifest.json")
        .canonicalize()
        .expect("манифест плагина «Скриншоты»")
}

#[test]
fn manifest_is_valid_and_declares_action() {
    let manifest = Manifest::load(&manifest_path()).expect("манифест");
    assert_eq!(manifest.id, MODULE_ID);
    let action = manifest.action("capture-region").expect("действие");
    assert_eq!(action.name, "Снять область");
    assert_eq!(
        manifest.resolved_entrypoint("windows-x64").unwrap(),
        PathBuf::from("screenshot-plugin.exe")
    );
}

#[test]
fn manifest_declares_own_icon() {
    let path = manifest_path();
    let manifest = Manifest::load(&path).expect("манифест");
    assert_eq!(manifest.icon.as_deref(), Some("assets/icon.png"));
    let icon = path.parent().expect("каталог").join("assets/icon.png");
    let bytes = std::fs::read(&icon).expect("иконка «Скриншотов»");
    assert!(
        bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "иконка «Скриншотов» не PNG"
    );
    let app_icon =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apps/desktop/icons/32x32.png");
    let app_bytes = std::fs::read(&app_icon).expect("иконка приложения");
    assert_ne!(
        bytes, app_bytes,
        "иконка «Скриншотов» повторяет логотип приложения"
    );
}

#[test]
fn process_reports_missing_frame_and_unknown_action() {
    let executable = support::binary("screenshot-plugin");
    assert!(
        executable.is_file(),
        "не найден процесс {}",
        executable.display()
    );
    let mut process = ModuleProcess::spawn(&executable).expect("запуск плагина");
    let initialized = process.initialize(MODULE_ID).expect("рукопожатие");
    assert_eq!(initialized.module_id, MODULE_ID);
    assert_eq!(initialized.api_version, "1.0.0");

    let error = process
        .invoke(
            "screenshot-call-1",
            "capture-region",
            InvocationSource::Hotkey,
            Duration::from_secs(5),
        )
        .expect_err("без источника кадра действие должно вернуть ошибку");
    match error {
        CoreError::Module { code, message } => {
            assert_eq!(code, 2001);
            assert!(message.contains("источник кадра"));
        }
        other => panic!("ожидалась ошибка плагина, получено: {other}"),
    }

    let error = process
        .invoke(
            "screenshot-call-2",
            "unknown-action",
            InvocationSource::Tray,
            Duration::from_secs(5),
        )
        .expect_err("неизвестное действие");
    match error {
        CoreError::Module { code, .. } => {
            assert_eq!(code, util_maker_sdk::error_codes::ACTION_NOT_FOUND);
        }
        other => panic!("ожидалась ошибка ACTION_NOT_FOUND, получено: {other}"),
    }

    process.shutdown();
}
