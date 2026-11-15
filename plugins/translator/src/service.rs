//! Связка выделения и активного провайдера-плагина для действия `translate`.
//!
//! Переводчик не делает HTTP-запросов и не знает сервисов: он читает текст,
//! получает активный вклад через `extensions.list` и вызывает
//! `extension.invoke` с операцией контракта `translate`.

use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};
use util_maker_sdk::{
    error_codes, host_methods, FrameRegion, HostBridge, HostCallError, EXTENSIONS_CALL,
    EXTENSIONS_READ,
};

/// Точка расширения родителя и её контракт.
pub const POINT_ID: &str = "translation-provider";
pub const CONTRACT: &str = "util-maker.translation.provider";
pub const CONTRACT_VERSION: &str = "1.0.0";
/// Операция контракта, которую выполняет активный провайдер.
pub const OPERATION: &str = "translate";

/// Возможность чтения выделения исходного окна.
pub const SELECTION_CAPABILITY: &str = "selection.read";
/// Возможность чтения обычных настроек модуля.
pub const SETTINGS_CAPABILITY: &str = "settings.read";
/// Возможность явной записи результата в буфер обмена.
pub const CLIPBOARD_CAPABILITY: &str = "clipboard.write";

/// Сколько ждать жест выделения.
pub const SELECTION_TIMEOUT: Duration = Duration::from_secs(25);
/// Один запрос ожидания не должен блокировать отмену дольше этого срока.
const WAIT_SLICE: Duration = Duration::from_secs(2);
/// Предел одного вызова провайдера.
const PROVIDER_TIMEOUT_MS: u32 = 10_000;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SelectionSnapshot {
    state: SelectionState,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum SelectionState {
    Text {
        text: String,
        bounds: FrameRegion,
    },
    Empty {
        #[allow(dead_code)]
        caret: ScreenPoint,
    },
    Unsupported {
        bounds: FrameRegion,
    },
    Password {
        bounds: FrameRegion,
    },
    SourceLost,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScreenPoint {
    #[allow(dead_code)]
    x: i32,
    #[allow(dead_code)]
    y: i32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum WaitOutcome {
    Selected { text: String, bounds: FrameRegion },
    Unsupported { bounds: FrameRegion },
    Password { bounds: FrameRegion },
    Cancelled,
    SourceChanged,
    SourceLost,
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SourceText {
    Text {
        text: String,
        bounds: FrameRegion,
    },
    Manual {
        reason: &'static str,
        bounds: Option<FrameRegion>,
    },
    Blocked {
        reason: &'static str,
        message: &'static str,
        bounds: Option<FrameRegion>,
    },
}

/// Активный плагин-провайдер.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRef {
    pub id: String,
    pub name: String,
}

/// Ошибка, которую вернул плагин. Код приходит из `data` его ответа.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderFailure {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

/// Итог одного вызова переводчика.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslationOutcome {
    Translated {
        text: String,
        source_text: String,
        provider: ProviderRef,
        detected_language: Option<String>,
        bounds: FrameRegion,
    },
    NeedsProvider {
        source_text: String,
        bounds: FrameRegion,
        reason: &'static str,
    },
    ManualInput {
        reason: &'static str,
        bounds: Option<FrameRegion>,
    },
    Blocked {
        reason: &'static str,
        message: &'static str,
        bounds: Option<FrameRegion>,
    },
    ProviderError {
        failure: ProviderFailure,
        provider: ProviderRef,
        source_text: String,
        bounds: FrameRegion,
    },
}

impl TranslationOutcome {
    pub fn message(&self) -> String {
        match self {
            Self::Translated { text, .. } => text.clone(),
            Self::NeedsProvider { .. } => {
                "Провайдер перевода не выбран. Откройте настройки переводчика и выберите плагин."
                    .to_owned()
            }
            Self::ManualInput { .. } => {
                "Автоматическое выделение недоступно. Вставьте текст и нажмите «Перевести»."
                    .to_owned()
            }
            Self::Blocked { message, .. } => (*message).to_owned(),
            Self::ProviderError { failure, .. } => failure.message.clone(),
        }
    }

    pub fn data(&self) -> Value {
        match self {
            Self::Translated {
                text,
                source_text,
                provider,
                detected_language,
                bounds,
            } => json!({
                "outcome": "translated",
                "text": text,
                "sourceText": source_text,
                "provider": provider.id,
                "providerName": provider.name,
                "detectedLanguage": detected_language,
                "bounds": bounds,
                "httpRequests": 1,
            }),
            Self::NeedsProvider {
                source_text,
                bounds,
                reason,
            } => json!({
                "outcome": "needs-profile",
                "reason": reason,
                "sourceText": source_text,
                "bounds": bounds,
                "httpRequests": 0,
            }),
            Self::ManualInput { reason, bounds } => json!({
                "outcome": "manual-input",
                "reason": reason,
                "bounds": bounds,
                "httpRequests": 0,
            }),
            Self::Blocked {
                reason,
                message,
                bounds,
            } => json!({
                "outcome": "blocked",
                "reason": reason,
                "message": message,
                "bounds": bounds,
                "httpRequests": 0,
            }),
            Self::ProviderError {
                failure,
                provider,
                source_text,
                bounds,
            } => json!({
                "outcome": "error",
                "reason": failure.code,
                "message": failure.message,
                "provider": provider.id,
                "providerName": provider.name,
                "sourceText": source_text,
                "bounds": bounds,
                "httpRequests": 1,
                "retryable": failure.retryable,
            }),
        }
    }
}

/// Выполняет действие перевода: получает текст, активный вклад и вызывает
/// операцию контракта. Ошибка одного плагина не переключает запрос другому.
pub fn translate_from_source(
    host: &mut dyn HostBridge,
    manual_text: Option<&str>,
) -> Result<TranslationOutcome, HostCallError> {
    let source = match manual_text {
        Some(text) if !text.trim().is_empty() => SourceText::Text {
            text: text.to_owned(),
            bounds: FrameRegion {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            },
        },
        _ => capture_source(host)?,
    };

    let (text, bounds) = match source {
        SourceText::Text { text, bounds } => (text, bounds),
        SourceText::Manual { reason, bounds } => {
            return Ok(TranslationOutcome::ManualInput { reason, bounds });
        }
        SourceText::Blocked {
            reason,
            message,
            bounds,
        } => {
            return Ok(TranslationOutcome::Blocked {
                reason,
                message,
                bounds,
            })
        }
    };

    let Some(provider) = active_provider(host)? else {
        return Ok(TranslationOutcome::NeedsProvider {
            source_text: text,
            bounds,
            reason: "not-selected",
        });
    };
    let (source_language, target_language) = load_languages(host)?;
    match invoke_translate(host, &text, source_language.as_deref(), &target_language) {
        Ok(value) => {
            let Some(translated) = value.get("translation").and_then(Value::as_str) else {
                return Ok(TranslationOutcome::ProviderError {
                    failure: ProviderFailure {
                        code: "invalid-response".to_owned(),
                        message: "Плагин не вернул перевод".to_owned(),
                        retryable: false,
                    },
                    provider,
                    source_text: text,
                    bounds,
                });
            };
            let detected = value
                .get("detectedLanguage")
                .and_then(Value::as_str)
                .map(str::to_owned);
            Ok(TranslationOutcome::Translated {
                text: translated.to_owned(),
                source_text: text,
                provider,
                detected_language: detected,
                bounds,
            })
        }
        Err(HostCallError::Cancelled) => Err(HostCallError::Cancelled),
        Err(HostCallError::Remote {
            code,
            message,
            data,
        }) => Ok(TranslationOutcome::ProviderError {
            failure: provider_failure(code, &message, data.as_deref()),
            provider,
            source_text: text,
            bounds,
        }),
        Err(error) => Ok(TranslationOutcome::ProviderError {
            failure: ProviderFailure {
                code: "invalid-response".to_owned(),
                message: error.to_string(),
                retryable: false,
            },
            provider,
            source_text: text,
            bounds,
        }),
    }
}

fn active_provider(host: &mut dyn HostBridge) -> Result<Option<ProviderRef>, HostCallError> {
    let value = match host.request(
        EXTENSIONS_READ,
        host_methods::EXTENSIONS_LIST,
        json!({ "pointId": POINT_ID }),
    ) {
        Ok(value) => value,
        Err(HostCallError::Remote { code, .. }) if code == error_codes::EXTENSION_NOT_SELECTED => {
            return Ok(None)
        }
        Err(error) => return Err(error),
    };
    let Some(selected) = value
        .get("selected")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let name = value
        .get("contributions")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("moduleId").and_then(Value::as_str) == Some(selected))
        })
        .and_then(|item| item.get("name"))
        .and_then(Value::as_str)
        .unwrap_or(selected)
        .to_owned();
    Ok(Some(ProviderRef {
        id: selected.to_owned(),
        name,
    }))
}

