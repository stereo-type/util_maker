//! Плагин-расширение переводчика: Яндекс.
//!
//! Собственных действий нет. Пакет реализует операцию `translate` контракта
//! `util-maker.translation.provider` и команду формы `testConnection`.

use std::sync::atomic::AtomicBool;

use serde_json::{json, Value};
use util_maker_providers::{
    check_connection, translate, HttpTransport, Profile, ProviderError, ReqwestTransport,
};
use util_maker_sdk::{
    error_codes, host_methods, run_stdio, ExtensionInvokeRequest, ExtensionInvokeResult,
    FormCommandRequest, FormCommandResult, FormFieldError, HostBridge, HostCallError,
    InvokeRequest, InvokeResult, Module, ModuleInfo, RpcError, HOST_API_VERSION,
};

const MODULE_ID: &str = "org.utilmaker.translator.yandex";
const PROVIDER_NAME: &str = "Яндекс";
const SECRET_REF: &str = "translator/yandex/apiKey";
const SECRET_CAPABILITY: &str = "secret.read";

struct ProviderModule {
    info: ModuleInfo,
    transport: Option<Box<dyn HttpTransport + Send + Sync>>,
}

impl ProviderModule {
    fn new() -> Self {
        Self {
            info: ModuleInfo {
                id: MODULE_ID,
                version: env!("CARGO_PKG_VERSION"),
                host_api: HOST_API_VERSION,
                capabilities: &["settings.read", SECRET_CAPABILITY],
            },
            transport: ReqwestTransport::new()
                .ok()
                .map(|transport| Box::new(transport) as Box<dyn HttpTransport + Send + Sync>),
        }
    }

    fn read_key(host: &mut dyn HostBridge) -> Result<String, RpcError> {
        match host.request(
            SECRET_CAPABILITY,
            host_methods::SECRET_READ,
            json!({"key": SECRET_REF}),
        ) {
            Ok(Value::String(value)) if !value.trim().is_empty() => Ok(value),
            Ok(_) => Err(missing_key()),
            Err(HostCallError::Denied(_)) => Err(missing_key()),
            Err(error) => Err(error.into_rpc_error()),
        }
    }
}

fn missing_key() -> RpcError {
    RpcError::new(
        error_codes::EXTENSION_FAILED,
        format!("Ключ {PROVIDER_NAME} не настроен"),
    )
    .with_data(json!({
        "code": "missing-profile",
        "message": format!("Введите API-ключ {PROVIDER_NAME} в настройках плагина"),
        "retryable": false,
    }))
}

fn provider_error(error: ProviderError) -> RpcError {
    RpcError::new(error_codes::EXTENSION_FAILED, error.to_string()).with_data(json!({
        "code": error.code(),
        "message": error.to_string(),
        "retryable": matches!(
            error,
            ProviderError::QuotaExceeded
                | ProviderError::RateLimited
                | ProviderError::Network
                | ProviderError::Timeout
                | ProviderError::HttpStatus(_)
        ),
    }))
}

impl Module for ProviderModule {
    fn info(&self) -> &ModuleInfo {
        &self.info
    }

    fn invoke(&mut self, _request: &InvokeRequest) -> Result<InvokeResult, RpcError> {
        Err(RpcError::new(
            error_codes::ACTION_NOT_FOUND,
            "у плагина-провайдера нет собственных действий",
        ))
    }

    fn extension_invoke_with_host(
        &mut self,
        request: &ExtensionInvokeRequest,
        host: &mut dyn HostBridge,
    ) -> Result<ExtensionInvokeResult, RpcError> {
        if request.operation != "translate" {
            return Err(RpcError::new(
                error_codes::METHOD_NOT_FOUND,
                format!("неизвестная операция: {}", request.operation),
            ));
        }
        let transport = self.transport.as_deref().ok_or_else(|| {
            RpcError::new(
                error_codes::INTERNAL_ERROR,
                "HTTP-транспорт провайдера недоступен",
            )
        })?;
        let text = request
            .params
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::new(error_codes::INVALID_PARAMS, "не передан текст"))?;
        let source = request
            .params
            .get("sourceLanguage")
            .and_then(Value::as_str)
            .filter(|value| *value != "auto");
        let target = request
            .params
            .get("targetLanguage")
            .and_then(Value::as_str)
            .unwrap_or("ru");
        let key = Self::read_key(host)?;
        let mut profile = Profile::yandex(key);
        if let Some(source) = source {
            profile.source = Some(source.to_owned());
        }
        profile.target = target.to_owned();
        let cancel = AtomicBool::new(false);
        let translation = translate(&profile, text, transport, &cancel).map_err(provider_error)?;
        Ok(ExtensionInvokeResult {
            data: json!({"translation": translation.text}),
        })
    }

    fn command_with_host(
        &mut self,
        request: &FormCommandRequest,
        host: &mut dyn HostBridge,
    ) -> Result<FormCommandResult, RpcError> {
        if request.command_id != "testConnection" {
            return Err(RpcError::new(
                error_codes::METHOD_NOT_FOUND,
                format!("неизвестная команда формы: {}", request.command_id),
            ));
        }
        let transport = self.transport.as_deref().ok_or_else(|| {
            RpcError::new(
                error_codes::INTERNAL_ERROR,
                "HTTP-транспорт провайдера недоступен",
            )
        })?;
        let key = match Self::read_key(host) {
            Ok(key) => key,
            Err(_) => {
                return Ok(FormCommandResult {
                    message: Some("Ключ не настроен".to_owned()),
                    data: Some(json!({"connected": false, "reason": "missing-profile"})),
                    field_errors: vec![FormFieldError {
                        field_id: "apiKey".to_owned(),
                        code: "missing-profile".to_owned(),
                        message: "Введите API-ключ и сохраните настройки".to_owned(),
                    }],
                })
            }
        };
        let profile = Profile::yandex(key);
        let cancel = AtomicBool::new(false);
        match check_connection(&profile, transport, &cancel) {
            Ok(()) => Ok(FormCommandResult {
                message: Some("Подключение установлено.".to_owned()),
                data: Some(json!({"connected": true})),
                field_errors: Vec::new(),
            }),
            Err(error) => Ok(FormCommandResult {
                message: Some(error.to_string()),
                data: Some(json!({"connected": false, "reason": error.code()})),
                field_errors: vec![FormFieldError {
                    field_id: "apiKey".to_owned(),
                    code: error.code().to_owned(),
                    message: error.to_string(),
                }],
            }),
        }
    }
}

