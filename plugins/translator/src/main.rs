//! Исполняемый процесс плагина «Переводчик».
//!
//! Процесс отвечает за выделение, языки и показ результата. HTTP-запросы
//! выполняет активный провайдер-плагин, которого переводчик вызывает через
//! контракт `util-maker.translation.provider`.

use serde_json::{json, Value};
use util_maker_sdk::{
    error_codes, run_stdio, HostBridge, HostCallError, InvokeRequest, InvokeResult, Module,
    ModuleInfo, RpcError, EXTENSIONS_CALL, EXTENSIONS_READ, HOST_API_VERSION,
};
use util_maker_translator::service::{
    translate_from_source, CLIPBOARD_CAPABILITY, SELECTION_CAPABILITY, SETTINGS_CAPABILITY,
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
                capabilities: &[
                    SELECTION_CAPABILITY,
                    SETTINGS_CAPABILITY,
                    CLIPBOARD_CAPABILITY,
                    EXTENSIONS_READ,
                    EXTENSIONS_CALL,
                ],
            },
            selection: SelectionController::new(),
        }
    }

    fn with_result(
        request: &InvokeRequest,
        outcome: util_maker_translator::TranslationOutcome,
    ) -> InvokeResult {
        let mut data = outcome.data();
        if let Value::Object(ref mut object) = data {
            object.insert("callId".to_owned(), json!(request.call_id));
            object.insert("actionId".to_owned(), json!(request.action_id));
            object.insert("moduleVersion".to_owned(), json!(MODULE_VERSION));
        }
        InvokeResult::ok(outcome.message()).with_data(data)
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

        // Без моста возможностей источник недоступен. Этот путь остаётся для
        // старого контракта и офлайн-проверки явной причины отказа.
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
                "вызов перевода требует моста возможностей",
            )),
        }
    }

    fn invoke_with_host(
        &mut self,
        request: &InvokeRequest,
        host: &mut dyn HostBridge,
    ) -> Result<InvokeResult, RpcError> {
        if request.action_id != TRANSLATE_ACTION {
            return Err(RpcError::new(
                error_codes::ACTION_NOT_FOUND,
                format!("неизвестное действие: {}", request.action_id),
            ));
        }
        let manual = request.params.get("manualText").and_then(Value::as_str);
        match translate_from_source(host, manual) {
            Ok(outcome) => Ok(Self::with_result(request, outcome)),
            Err(HostCallError::Cancelled) => Ok(InvokeResult::cancelled()),
            Err(error) if error.rpc_code() == error_codes::CAPABILITY_DENIED => {
                self.invoke(request)
            }
            Err(error) => Err(error.into_rpc_error()),
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
