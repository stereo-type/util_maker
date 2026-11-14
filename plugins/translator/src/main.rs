//! Исполняемый процесс плагина «Переводчик».
//!
//! На этой ревизии процесс закрывает границу IPC: подтверждает ID модуля,
//! принимает действие `translate` и использует переносимый
//! [`SelectionController`]. Получение выделения через UI Automation и окно
//! результата подключаются отдельной задачей, поэтому текущий снимок источника
//! явно сообщает о потере контекста и не отправляет HTTP-запросов.

use serde_json::json;
use util_maker_sdk::{
    error_codes, run_stdio, InvokeRequest, InvokeResult, Module, ModuleInfo, RpcError,
    HOST_API_VERSION,
};
use util_maker_translator::{BlockReason, SelectionController, SourceSnapshot, StartOutcome};

/// Стабильный ID модуля из манифеста.
const MODULE_ID: &str = "org.utilmaker.translator";
/// Стабильный ID действия из манифеста.
const TRANSLATE_ACTION: &str = "translate";
/// Версия процесса. Тестовые пакеты могут переопределить её при сборке.
const MODULE_VERSION: &str = match option_env!("UTIL_MAKER_MODULE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

struct TranslatorModule {
    info: ModuleInfo,
    selection: SelectionController,
}

impl TranslatorModule {
    fn new() -> Self {
        Self {
            info: ModuleInfo {
                id: MODULE_ID,
                version: MODULE_VERSION,
                host_api: HOST_API_VERSION,
                capabilities: &[],
            },
            selection: SelectionController::new(),
        }
    }

    fn blocked_result(reason: BlockReason, request: &InvokeRequest) -> InvokeResult {
        InvokeResult::ok(reason.message()).with_data(json!({
            "callId": request.call_id,
            "outcome": "blocked",
            "reason": reason.code(),
            "source": request.context.source.as_str(),
            "moduleVersion": MODULE_VERSION,
            "httpRequests": 0,
        }))
    }
}

impl Module for TranslatorModule {
    fn info(&self) -> &ModuleInfo {
        &self.info
    }

    fn invoke(&mut self, request: &InvokeRequest) -> Result<InvokeResult, RpcError> {
        if request.action_id != TRANSLATE_ACTION {
            return Err(RpcError::new(
                error_codes::ACTION_NOT_FOUND,
                format!("неизвестное действие: {}", request.action_id),
            ));
        }

        // UI Automation подключается в задаче о выделении исходного окна.
        // До этого явно фиксируем потерю контекста: это гарантирует, что
        // ни один провайдер не получит текст и сетевой запрос не возникнет.
        match self.selection.start(SourceSnapshot::ContextLost) {
            StartOutcome::Blocked(reason) => Ok(Self::blocked_result(reason, request)),
            StartOutcome::AwaitSelection => Ok(InvokeResult::ok(
                "Выделите текст в исходном приложении одним жестом мыши.",
            )
            .with_data(json!({
                "callId": request.call_id,
                "outcome": "awaiting-selection",
                "httpRequests": 0,
            }))),
            StartOutcome::ManualInput => Ok(InvokeResult::ok(
                "Вставьте или введите текст и явно подтвердите перевод.",
            )
            .with_data(json!({
                "callId": request.call_id,
                "outcome": "manual-input",
                "httpRequests": 0,
            }))),
            StartOutcome::Translate(_) => Err(RpcError::new(
                error_codes::INTERNAL_ERROR,
                "процесс переводчика ещё не подключён к профилю провайдера",
            )),
        }
    }
}

fn main() {
    eprintln!(
        "[translator-module] старт: id {MODULE_ID}, версия {MODULE_VERSION}, API {HOST_API_VERSION}"
    );
    let mut module = TranslatorModule::new();
    match run_stdio(&mut module) {
        Ok(()) => eprintln!("[translator-module] штатное завершение"),
        Err(error) => {
            eprintln!("[translator-module] ошибка stdio: {error}");
            std::process::exit(1);
        }
    }
}
