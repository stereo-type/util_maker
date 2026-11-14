//! Пример стороннего микроприложения с двумя действиями.
//!
//! Модуль не зависит от исходников хоста: он использует только публичный
//! `util-maker-sdk` и протокол JSON-RPC v1. Действие `hello` возвращает
//! приветствие, `clock` — идентификатор вызова, PID и время процесса.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use util_maker_sdk::{
    error_codes, run_stdio, InvokeRequest, InvokeResult, Module, ModuleInfo, RpcError,
    HOST_API_VERSION,
};

const MODULE_VERSION: &str = match option_env!("UTIL_MAKER_MODULE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};
const INFO: ModuleInfo = ModuleInfo {
    id: "com.example.echo",
    version: MODULE_VERSION,
    host_api: HOST_API_VERSION,
    capabilities: &[],
};

struct EchoModule;

impl Module for EchoModule {
    fn info(&self) -> &ModuleInfo {
        &INFO
    }

    fn invoke(&mut self, request: &InvokeRequest) -> Result<InvokeResult, RpcError> {
        match request.action_id.as_str() {
            "hello" => Ok(
                InvokeResult::ok("Привет из стороннего плагина").with_data(json!({
                    "callId": request.call_id,
                    "source": request.context.source.as_str(),
                    "moduleVersion": MODULE_VERSION,
                })),
            ),
            "clock" => {
                let unix_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|error| {
                        RpcError::new(
                            error_codes::INTERNAL_ERROR,
                            format!("системное время: {error}"),
                        )
                    })?
                    .as_millis() as u64;
                Ok(InvokeResult::ok("Текущее время получено").with_data(json!({
                    "callId": request.call_id,
                    "unixMs": unix_ms,
                    "pid": std::process::id(),
                })))
            }
            other => Err(RpcError::new(
                error_codes::ACTION_NOT_FOUND,
                format!("неизвестное действие: {other}"),
            )),
        }
    }
}

fn main() {
    eprintln!(
        "[echo-plugin] старт: {}, версия {}, API {}",
        INFO.id, INFO.version, INFO.host_api
    );
    if let Err(error) = run_stdio(&mut EchoModule) {
        eprintln!("[echo-plugin] ошибка stdio: {error}");
        std::process::exit(1);
    }
}
