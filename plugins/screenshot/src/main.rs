//! Исполняемый процесс плагина «Скриншоты».
//!
//! Захват рабочего стола, overlay и буфер подключаются через возможности хоста
//! на следующем этапе. Сейчас процесс проверяет границу SDK: согласует API и
//! сообщает машинную ошибку, если источник кадра ещё недоступен.

use serde_json::json;
use util_maker_sdk::{
    error_codes, run_stdio, InvokeRequest, InvokeResult, Module, ModuleInfo, RpcError,
    HOST_API_VERSION,
};

const MODULE_ID: &str = "org.utilmaker.screenshot";
const CAPTURE_UNAVAILABLE: i64 = 2001;

const MODULE_VERSION: &str = match option_env!("UTIL_MAKER_MODULE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

struct ScreenshotModule {
    info: ModuleInfo,
}

impl ScreenshotModule {
    fn new() -> Self {
        Self {
            info: ModuleInfo {
                id: MODULE_ID,
                version: MODULE_VERSION,
                host_api: HOST_API_VERSION,
                capabilities: &["screen.capture", "clipboard.write"],
            },
        }
    }
}

impl Module for ScreenshotModule {
    fn info(&self) -> &ModuleInfo {
        &self.info
    }

    fn invoke(&mut self, request: &InvokeRequest) -> Result<InvokeResult, RpcError> {
        match request.action_id.as_str() {
            "capture-region" => Err(RpcError::new(
                CAPTURE_UNAVAILABLE,
                "источник кадра недоступен: захват рабочего стола ещё не подключён",
            )
            .with_data(json!({
                "callId": request.call_id,
                "actionId": request.action_id,
                "reason": "capture_unavailable"
            }))),
            other => Err(RpcError::new(
                error_codes::ACTION_NOT_FOUND,
                format!("неизвестное действие: {other}"),
            )),
        }
    }
}

fn main() {
    eprintln!(
        "[screenshot-plugin] старт: id {MODULE_ID}, версия {MODULE_VERSION}, API {HOST_API_VERSION}"
    );
    let mut module = ScreenshotModule::new();
    match run_stdio(&mut module) {
        Ok(()) => eprintln!("[screenshot-plugin] штатное завершение"),
        Err(error) => {
            eprintln!("[screenshot-plugin] ошибка stdio: {error}");
            std::process::exit(1);
        }
    }
}
