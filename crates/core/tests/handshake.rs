//! Рукопожатие SDK: совместимая версия, несовместимая версия, повреждённый
//! JSON, ответ после таймаута и отделение диагностики от stdout JSON-RPC.

mod support;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use util_maker_core::ipc::ModuleProcess;
use util_maker_core::{CoreError, Logger};
use util_maker_sdk::INITIALIZE_TIMEOUT;

const FIXTURE_ID: &str = "org.utilmaker.fixture";
const CONTROL_ID: &str = "org.utilmaker.control";

fn fixture(mode: &str) -> ModuleProcess {
    let executable = support::binary("fixture-module");
    assert!(
        executable.is_file(),
        "не найден тестовый модуль {}; соберите workspace: cargo test --workspace",
        executable.display()
    );
    ModuleProcess::spawn_with_args(&executable, &[mode], Arc::new(|_: &str| {}))
        .expect("запуск тестового модуля")
}

#[test]
fn compatible_handshake_fits_three_seconds() {
    let mut process = fixture("ok");
    let started = Instant::now();
    let initialized = process
        .initialize(FIXTURE_ID)
        .expect("совместимое рукопожатие");
    assert_eq!(initialized.api_version, "1.1.0");
    assert_eq!(initialized.module_id, FIXTURE_ID);
    assert!(
        started.elapsed() <= INITIALIZE_TIMEOUT,
        "рукопожатие заняло {:?}",
        started.elapsed()
    );
    process.shutdown();
}

#[test]
fn host_rejects_incompatible_module_api() {
    let mut process = fixture("incompatible");
    let error = process
        .initialize(FIXTURE_ID)
        .expect_err("модуль с API 2.x должен быть отклонён");
    assert!(
        matches!(error, CoreError::IncompatibleApi(_)),
        "ожидалась ошибка версии API, получено: {error}"
    );
    assert_eq!(error.code(), "host_api_incompatible");
}

#[test]
fn module_rejection_of_host_api_is_visible_as_version_error() {
    let mut process = fixture("module-incompatible");
    let error = process
        .initialize(FIXTURE_ID)
        .expect_err("модуль должен отклонить несовместимый hostApi");
    assert_eq!(error.code(), "host_api_incompatible");
}

#[test]
fn corrupted_json_is_protocol_error() {
    let mut process = fixture("garbage");
    let error = process
        .initialize(FIXTURE_ID)
        .expect_err("повреждённый JSON должен завершить рукопожатие");
    assert!(
        matches!(error, CoreError::Protocol(_)),
        "ожидалась ошибка протокола, получено: {error}"
    );
    assert_eq!(error.code(), "protocol_violation");
}

#[test]
fn late_handshake_times_out_and_next_try_succeeds() {
    let mut process = fixture("late");
    let started = Instant::now();
    let error = process
        .initialize(FIXTURE_ID)
        .expect_err("ответ после 3 с должен дать таймаут");
    assert!(
        matches!(error, CoreError::Timeout { .. }),
        "ожидался таймаут, получено: {error}"
    );
    assert_eq!(error.code(), "module_timeout");
    assert!(
        started.elapsed() >= INITIALIZE_TIMEOUT,
        "таймаут сработал раньше трёх секунд: {:?}",
        started.elapsed()
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "таймаут сработал слишком поздно: {:?}",
        started.elapsed()
    );

    // Поздний ответ на первый запрос должен быть пропущен, а повторное
    // рукопожатие — завершиться успешно.
    let initialized = process
        .initialize(FIXTURE_ID)
        .expect("повторное рукопожатие после таймаута");
    assert_eq!(initialized.api_version, "1.1.0");
    process.shutdown();
}

#[test]
fn diagnostics_of_control_process_stay_out_of_stdout() {
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = lines.clone();
    let logger: Logger = Arc::new(move |line: &str| {
        sink.lock().expect("журнал теста").push(line.to_owned());
    });
    let executable = support::binary("control-module");
    assert!(
        executable.is_file(),
        "не найден контрольный модуль {}",
        executable.display()
    );
    let mut process =
        ModuleProcess::spawn_with_logger(&executable, logger).expect("запуск контрольного модуля");

    // Если бы модуль писал диагностику в stdout, разбор JSON-RPC завершился бы
    // ошибкой протокола.
    let initialized = process
        .initialize(CONTROL_ID)
        .expect("рукопожатие при диагностике в stderr");
    assert_eq!(initialized.api_version, "1.1.0");

    // stderr читается отдельным потоком; shutdown дожидается его завершения.
    process.shutdown();
    let captured = lines.lock().expect("журнал теста").clone();
    assert!(
        captured
            .iter()
            .any(|line| line.contains("[module]") && line.contains("control-module")),
        "диагностика модуля не попала в stderr: {captured:?}"
    );
}

#[test]
fn error_codes_of_handshake_failures_are_distinct() {
    let mut incompatible = fixture("incompatible");
    let mut corrupted = fixture("garbage");
    let mut late = fixture("late");

    let version_code = incompatible.initialize(FIXTURE_ID).unwrap_err().code();
    let json_code = corrupted.initialize(FIXTURE_ID).unwrap_err().code();
    let timeout_code = late.initialize(FIXTURE_ID).unwrap_err().code();

    assert_ne!(version_code, json_code);
    assert_ne!(version_code, timeout_code);
    assert_ne!(json_code, timeout_code);
}