fn load_languages(host: &mut dyn HostBridge) -> Result<(Option<String>, String), HostCallError> {
    let values = host.request(SETTINGS_CAPABILITY, host_methods::SETTINGS_READ, json!({}))?;
    let source = values
        .get("sourceLang")
        .and_then(Value::as_str)
        .filter(|value| *value != "auto")
        .map(str::to_owned);
    let target = values
        .get("targetLang")
        .and_then(Value::as_str)
        .unwrap_or("ru")
        .to_owned();
    Ok((source, target))
}

fn invoke_translate(
    host: &mut dyn HostBridge,
    text: &str,
    source_language: Option<&str>,
    target_language: &str,
) -> Result<Value, HostCallError> {
    let value = host.request(
        EXTENSIONS_CALL,
        host_methods::EXTENSION_INVOKE,
        json!({
            "callId": "",
            "pointId": POINT_ID,
            "operation": OPERATION,
            "timeoutMs": PROVIDER_TIMEOUT_MS,
            "params": {
                "text": text,
                "sourceLanguage": source_language.unwrap_or("auto"),
                "targetLanguage": target_language,
            },
        }),
    )?;
    Ok(value.get("data").cloned().unwrap_or(value))
}

fn provider_failure(code: i64, message: &str, data: Option<&str>) -> ProviderFailure {
    let parsed = data.and_then(|value| serde_json::from_str::<Value>(value).ok());
    let provider_code = parsed
        .as_ref()
        .and_then(|value| value.get("code"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            if code == error_codes::EXTENSION_FAILED {
                "extension-failed".to_owned()
            } else {
                "invalid-response".to_owned()
            }
        });
    let provider_message = parsed
        .as_ref()
        .and_then(|value| value.get("message"))
        .and_then(Value::as_str)
        .unwrap_or(message)
        .to_owned();
    let retryable = parsed
        .as_ref()
        .and_then(|value| value.get("retryable"))
        .and_then(Value::as_bool)
        .unwrap_or(matches!(
            provider_code.as_str(),
            "network" | "timeout" | "rate-limited" | "quota-exceeded" | "http-status"
        ));
    ProviderFailure {
        code: provider_code,
        message: provider_message,
        retryable,
    }
}

