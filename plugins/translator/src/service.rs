//! Связка выделения, профилей и HTTP-адаптеров для действия `translate`.

use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};
use util_maker_providers::{
    check_connection, HttpTransport, Profile, ProviderError, ProviderHub, ProviderKind, Translation,
};
use util_maker_sdk::{host_methods, FrameRegion, HostBridge, HostCallError};

/// Возможность чтения выделения исходного окна.
pub const SELECTION_CAPABILITY: &str = "selection.read";
/// Возможность чтения обычных настроек модуля.
pub const SETTINGS_CAPABILITY: &str = "settings.read";
/// Возможность чтения ключа профиля.
pub const SECRET_CAPABILITY: &str = "secret.read";
/// Возможность явной записи результата в буфер обмена.
pub const CLIPBOARD_CAPABILITY: &str = "clipboard.write";

/// Сколько ждать жест выделения.
pub const SELECTION_TIMEOUT: Duration = Duration::from_secs(25);
/// Один запрос ожидания не должен блокировать отмену дольше этого срока.
const WAIT_SLICE: Duration = Duration::from_secs(2);

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

/// Готовый текст либо причина, по которой автоматический захват невозможен.
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

/// Итог одного вызова переводчика.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslationOutcome {
    Translated {
        text: String,
        source_text: String,
        provider: ProviderKind,
        bounds: FrameRegion,
    },
    NeedsProfile {
        profile: ProviderKind,
        source_text: String,
        bounds: FrameRegion,
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
        error: ProviderError,
        provider: ProviderKind,
        source_text: String,
        bounds: FrameRegion,
    },
}

impl TranslationOutcome {
    pub fn message(&self) -> String {
        match self {
            Self::Translated { text, .. } => text.clone(),
            Self::NeedsProfile { profile, .. } => format!(
                "Ключ профиля «{}» не настроен. Откройте настройки и сохраните API-ключ.",
                profile.name()
            ),
            Self::ManualInput { .. } => {
                "Автоматическое выделение недоступно. Вставьте текст и нажмите «Перевести»."
                    .to_owned()
            }
            Self::Blocked { message, .. } => (*message).to_owned(),
            Self::ProviderError { error, .. } => error.to_string(),
        }
    }