fn main() {
    eprintln!("[{MODULE_ID}] старт: {PROVIDER_NAME}, API {HOST_API_VERSION}");
    let mut module = ProviderModule::new();
    if let Err(error) = run_stdio(&mut module) {
        eprintln!("[{MODULE_ID}] ошибка stdio: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use util_maker_providers::{HttpRequest, HttpResponse};

    struct FakeHost {
        secret: Option<String>,
    }

    impl HostBridge for FakeHost {
        fn request(
            &mut self,
            _capability: &str,
            method: &str,
            _params: Value,
        ) -> Result<Value, HostCallError> {
            if method == host_methods::SECRET_READ {
                Ok(self
                    .secret
                    .clone()
                    .map(Value::String)
                    .unwrap_or(Value::Null))
            } else {
                Ok(Value::Null)
            }
        }
    }

    struct FakeTransport {
        request: Mutex<Option<HttpRequest>>,
        body: &'static str,
    }

    impl HttpTransport for FakeTransport {
        fn execute(
            &self,
            request: &HttpRequest,
        ) -> Result<HttpResponse, util_maker_providers::TransportError> {
            *self.request.lock().expect("запрос") = Some(request.clone());
            Ok(HttpResponse {
                status: 200,
                body: self.body.as_bytes().to_vec(),
            })
        }
    }

    fn module() -> ProviderModule {
        ProviderModule {
            info: ModuleInfo {
                id: MODULE_ID,
                version: "0.0.0",
                host_api: HOST_API_VERSION,
                capabilities: &["settings.read", SECRET_CAPABILITY],
            },
            transport: Some(Box::new(FakeTransport {
                request: Mutex::new(None),
                body: "{\"translations\": [{\"text\": \"привет\"}]}",
            })),
        }
    }

    fn request() -> ExtensionInvokeRequest {
        ExtensionInvokeRequest {
            call_id: "call-1".to_owned(),
            point_id: "translation-provider".to_owned(),
            operation: "translate".to_owned(),
            timeout_ms: None,
            params: json!({
                "text": "hello",
                "sourceLanguage": "auto",
                "targetLanguage": "ru",
            }),
        }
    }

    #[test]
    fn missing_key_is_machine_readable() {
        let mut module = module();
        let mut host = FakeHost { secret: None };
        let error = module
            .extension_invoke_with_host(&request(), &mut host)
            .expect_err("нет ключа");
        assert_eq!(error.code, error_codes::EXTENSION_FAILED);
        assert_eq!(error.data.as_ref().unwrap()["code"], "missing-profile");
    }

    #[test]
    fn translation_uses_own_key_and_parses_response() {
        let mut module = module();
        let mut host = FakeHost {
            secret: Some("test-key".to_owned()),
        };
        let result = module
            .extension_invoke_with_host(&request(), &mut host)
            .expect("перевод");
        assert_eq!(result.data["translation"], "привет");
    }

    #[test]
    fn manifest_declares_contribution_and_settings() {
        let manifest = util_maker_sdk::Manifest::from_json(include_str!("../manifest.json"))
            .expect("манифест");
        manifest.validate().expect("проверка");
        assert!(manifest.actions.is_empty());
        assert_eq!(manifest.provides.len(), 1);
        let settings = manifest.settings.as_ref().expect("настройки");
        assert_eq!(settings.fields[0].secret_ref.as_deref(), Some(SECRET_REF));
    }
}