fn capture_source(host: &mut dyn HostBridge) -> Result<SourceText, HostCallError> {
    let value = host.request(
        SELECTION_CAPABILITY,
        host_methods::SELECTION_GET,
        Value::Null,
    )?;
    let snapshot: SelectionSnapshot = serde_json::from_value(value)
        .map_err(|error| HostCallError::Protocol(format!("снимок выделения: {error}")))?;
    match snapshot.state {
        SelectionState::Text { text, bounds } => Ok(SourceText::Text { text, bounds }),
        SelectionState::Unsupported { bounds } => Ok(SourceText::Manual {
            reason: "unsupported",
            bounds: Some(bounds),
        }),
        SelectionState::Password { bounds } => Ok(SourceText::Blocked {
            reason: "password-field",
            message: "Поле пароля защищено: чтение выделения и перевод запрещены.",
            bounds: Some(bounds),
        }),
        SelectionState::SourceLost => Ok(SourceText::Blocked {
            reason: "context-lost",
            message: "Исходное окно потеряло контекст: выделение недоступно, запрос не отправлен.",
            bounds: None,
        }),
        SelectionState::Empty { .. } => wait_for_selection(host),
    }
}

fn wait_for_selection(host: &mut dyn HostBridge) -> Result<SourceText, HostCallError> {
    let deadline = Instant::now() + SELECTION_TIMEOUT;
    loop {
        if host.is_cancelled() {
            return Err(HostCallError::Cancelled);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(SourceText::Manual {
                reason: "selection-timeout",
                bounds: None,
            });
        }
        let timeout = remaining.min(WAIT_SLICE);
        let value = host.request(
            SELECTION_CAPABILITY,
            host_methods::SELECTION_WAIT,
            json!({"timeoutMs": timeout.as_millis().min(u128::from(u64::MAX)) as u64}),
        )?;
        let outcome: WaitOutcome = serde_json::from_value(value)
            .map_err(|error| HostCallError::Protocol(format!("ожидание выделения: {error}")))?;
        match outcome {
            WaitOutcome::Selected { text, bounds } => {
                return Ok(SourceText::Text { text, bounds });
            }
            WaitOutcome::Unsupported { bounds } => {
                return Ok(SourceText::Manual {
                    reason: "unsupported",
                    bounds: Some(bounds),
                });
            }
            WaitOutcome::Password { bounds } => {
                return Ok(SourceText::Blocked {
                    reason: "password-field",
                    message: "Поле пароля защищено: чтение выделения и перевод запрещены.",
                    bounds: Some(bounds),
                });
            }
            WaitOutcome::Cancelled => return Err(HostCallError::Cancelled),
            WaitOutcome::SourceChanged | WaitOutcome::SourceLost => {
                return Ok(SourceText::Blocked {
                    reason: "context-lost",
                    message: "Исходное окно изменилось: перевод отменён без запроса.",
                    bounds: None,
                })
            }
            WaitOutcome::TimedOut => continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct FakeHost {
        responses: VecDeque<Result<Value, HostCallError>>,
        calls: Vec<(String, String, Value)>,
    }

    impl FakeHost {
        fn new(responses: Vec<Result<Value, HostCallError>>) -> Self {
            Self {
                responses: VecDeque::from(responses),
                calls: Vec::new(),
            }
        }
    }

    impl HostBridge for FakeHost {
        fn request(
            &mut self,
            capability: &str,
            method: &str,
            params: Value,
        ) -> Result<Value, HostCallError> {
            self.calls
                .push((capability.to_owned(), method.to_owned(), params));
            self.responses.pop_front().unwrap_or(Ok(Value::Null))
        }
    }

    fn bounds() -> Value {
        json!({"x": 10, "y": 20, "width": 100, "height": 20})
    }

    fn snapshot_text(text: &str) -> Value {
        json!({"state": {"kind": "text", "text": text, "bounds": bounds()}})
    }

    fn list(selected: Value) -> Value {
        json!({
            "pointId": POINT_ID,
            "selected": selected,
            "required": true,
            "contributions": [
                {"moduleId": "org.utilmaker.translator.yandex", "name": "Яндекс", "available": true},
                {"moduleId": "org.utilmaker.translator.google", "name": "Google", "available": true}
            ]
        })
    }

    #[test]
    fn translates_through_active_extension_with_languages() {
        let mut host = FakeHost::new(vec![
            Ok(snapshot_text("hello")),
            Ok(list(json!("org.utilmaker.translator.yandex"))),
            Ok(json!({"sourceLang": "auto", "targetLang": "de"})),
            Ok(json!({"data": {"translation": "hallo", "detectedLanguage": "en"}})),
        ]);
        let outcome = translate_from_source(&mut host, None).expect("перевод");
        match &outcome {
            TranslationOutcome::Translated {
                text,
                provider,
                detected_language,
                ..
            } => {
                assert_eq!(text, "hallo");
                assert_eq!(provider.id, "org.utilmaker.translator.yandex");
                assert_eq!(provider.name, "Яндекс");
                assert_eq!(detected_language.as_deref(), Some("en"));
            }
            other => panic!("неожиданный итог: {other:?}"),
        }
        assert_eq!(host.calls.len(), 4);
        let invoke = &host.calls[3];
        assert_eq!(invoke.0, EXTENSIONS_CALL);
        assert_eq!(invoke.1, host_methods::EXTENSION_INVOKE);
        assert_eq!(invoke.2["params"]["text"], "hello");
        assert_eq!(invoke.2["params"]["sourceLanguage"], "auto");
        assert_eq!(invoke.2["params"]["targetLanguage"], "de");
        assert_eq!(outcome.data()["httpRequests"], 1);
    }

    #[test]
    fn missing_selection_reports_needs_provider_without_invoke() {
        let mut host = FakeHost::new(vec![Ok(snapshot_text("hello")), Ok(list(Value::Null))]);
        let outcome = translate_from_source(&mut host, None).expect("итог");
        assert!(matches!(
            outcome,
            TranslationOutcome::NeedsProvider {
                reason: "not-selected",
                ..
            }
        ));
        assert_eq!(host.calls.len(), 2);
        assert_eq!(outcome.data()["httpRequests"], 0);
    }

    #[test]
    fn manual_text_skips_selection() {
        let mut host = FakeHost::new(vec![
            Ok(list(json!("org.utilmaker.translator.yandex"))),
            Ok(json!({"sourceLang": "en", "targetLang": "ru"})),
            Ok(json!({"data": {"translation": "привет"}})),
        ]);
        let outcome = translate_from_source(&mut host, Some("hello")).expect("итог");
        assert!(matches!(outcome, TranslationOutcome::Translated { .. }));
        assert_eq!(host.calls.len(), 3);
        assert_eq!(host.calls[0].1, host_methods::EXTENSIONS_LIST);
        assert_eq!(host.calls[2].2["params"]["sourceLanguage"], "en");
    }

    #[test]
    fn plugin_failure_keeps_machine_code_and_does_not_fallback() {
        let mut host = FakeHost::new(vec![
            Ok(snapshot_text("hello")),
            Ok(list(json!("org.utilmaker.translator.yandex"))),
            Ok(json!({"sourceLang": "auto", "targetLang": "ru"})),
            Err(HostCallError::Remote {
                code: error_codes::EXTENSION_FAILED,
                message: "плагин вернул ошибку".to_owned(),
                data: Some(json!({"code": "unauthorized", "message": "нет ключа"}).to_string()),
            }),
        ]);
        let outcome = translate_from_source(&mut host, None).expect("итог");
        match &outcome {
            TranslationOutcome::ProviderError {
                failure, provider, ..
            } => {
                assert_eq!(failure.code, "unauthorized");
                assert_eq!(failure.message, "нет ключа");
                assert!(!failure.retryable);
                assert_eq!(provider.id, "org.utilmaker.translator.yandex");
            }
            other => panic!("неожиданный итог: {other:?}"),
        }
        assert_eq!(host.calls.len(), 4);
        assert_eq!(outcome.data()["reason"], "unauthorized");
        assert_eq!(outcome.data()["httpRequests"], 1);
    }

    #[test]
    fn cancellation_and_password_blocking_are_kept() {
        let mut password = FakeHost::new(vec![Ok(json!({
            "state": {"kind": "password", "bounds": bounds()}
        }))]);
        let outcome = translate_from_source(&mut password, None).expect("итог");
        assert!(matches!(
            outcome,
            TranslationOutcome::Blocked {
                reason: "password-field",
                ..
            }
        ));
        assert_eq!(password.calls.len(), 1);

        let mut cancelled = FakeHost::new(vec![
            Ok(snapshot_text("hello")),
            Ok(list(json!("org.utilmaker.translator.yandex"))),
            Ok(json!({"sourceLang": "auto", "targetLang": "ru"})),
            Err(HostCallError::Cancelled),
        ]);
        assert_eq!(
            translate_from_source(&mut cancelled, None),
            Err(HostCallError::Cancelled)
        );
    }
}
