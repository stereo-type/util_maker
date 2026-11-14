//! Вызов действия через IPC: идентификатор вызова, аварийное завершение,
//! отмена зависшего процесса и игнорирование позднего ответа.

mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use util_maker_core::ipc::ModuleProcess;
use util_maker_core::registry::{ActionRef, Registry};
use util_maker_core::Manifest;
use util_maker_sdk::InvocationSource;

const FIXTURE_ID: &str = "org.utilmaker.fixture";

fn fixture(mode: &str) -> ModuleProcess {
    let executable = support::binary("fixture-module");
    assert!(executable.is_file(), "не найден {}", executable.display());
    ModuleProcess::spawn_with_args(&executable, &[mode], Arc::new(|_: &str| {}))
        .expect("запуск тестового модуля")
}

fn control_manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/control/manifest.json")
        .canonicalize()
        .expect("манифест контрольного модуля")
}

#[test]
fn result_contains_call_id() {
    let manifest = Manifest::load(&control_manifest()).expect("манифест");
    let executable = support::binary("control-module");
    assert!(executable.is_file(), "не найден {}", executable.display());
    let mut registry = Registry::new();
    registry.add(manifest, executable).expect("регистрация");

    let outcome = registry
        .invoke(
            &ActionRef::new("org.utilmaker.control", "ping"),
            InvocationSource::Hotkey,
            Duration::from_secs(5),
            None,
        )
        .expect("вызов контрольного модуля");
    assert!(!outcome.call_id.is_empty());
    let data = outcome.data.expect("данные результата");
    assert_eq!(data["callId"], outcome.call_id);
}

#[test]
fn crashed_module_fails_call_and_retry_succeeds() {
    let mut process = fixture("crash");
    process.initialize(FIXTURE_ID).expect("рукопожатие");
    let error = process
        .invoke(
            "crash-1",
            "any",
            InvocationSource::Hotkey,
            Duration::from_secs(5),
        )
        .expect_err("аварийный процесс должен завершить вызов ошибкой");
    assert_eq!(error.code(), "module_exited");
    drop(process);

    let mut retry = fixture("ok");
    retry.initialize(FIXTURE_ID).expect("рукопожатие");
    let result = retry
        .invoke(
            "retry-1",
            "any",
            InvocationSource::Hotkey,
            Duration::from_secs(5),
        )
        .expect("повторный вызов");
    assert!(result.message.unwrap_or_default().contains("retry-1"));
    retry.shutdown();
}

#[test]
fn cancel_kills_hung_process_within_two_seconds() {
    let mut process = fixture("hang");
    process.initialize(FIXTURE_ID).expect("рукопожатие");
    let pid = process.pid();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let canceller = thread::spawn(move || {
        thread::sleep(Duration::from_millis(100));
        flag.store(true, Ordering::SeqCst);
    });

    let started = Instant::now();
    let error = process
        .invoke_cancellable(
            "hang-1",
            "any",
            InvocationSource::Hotkey,
            Duration::from_secs(30),
            &cancel,
        )
        .expect_err("вызов должен быть отменён");
    canceller.join().expect("поток отмены");
    assert_eq!(error.code(), "cancelled");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "отмена заняла {:?}",
        started.elapsed()
    );

    #[cfg(target_os = "linux")]
    assert!(
        !PathBuf::from(format!("/proc/{pid}")).exists(),
        "зависший процесс {pid} ещё существует"
    );
}

#[test]
fn late_response_after_timeout_is_ignored() {
    let mut process = fixture("late-invoke");
    process.initialize(FIXTURE_ID).expect("рукопожатие");
    let error = process
        .invoke(
            "late-1",
            "any",
            InvocationSource::Hotkey,
            Duration::from_millis(100),
        )
        .expect_err("короткий таймаут");
    assert_eq!(error.code(), "module_timeout");

    // Поздний ответ на invoke приходит уже во время следующего запроса и
    // должен быть пропущен по несовпадению id.
    let initialized = process
        .initialize(FIXTURE_ID)
        .expect("рукопожатие после позднего ответа");
    assert_eq!(initialized.api_version, "1.0.0");
    process.shutdown();
}
