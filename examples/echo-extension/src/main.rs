//! Пример плагина-расширения: собственных действий нет, есть вклад в точку
//! `echo` родителя `org.example.echo`. Модуль использует только публичный SDK.

use serde_json::json;
use util_maker_sdk::{
    error_codes, run_stdio, ExtensionInvokeRequest, ExtensionInvokeResult, InvokeRequest,
    InvokeResult, Module, ModuleInfo, RpcError, HOST_API_VERSION,
};

const MODULE_VERSION: &str = match option_env!("UTIL_MAKER_MODULE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};
const INFO: ModuleInfo = ModuleInfo {
    id: "org.example.echo-extension",
    version: MODULE_VERSION,
    host_api: HOST_API_VERSION,
    capabilities: &[],
};

struct EchoExtension;

impl Module for EchoExtension {
    fn info(&self) -> &ModuleInfo {
        &INFO
    }

    fn invoke(&mut self, _request: &InvokeRequest) -> Result<InvokeResult, RpcError> {
        Err(RpcError::new(
            error_codes::ACTION_NOT_FOUND,
            "у расширения нет собственных действий",
        ))
    }

    fn extension_invoke(
        &mut self,
        request: &ExtensionInvokeRequest,
    ) -> Result<ExtensionInvokeResult, RpcError> {
        if request.point_id != "echo" || request.operation != "echo" {
            return Err(RpcError::new(
                error_codes::METHOD_NOT_FOUND,
                format!(
                    "операция {} для точки {} не поддерживается",
                    request.operation, request.point_id
                ),
            ));
        }
        let text = request
            .params
            .get("text")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| RpcError::new(error_codes::INVALID_PARAMS, "не передан текст"))?;
        Ok(ExtensionInvokeResult {
            data: json!({ "text": text, "callId": request.call_id }),
        })
    }
}

fn main() {
    eprintln!(
        "[echo-extension] старт: {}, версия {}, API {}",
        INFO.id, INFO.version, INFO.host_api
    );
    if let Err(error) = run_stdio(&mut EchoExtension) {
        eprintln!("[echo-extension] ошибка stdio: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use util_maker_sdk::run_stdio_with_io;

    #[test]
    fn answers_echo_operation_without_actions() {
        let input = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"hostApi\":\"1.1.0\",\"moduleId\":\"org.example.echo-extension\"}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"extension.invoke\",\"params\":{\"callId\":\"call-1\",\"pointId\":\"echo\",\"operation\":\"echo\",\"params\":{\"text\":\"hello\"}}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"invoke\",\"params\":{\"callId\":\"call-2\",\"actionId\":\"missing\"}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"shutdown\",\"params\":null}\n",
        );
        let mut output = Vec::new();
        run_stdio_with_io(&mut EchoExtension, Cursor::new(input), &mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines[1]["result"]["data"]["text"], "hello");
        assert_eq!(lines[1]["result"]["data"]["callId"], "call-1");
        assert_eq!(lines[2]["error"]["code"], error_codes::ACTION_NOT_FOUND);
    }
}
