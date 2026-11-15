//! Тестовый плагин-расширение: операции `echo`, `settings`, `fail`, `sleep`.
//!
//! Используется интеграционными тестами ядра. Собственных действий у пакета
//! нет, возможности ограничены `settings.read`.

use std::time::Duration;

use serde_json::json;
use util_maker_sdk::{
    error_codes, host_methods, run_stdio, ExtensionInvokeRequest, ExtensionInvokeResult,
    HostBridge, InvokeRequest, InvokeResult, Module, ModuleInfo, RpcError, HOST_API_VERSION,
};

fn module_id() -> &'static str {
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        if argument == "--module-id" {
            if let Some(value) = arguments.next() {
                return Box::leak(value.into_boxed_str());
            }
        }
    }
    "org.example.fixture-extension"
}

fn info() -> ModuleInfo {
    ModuleInfo {
        id: module_id(),
        version: "0.1.0",
        host_api: HOST_API_VERSION,
        capabilities: &["settings.read"],
    }
}

struct FixtureExtension {
    info: ModuleInfo,
}

impl Module for FixtureExtension {
    fn info(&self) -> &ModuleInfo {
        &self.info
    }

    fn invoke(&mut self, _request: &InvokeRequest) -> Result<InvokeResult, RpcError> {
        Err(RpcError::new(
            error_codes::ACTION_NOT_FOUND,
            "у тестового расширения нет действий",
        ))
    }

    fn extension_invoke_with_host(
        &mut self,
        request: &ExtensionInvokeRequest,
        host: &mut dyn HostBridge,
    ) -> Result<ExtensionInvokeResult, RpcError> {
        match request.operation.as_str() {
            "echo" => Ok(ExtensionInvokeResult {
                data: json!({
                    "text": request.params.get("text").cloned().unwrap_or(serde_json::Value::Null),
                    "callId": request.call_id,
                    "pointId": request.point_id,
                }),
            }),
            "settings" => {
                let value = host
                    .request("settings.read", host_methods::SETTINGS_READ, json!({}))
                    .map_err(|error| error.into_rpc_error())?;
                Ok(ExtensionInvokeResult { data: value })
            }
            "fail" => Err(RpcError::new(
                error_codes::EXTENSION_FAILED,
                "тестовый отказ плагина",
            )),
            "sleep" => {
                let ms = request
                    .params
                    .get("ms")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(200);
                std::thread::sleep(Duration::from_millis(ms));
                Ok(ExtensionInvokeResult {
                    data: json!({"sleptMs": ms}),
                })
            }
            other => Err(RpcError::new(
                error_codes::METHOD_NOT_FOUND,
                format!("неизвестная операция {other}"),
            )),
        }
    }
}

fn main() {
    let mut module = FixtureExtension { info: info() };
    if let Err(error) = run_stdio(&mut module) {
        eprintln!("[fixture-extension] ошибка stdio: {error}");
        std::process::exit(1);
    }
}
