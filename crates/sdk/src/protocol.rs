//! Сообщения JSON-RPC 2.0 контракта микроприложения v1.
//!
//! Один объект UTF-8 на строку, предел [`MAX_MESSAGE_BYTES`] на сообщение.
//! Служебная диагностика идёт в stderr и не является частью протокола.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::manifest::SecretPresence;

/// Версия API хоста, которую реализует первая поставка.
pub const HOST_API_VERSION: &str = "1.0.0";
/// Предел одного сообщения в любую сторону.
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
/// Модуль обязан подтвердить готовность за это время.
pub const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(3);
/// Сколько ждать корректного выхода после `shutdown`.
pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
/// Версия JSON-RPC, зафиксированная контрактом.
pub const JSONRPC_VERSION: &str = "2.0";

/// Машинные коды ошибок JSON-RPC и контракта модуля.
pub mod error_codes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;

    pub const ACTION_NOT_FOUND: i64 = 1001;
    pub const CANCELLED: i64 = 1002;
    pub const CAPABILITY_DENIED: i64 = 1003;
    pub const HOST_API_INCOMPATIBLE: i64 = 1004;
    pub const CONTEXT_LOST: i64 = 1005;
    pub const FIELD_INVALID: i64 = 1006;
    pub const BUFFER_EXPIRED: i64 = 1007;
}

/// Запрос от хоста к модулю или от модуля к хосту.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl RpcRequest {
    pub fn new(id: Value, method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id: Some(id),
            method: method.into(),
            params,
        }
    }
}

/// Ответ на запрос. Ровно одно из полей `result` и `error` заполнено.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl RpcResponse {
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Value, error: RpcError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// Ошибка протокола с машинным кодом и сообщением для журнала.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for RpcError {}

/// Параметры `initialize`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub host_api: String,
    pub module_id: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// Ответ на `initialize`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub api_version: String,
    pub module_id: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// Откуда пользователь вызвал действие.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InvocationSource {
    #[default]
    Tray,
    Hotkey,
}

impl InvocationSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tray => "tray",
            Self::Hotkey => "hotkey",
        }
    }
}

/// Исходный контекст вызова, который ядро фиксирует до открытия своих окон.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallContext {
    #[serde(default)]
    pub source: InvocationSource,
}

/// Параметры `invoke`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvokeRequest {
    pub call_id: String,
    pub action_id: String,
    #[serde(default)]
    pub context: CallContext,
    /// Необязательные входные данные действия, например текст ручного ввода.
    #[serde(default)]
    pub params: serde_json::Value,
}

/// Конечный исход вызова.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InvokeStatus {
    Ok,
    Cancelled,
}

/// Запрос команды формы настроек. Хост передаёт значения только тех полей,
/// которые перечислены в манифесте; секреты заменяются ссылками хранилища.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FormCommandRequest {
    pub command_id: String,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub values: BTreeMap<String, Value>,
    /// Поле секрета и признак наличия сохранённого значения. Сам секрет
    /// передаётся только через возможность хоста `secret.read`.
    #[serde(default)]
    pub secrets: BTreeMap<String, SecretPresence>,
}

/// Результат команды формы. Ошибки привязываются к конкретным полям, чтобы
/// оболочка показала их рядом с вводом.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FormCommandResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default)]
    pub field_errors: Vec<FormFieldError>,
}

/// Ошибка одного поля формы.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FormFieldError {
    pub field_id: String,
    pub code: String,
    pub message: String,
}

/// Результат успешного или отменённого вызова.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvokeResult {
    pub status: InvokeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl InvokeResult {
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            status: InvokeStatus::Ok,
            message: Some(message.into()),
            data: None,
        }
    }

    pub fn cancelled() -> Self {
        Self {
            status: InvokeStatus::Cancelled,
            message: None,
            data: None,
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    /// Текст для интерфейса; у отмены собственного сообщения нет.
    pub fn message_or_default(&self) -> &str {
        self.message.as_deref().unwrap_or("Действие отменено")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_round_trip() {
        let request = RpcRequest::new(json!(7), "invoke", json!({"callId": "call-1"}));
        let line = serde_json::to_string(&request).unwrap();
        let parsed: RpcRequest = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed.method, "invoke");
        assert_eq!(parsed.id, Some(json!(7)));
    }

    #[test]
    fn response_with_error_omits_result() {
        let response = RpcResponse::error(
            json!(1),
            RpcError::new(error_codes::ACTION_NOT_FOUND, "нет"),
        );
        let value = serde_json::to_value(&response).unwrap();
        assert!(value.get("result").is_none());
        assert_eq!(value["error"]["code"], error_codes::ACTION_NOT_FOUND);
    }

    #[test]
    fn invoke_result_default_message() {
        assert_eq!(
            InvokeResult::cancelled().message_or_default(),
            "Действие отменено"
        );
        assert_eq!(InvokeResult::ok("готово").message_or_default(), "готово");
    }
    #[test]
    fn form_command_round_trip() {
        let request = FormCommandRequest {
            command_id: "testConnection".to_owned(),
            profile: Some("yandex".to_owned()),
            values: BTreeMap::from([("sourceLang".to_owned(), json!("en"))]),
            secrets: BTreeMap::from([(
                "apiKey".to_owned(),
                SecretPresence {
                    secret_ref: "translator/yandex/apiKey".to_owned(),
                    present: true,
                },
            )]),
        };
        let line = serde_json::to_string(&request).unwrap();
        let parsed: FormCommandRequest = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed.command_id, "testConnection");
        assert_eq!(parsed.profile.as_deref(), Some("yandex"));

        let result = FormCommandResult {
            message: None,
            data: None,
            field_errors: vec![FormFieldError {
                field_id: "apiKey".to_owned(),
                code: "required".to_owned(),
                message: "Укажите ключ".to_owned(),
            }],
        };
        let value = serde_json::to_value(result).unwrap();
        assert_eq!(value["fieldErrors"][0]["fieldId"], "apiKey");
    }
}