    pub fn data(&self) -> Value {
        match self {
            Self::Translated {
                text,
                source_text,
                provider,
                bounds,
            } => json!({
                "outcome": "translated",
                "text": text,
                "sourceText": source_text,
                "provider": provider.id(),
                "providerName": provider.name(),
                "bounds": bounds,
                "httpRequests": 1,
            }),
            Self::NeedsProfile {
                profile,
                source_text,
                bounds,
            } => json!({
                "outcome": "needs-profile",
                "profile": profile.id(),
                "reason": "missing-key",
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
                error,
                provider,
                source_text,
                bounds,
            } => json!({
                "outcome": "error",
                "reason": error.code(),
                "message": error.to_string(),
                "provider": provider.id(),
                "providerName": provider.name(),
                "sourceText": source_text,
                "bounds": bounds,
                "httpRequests": 1,
                "retryable": matches!(
                    error,
                    ProviderError::QuotaExceeded
                        | ProviderError::RateLimited
                        | ProviderError::Network
                        | ProviderError::Timeout
                        | ProviderError::HttpStatus(_)
                ),
            }),
        }
    }
}

/// Выполняет действие перевода: получает текст, строит профиль и делает один
/// запрос выбранным адаптером. Смена профиля действует со следующего вызова.
pub fn translate_from_source(
    host: &mut dyn HostBridge,
    transport: &dyn HttpTransport,
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

    let hub = load_hub(host)?;
    let active = hub.active().unwrap_or(ProviderKind::Yandex);
    let profile = hub.profile(active);
    if profile.is_none_or(|profile| profile.api_key.trim().is_empty()) {
        return Ok(TranslationOutcome::NeedsProfile {
            profile: active,
            source_text: text,
            bounds,
        });
    }

    let cancel = std::sync::atomic::AtomicBool::new(false);
    match hub.translate(&text, transport, &cancel) {
        Ok(Translation {
            text: translated, ..
        }) => Ok(TranslationOutcome::Translated {
            text: translated,
            source_text: text,
            provider: active,
            bounds,
        }),
        Err(error) => Ok(TranslationOutcome::ProviderError {
            error,
            provider: active,
            source_text: text,
            bounds,
        }),
    }
}

/// Проверяет соединение фиксированной фразой. Ключ берётся у активного
/// профиля, поэтому проверка не переключает сервис.
pub fn check_active_profile(
    host: &mut dyn HostBridge,
    transport: &dyn HttpTransport,
) -> Result<Result<(), ProviderError>, HostCallError> {
    let hub = load_hub(host)?;
    let Some(active) = hub.active() else {
        return Ok(Err(ProviderError::MissingProfile("активный")));
    };
    let Some(profile) = hub.profile(active) else {
        return Ok(Err(ProviderError::MissingProfile(active.name())));
    };
    if profile.api_key.trim().is_empty() {
        return Ok(Err(ProviderError::MissingProfile(active.name())));
    }
    let cancel = std::sync::atomic::AtomicBool::new(false);
    Ok(check_connection(profile, transport, &cancel))
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

fn load_hub(host: &mut dyn HostBridge) -> Result<ProviderHub, HostCallError> {
    let root = host.request(SETTINGS_CAPABILITY, host_methods::SETTINGS_READ, json!({}))?;
    let active = root
        .get("activeProfile")
        .and_then(Value::as_str)
        .and_then(ProviderKind::parse)
        .unwrap_or(ProviderKind::Yandex);
    let mut hub = ProviderHub::new();
    for kind in [ProviderKind::Yandex, ProviderKind::Google] {
        let values = host.request(
            SETTINGS_CAPABILITY,
            host_methods::SETTINGS_READ,
            json!({"profile": kind.id()}),
        )?;
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
        let key = match host.request(
            SECRET_CAPABILITY,
            host_methods::SECRET_READ,
            json!({"key": kind.api_key_secret_ref()}),
        ) {
            Ok(Value::String(value)) => value,
            Ok(_) => String::new(),
            Err(HostCallError::Denied(_)) => String::new(),
            Err(error) => return Err(error),
        };
        let mut profile = match kind {
            ProviderKind::Yandex => Profile::yandex(key),
            ProviderKind::Google => Profile::google(key),
        };
        profile.source = source;
        profile.target = target;
        hub.add_profile(profile);
    }
    hub.set_active(active);
    Ok(hub)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use util_maker_providers::{HttpRequest, HttpResponse, TransportError};

    struct FakeHost {
        snapshots: VecDeque<Result<Value, HostCallError>>,
        waits: VecDeque<Result<Value, HostCallError>>,
        settings: Value,
        secret: Value,
        calls: Mutex<Vec<(String, String, Value)>>,
        cancelled: AtomicBool,
    }

    impl FakeHost {
        fn new(snapshot: Value, settings: Value, secret: Value) -> Self {
            Self {
                snapshots: VecDeque::from([Ok(snapshot)]),
                waits: VecDeque::new(),
                settings,
                secret,
                calls: Mutex::new(Vec::new()),
                cancelled: AtomicBool::new(false),
            }
        }

        fn with_wait(mut self, wait: Value) -> Self {
            self.waits.push_back(Ok(wait));
            self
        }
    }

    impl HostBridge for FakeHost {
        fn request(
            &mut self,
            capability: &str,
            method: &str,
            params: Value,
        ) -> Result<Value, HostCallError> {
            self.calls.lock().unwrap().push((
                capability.to_owned(),
                method.to_owned(),
                params.clone(),
            ));
            match method {
                host_methods::SELECTION_GET => self
                    .snapshots
                    .pop_front()
                    .unwrap_or_else(|| Err(HostCallError::Denied("нет снимка".to_owned()))),
                host_methods::SELECTION_WAIT => self
                    .waits
                    .pop_front()
                    .unwrap_or_else(|| Err(HostCallError::Denied("нет ожидания".to_owned()))),
                host_methods::SETTINGS_READ => Ok(self.settings.clone()),
                host_methods::SECRET_READ => Ok(self.secret.clone()),
                _ => Err(HostCallError::Denied(method.to_owned())),
            }
        }

        fn is_cancelled(&self) -> bool {
            self.cancelled.load(Ordering::SeqCst)
        }
    }

    #[derive(Default)]
    struct RecordingTransport {
        requests: Mutex<Vec<HttpRequest>>,
        response: Mutex<Option<Result<HttpResponse, TransportError>>>,
    }

    impl RecordingTransport {
        fn success(body: Value) -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                response: Mutex::new(Some(Ok(HttpResponse {
                    status: 200,
                    body: serde_json::to_vec(&body).unwrap(),
                }))),
            }
        }

        fn error(error: TransportError) -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                response: Mutex::new(Some(Err(error))),
            }
        }
    }

    impl HttpTransport for RecordingTransport {
        fn execute(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
            self.requests.lock().unwrap().push(request.clone());
            self.response
                .lock()
                .unwrap()
                .take()
                .unwrap_or(Err(TransportError::Network("нет ответа".to_owned())))
        }
    }

    fn settings(active: &str) -> Value {
        json!({
            "activeProfile": active,
            "sourceLang": "auto",
            "targetLang": "ru"
        })
    }

    fn selected(text: &str) -> Value {
        json!({
            "source": {"sourceRef": "selection-1", "window": {"x": 10, "y": 20, "width": 300, "height": 200}, "processId": 42},
            "state": {"kind": "text", "text": text, "bounds": {"x": 40, "y": 50, "width": 100, "height": 20}}
        })
    }

    fn yandex_body() -> Value {
        json!({"translations": [{"text": "привет"}]})
    }

    #[test]
    fn selected_text_is_sent_once_and_result_keeps_bounds() {
        let mut host = FakeHost::new(
            selected("hello"),
            settings("yandex"),
            Value::String("key".to_owned()),
        );
        let transport = RecordingTransport::success(yandex_body());
        let outcome = translate_from_source(&mut host, &transport, None).expect("перевод");
        assert!(matches!(
            outcome,
            TranslationOutcome::Translated {
                ref text,
                provider: ProviderKind::Yandex,
                bounds: FrameRegion { x: 40, y: 50, .. },
                ..
            } if text == "привет"
        ));
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["texts"], json!(["hello"]));
        assert_eq!(outcome.data()["httpRequests"], json!(1));
    }

    #[test]
    fn password_blocks_before_settings_and_network() {
        let mut host = FakeHost::new(
            json!({
                "source": {"sourceRef": "selection-1", "window": {"x": 0, "y": 0, "width": 10, "height": 10}, "processId": 1},
                "state": {"kind": "password", "bounds": {"x": 1, "y": 2, "width": 3, "height": 4}}
            }),
            settings("yandex"),
            Value::Null,
        );
        let transport = RecordingTransport::success(yandex_body());
        let outcome = translate_from_source(&mut host, &transport, None).expect("исход");
        assert!(matches!(
            outcome,
            TranslationOutcome::Blocked {
                reason: "password-field",
                ..
            }
        ));
        assert!(transport.requests.lock().unwrap().is_empty());
        assert!(host.calls.lock().unwrap().iter().all(|(_, method, _)| {
            method != host_methods::SETTINGS_READ && method != host_methods::SECRET_READ
        }));
        assert_eq!(outcome.data()["httpRequests"], json!(0));
    }

    #[test]
    fn empty_selection_waits_for_one_gesture_without_second_action() {
        let mut host = FakeHost::new(
            json!({
                "source": {"sourceRef": "selection-1", "window": {"x": 0, "y": 0, "width": 10, "height": 10}, "processId": 1},
                "state": {"kind": "empty", "caret": {"x": 4, "y": 5}}
            }),
            settings("yandex"),
            Value::String("key".to_owned()),
        )
        .with_wait(json!({
            "kind": "selected",
            "text": "выбранный текст",
            "bounds": {"x": 5, "y": 6, "width": 80, "height": 18}
        }));
        let transport = RecordingTransport::success(yandex_body());
        let outcome = translate_from_source(&mut host, &transport, None).expect("перевод");
        assert!(matches!(
            outcome,
            TranslationOutcome::Translated { ref source_text, .. } if source_text == "выбранный текст"
        ));
        let methods: Vec<_> = host
            .calls
            .lock()
            .unwrap()
            .iter()
            .map(|(_, method, _)| method.clone())
            .collect();
        assert_eq!(
            methods,
            vec![
                host_methods::SELECTION_GET.to_owned(),
                host_methods::SELECTION_WAIT.to_owned(),
                host_methods::SETTINGS_READ.to_owned(),
                host_methods::SETTINGS_READ.to_owned(),
                host_methods::SECRET_READ.to_owned(),
                host_methods::SETTINGS_READ.to_owned(),
                host_methods::SECRET_READ.to_owned(),
            ]
        );
    }

    #[test]
    fn manual_text_skips_selection_and_missing_key_skips_http() {
        let mut host = FakeHost::new(json!({}), settings("google"), Value::Null);
        let transport = RecordingTransport::success(yandex_body());
        let outcome =
            translate_from_source(&mut host, &transport, Some("ручной текст")).expect("итог");
        assert!(matches!(
            outcome,
            TranslationOutcome::NeedsProfile {
                profile: ProviderKind::Google,
                ..
            }
        ));
        assert!(transport.requests.lock().unwrap().is_empty());
        assert!(host.calls.lock().unwrap().iter().all(|(_, method, _)| {
            method != host_methods::SELECTION_GET && method != host_methods::SELECTION_WAIT
        }));
    }

    #[test]
    fn active_profile_uses_only_its_key_and_reports_error() {
        let mut host = FakeHost::new(
            selected("hello"),
            settings("google"),
            Value::String("google-key".to_owned()),
        );
        let transport = RecordingTransport::error(TransportError::Timeout);
        let outcome = translate_from_source(&mut host, &transport, None).expect("ошибка");
        assert!(matches!(
            outcome,
            TranslationOutcome::ProviderError {
                error: ProviderError::Timeout,
                provider: ProviderKind::Google,
                ..
            }
        ));
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].url.contains("google-key"));
        assert!(!requests[0].url.contains("yandex-key"));
        assert_eq!(outcome.data()["reason"], json!("timeout"));
        assert_eq!(outcome.data()["retryable"], json!(true));
    }

    #[test]
    fn probe_uses_fixed_phrase() {
        let mut host = FakeHost::new(
            Value::Null,
            settings("yandex"),
            Value::String("key".to_owned()),
        );
        let transport = RecordingTransport::success(yandex_body());
        let result = check_active_profile(&mut host, &transport).expect("проверка");
        assert!(result.is_ok());
        let requests = transport.requests.lock().unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["texts"], json!([util_maker_providers::PROBE_TEXT]));
    }
}
