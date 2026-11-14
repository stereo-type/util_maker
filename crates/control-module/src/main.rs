//! Контрольный модуль прототипа: подтверждает рукопожатие и отвечает на `ping`.
//!
//! Процесс не открывает окон и не обращается к рабочему столу. Его можно
//! запустить в тесте на Ubuntu и вызвать через JSON-RPC по stdin/stdout.

use std::time::Instant;

use serde_json::json;
use util_maker_sdk::{
    error_codes, run_stdio, InvokeRequest, InvokeResult, Module, ModuleInfo, RpcError,
    HOST_API_VERSION,
};

struct ControlModule {
    info: ModuleInfo,
    started: Instant,
}

impl ControlModule {
    fn new() -> Self {
        Self {
            info: ModuleInfo {
                id: "org.utilmaker.control",
                version: env!("CARGO_PKG_VERSION"),
                host_api: HOST_API_VERSION,
                capabilities: &[],
            },
            started: Instant::now(),
        }
    }
}

impl Module for ControlModule {
    fn info(&self) -> &ModuleInfo {
        &self.info
    }

    fn invoke(&mut self, request: &InvokeRequest) -> Result<InvokeResult, RpcError> {
        match request.action_id.as_str() {
            "ping" => {
                let pid = std::process::id();
                let uptime_ms = self.started.elapsed().as_millis();
                Ok(InvokeResult::ok(format!(
                    "Контрольный модуль на связи. PID {pid}, версия {}, принят вызов {}. Аптайм процесса {uptime_ms} мс.",
                    self.info.version, request.call_id
                ))
                .with_data(json!({
                    "pid": pid,
                    "moduleVersion": self.info.version,
                    "apiVersion": self.info.host_api,
                    "source": request.context.source,
                    "uptimeMs": uptime_ms as u64,
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
        "[control-module] старт: id org.utilmaker.control, версия {}, API {}",
        env!("CARGO_PKG_VERSION"),
        HOST_API_VERSION
    );
    let mut module = ControlModule::new();
    match run_stdio(&mut module) {
        Ok(()) => eprintln!("[control-module] штатное завершение"),
        Err(error) => {
            eprintln!("[control-module] ошибка stdio: {error}");
            std::process::exit(1);
        }
    }
}
