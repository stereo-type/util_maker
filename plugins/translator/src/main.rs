//! Исполняемый процесс плагина «Переводчик».
//!
//! На этой ревизии процесс закрывает границу IPC: подтверждает ID модуля,
//! принимает действие `translate` и использует переносимый
//! [`SelectionController`]. Получение выделения через UI Automation и окно
//! результата подключаются отдельной задачей, поэтому текущий снимок источника
//! явно сообщает о потере контекста и не отправляет HTTP-запросов.

use std::sync::atomic::AtomicBool;

use serde_json::{json, Value};
use util_maker_providers::{ProviderHub, ProviderSettings, ReqwestTransport};
use util_maker_sdk::{
    error_codes, run_stdio, HostBridge, InvokeRequest, InvokeResult, Module, ModuleInfo, RpcError,
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

        let snapshot = selection_snapshot();
        match self.selection.start(snapshot) {
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
        let snapshot = host_snapshot(host);
        match self.selection.start(snapshot) {
            StartOutcome::Translate(text) => translate_with_host(host, request, &text),
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
        }
    }
}

fn host_snapshot(host: &mut dyn HostBridge) -> SourceSnapshot {
    let value = match host.request("selection.read", "selection.get", json!({})) {
        Ok(value) => value,
        Err(_) => return SourceSnapshot::ContextLost,
    };
    match value.get("state").and_then(Value::as_str) {
        Some("text") => value
            .get("text")
            .and_then(Value::as_str)
            .map(|text| SourceSnapshot::Selected(text.to_owned()))
            .unwrap_or(SourceSnapshot::Empty),
        Some("empty") => SourceSnapshot::Empty,
        Some("unsupported") => SourceSnapshot::Unsupported,
        Some("password") => SourceSnapshot::Password,
        _ => SourceSnapshot::ContextLost,
    }
}

fn translate_with_host(
    host: &mut dyn HostBridge,
    request: &InvokeRequest,
    text: &str,
) -> Result<InvokeResult, RpcError> {
    let settings_value = host
        .request("settings.read", "settings.read", json!({}))
        .unwrap_or_else(|_| json!({}));
    let settings = ProviderSettings::from_value(&settings_value)
        .unwrap_or_else(|_| ProviderSettings::defaults());
    let hub = ProviderHub::from_settings(&settings, |reference| {
        let profile = if reference.contains("yandex") {
            "yandex"
        } else {
            "google"
        };
        host.request(
            "secret.read",
            "secret.read",
            json!({"profile": profile, "name": reference}),
        )
        .ok()
        .and_then(|value| {
            value
                .get("value")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
    })
    .map_err(|error| RpcError::new(error_codes::INTERNAL_ERROR, error.to_string()))?;
    if hub.missing_key_profile().is_some() {
        return Ok(InvokeResult::ok(
            "Для выбранного сервиса не задан API-ключ. Откройте настройки переводчика.",
        )
        .with_data(json!({
            "callId": request.call_id,
            "outcome": "missing-key",
            "httpRequests": 0,
        })));
    }
    let transport = ReqwestTransport::new()
        .map_err(|error| RpcError::new(error_codes::INTERNAL_ERROR, error.to_string()))?;
    match hub.translate(text, &transport, &AtomicBool::new(false)) {
        Ok(translation) => Ok(InvokeResult::ok(translation.text.clone()).with_data(json!({
            "callId": request.call_id,
            "outcome": "translated",
            "provider": translation.provider.name(),
            "text": translation.text,
            "httpRequests": 1,
        }))),
        Err(error) => Err(RpcError::new(
            error_codes::INTERNAL_ERROR,
            error.to_string(),
        )),
    }
}

fn selection_snapshot() -> SourceSnapshot {
    #[cfg(windows)]
    {
        use util_maker_platform::SelectionSource;
        if let Ok(mut source) = util_maker_platform::WindowsSelectionSource::new() {
            return match source.snapshot() {
                util_maker_platform::SelectionSnapshot::Text(text) => {
                    SourceSnapshot::Selected(text)
                }
                util_maker_platform::SelectionSnapshot::Empty => SourceSnapshot::Empty,
                util_maker_platform::SelectionSnapshot::Unsupported => SourceSnapshot::Unsupported,
                util_maker_platform::SelectionSnapshot::Password => SourceSnapshot::Password,
                util_maker_platform::SelectionSnapshot::ContextLost => SourceSnapshot::ContextLost,
            };
        }
    }
    SourceSnapshot::ContextLost
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
