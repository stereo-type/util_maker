//! HTTP-адаптеры перевода: Yandex Translate API v2 и Google Cloud Translation
//! Basic v2. Транспорт отделён трейтом, поэтому поля запроса, авторизация,
//! разбор ответа и ошибки проверяются на тестовом HTTP-адаптере.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Короткая фиксированная фраза для проверки подключения.
pub const PROBE_TEXT: &str = "Util Maker";
/// Таймаут одного перевода.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Поддерживаемый сервис.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ProviderKind {
    Yandex,
    Google,
}

impl ProviderKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Yandex => "Яндекс",
            Self::Google => "Google",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Yandex => "yandex",
            Self::Google => "google",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "yandex" => Some(Self::Yandex),
            "google" => Some(Self::Google),
            _ => None,
        }
    }

    pub fn api_key_secret_ref(self) -> &'static str {
        match self {
            Self::Yandex => "translator/yandex/apiKey",
            Self::Google => "translator/google/apiKey",
        }
    }

    pub fn from_secret_ref(value: &str) -> Option<Self> {
        match value {
            "translator/yandex/apiKey" => Some(Self::Yandex),
            "translator/google/apiKey" => Some(Self::Google),
            _ => None,
        }
    }
}

/// Профиль подключения. Ключ относится только к этому сервису.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub kind: ProviderKind,
    pub api_key: String,
    pub source: Option<String>,
    pub target: String,
}

impl Profile {
    pub fn yandex(api_key: impl Into<String>) -> Self {
        Self {
            kind: ProviderKind::Yandex,
            api_key: api_key.into(),
            source: None,
            target: "ru".to_owned(),
        }
    }

    pub fn google(api_key: impl Into<String>) -> Self {
        Self {
            kind: ProviderKind::Google,
            api_key: api_key.into(),
            source: None,
            target: "ru".to_owned(),
        }
    }

    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }
}

/// Перевод и имя сервиса, который его вернул.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Translation {
    pub text: String,
    pub provider: ProviderKind,
}

/// Запрос к HTTP-транспорту.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub timeout: Duration,
    pub cancel: Arc<AtomicBool>,
}

/// Ответ транспорта.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Отказ транспорта.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    #[error("таймаут запроса")]
    Timeout,
    #[error("сеть недоступна: {0}")]
    Network(String),
    #[error("запрос отменён")]
    Cancelled,
}

/// HTTP-транспорт, подменяемый в тестах.
pub trait HttpTransport: Send + Sync {
    fn execute(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError>;
}

/// Ошибка перевода, различимая в интерфейсе.
impl ProviderError {
    /// Стабильный машинный код для ответа процесса и формы.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::QuotaExceeded => "quota-exceeded",
            Self::RateLimited => "rate-limited",
            Self::HttpStatus(_) => "http-status",
            Self::Network => "network",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::InvalidResponse(_) => "invalid-response",
            Self::MissingProfile(_) => "missing-profile",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    #[error("неверный API-ключ или нет доступа")]
    Unauthorized,
    #[error("исчерпана квота сервиса")]
    QuotaExceeded,
    #[error("слишком много запросов, повторите позже")]
    RateLimited,
    #[error("сервис ответил ошибкой HTTP {0}")]
    HttpStatus(u16),
    #[error("нет сети")]
    Network,
    #[error("сервис перевода не ответил за отведённое время")]
    Timeout,
    #[error("запрос отменён")]
    Cancelled,
    #[error("некорректный ответ сервиса: {0}")]
    InvalidResponse(String),
    #[error("профиль {0} не настроен")]
    MissingProfile(&'static str),
}

/// Переводит текст активным профилем через заданный транспорт.
pub fn translate(
    profile: &Profile,
    text: &str,
    transport: &dyn HttpTransport,
    cancel: &AtomicBool,
) -> Result<Translation, ProviderError> {
    if cancel.load(Ordering::SeqCst) {
        return Err(ProviderError::Cancelled);
    }
    let request = build_request(profile, text, cancel)?;
    let response = transport.execute(&request).map_err(map_transport)?;
    if cancel.load(Ordering::SeqCst) {
        return Err(ProviderError::Cancelled);
    }
    check_status(response.status)?;
    let value: Value = serde_json::from_slice(&response.body)
        .map_err(|error| ProviderError::InvalidResponse(error.to_string()))?;
    let translated = match profile.kind {
        ProviderKind::Yandex => value
            .pointer("/translations/0/text")
            .and_then(Value::as_str),
        ProviderKind::Google => value
            .pointer("/data/translations/0/translatedText")
            .and_then(Value::as_str),
    }
    .ok_or_else(|| ProviderError::InvalidResponse("в ответе нет перевода".to_owned()))?;
    Ok(Translation {
        text: translated.to_owned(),
        provider: profile.kind,
    })
}

/// Проверка подключения фиксированной короткой фразой.
pub fn check_connection(
    profile: &Profile,
    transport: &dyn HttpTransport,
    cancel: &AtomicBool,
) -> Result<(), ProviderError> {
    translate(profile, PROBE_TEXT, transport, cancel).map(|_| ())
}

fn build_request(
    profile: &Profile,
    text: &str,
    cancel: &AtomicBool,
) -> Result<HttpRequest, ProviderError> {
    if profile.api_key.trim().is_empty() {
        return Err(ProviderError::MissingProfile(match profile.kind {
            ProviderKind::Yandex => "Яндекс",
            ProviderKind::Google => "Google",
        }));
    }
    let (url, headers, body) = match profile.kind {
        ProviderKind::Yandex => {
            let mut body = json!({
                "texts": [text],
                "targetLanguageCode": profile.target,
            });
            if let Some(source) = &profile.source {
                body["sourceLanguageCode"] = json!(source);
            }
            (
                "https://translate.api.cloud.yandex.net/translate/v2/translate".to_owned(),
                vec![
                    (
                        "Authorization".to_owned(),
                        format!("Api-Key {}", profile.api_key),
                    ),
                    ("Content-Type".to_owned(), "application/json".to_owned()),
                ],
                serde_json::to_vec(&body),
            )
        }
        ProviderKind::Google => {
            let mut body = json!({
                "q": [text],
                "target": profile.target,
                "format": "text",
            });
            if let Some(source) = &profile.source {
                body["source"] = json!(source);
            }
            (
                format!(
                    "https://translation.googleapis.com/language/translate/v2?key={}",
                    url_encode(&profile.api_key)
                ),
                vec![("Content-Type".to_owned(), "application/json".to_owned())],
                serde_json::to_vec(&body),
            )
        }
    };
    Ok(HttpRequest {
        url,
        headers,
        body: body.map_err(|error| ProviderError::InvalidResponse(error.to_string()))?,
        timeout: REQUEST_TIMEOUT,
        cancel: Arc::new(AtomicBool::new(cancel.load(Ordering::SeqCst))),
    })
}

fn check_status(status: u16) -> Result<(), ProviderError> {
    match status {
        200..=299 => Ok(()),
        401 | 403 => Err(ProviderError::Unauthorized),
        429 => Err(ProviderError::RateLimited),
        456 => Err(ProviderError::QuotaExceeded),
        other => Err(ProviderError::HttpStatus(other)),
    }
}

fn map_transport(error: TransportError) -> ProviderError {
    match error {
        TransportError::Timeout => ProviderError::Timeout,
        TransportError::Network(_) => ProviderError::Network,
        TransportError::Cancelled => ProviderError::Cancelled,
    }
}

fn url_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

/// Набор профилей с одним активным. Переключение действует со следующего вызова.
#[derive(Debug, Default)]
pub struct ProviderHub {
    profiles: BTreeMap<ProviderKind, Profile>,
    active: Option<ProviderKind>,
}

impl ProviderHub {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_profile(&mut self, profile: Profile) {
        if self.active.is_none() {
            self.active = Some(profile.kind);
        }
        self.profiles.insert(profile.kind, profile);
    }

    pub fn set_active(&mut self, kind: ProviderKind) {
        self.active = Some(kind);
    }

    pub fn active(&self) -> Option<ProviderKind> {
        self.active
    }

    pub fn profile(&self, kind: ProviderKind) -> Option<&Profile> {
        self.profiles.get(&kind)
    }

    pub fn translate(
        &self,
        text: &str,
        transport: &dyn HttpTransport,
        cancel: &AtomicBool,
    ) -> Result<Translation, ProviderError> {
        let kind = self
            .active
            .ok_or(ProviderError::MissingProfile("активный"))?;
        let profile = self
            .profiles
            .get(&kind)
            .ok_or(ProviderError::MissingProfile(kind.name()))?;
        translate(profile, text, transport, cancel)
    }
}

/// Боевой HTTPS-транспорт.
pub struct ReqwestTransport {
    client: reqwest::blocking::Client,
}

impl ReqwestTransport {
    pub fn new() -> Result<Self, ProviderError> {
        let client = reqwest::blocking::Client::builder()
            .build()
            .map_err(|_| ProviderError::Network)?;
        Ok(Self { client })
    }
}

impl HttpTransport for ReqwestTransport {
    fn execute(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
        if request.cancel.load(Ordering::SeqCst) {
            return Err(TransportError::Cancelled);
        }
        let mut builder = self
            .client
            .post(&request.url)
            .timeout(request.timeout)
            .body(request.body.clone());
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        let response = builder.send().map_err(|error| {
            if error.is_timeout() {
                TransportError::Timeout
            } else {
                TransportError::Network(error.to_string())
            }
        })?;
        if request.cancel.load(Ordering::SeqCst) {
            return Err(TransportError::Cancelled);
        }
        let status = response.status().as_u16();
        let body = response
            .bytes()
            .map_err(|error| TransportError::Network(error.to_string()))?
            .to_vec();
        Ok(HttpResponse { status, body })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MockTransport {
        requests: Mutex<Vec<HttpRequest>>,
        response: Mutex<Option<Result<HttpResponse, TransportError>>>,
    }

    impl MockTransport {
        fn with_response(status: u16, body: Value) -> Self {
            let transport = Self::default();
            *transport.response.lock().unwrap() = Some(Ok(HttpResponse {
                status,
                body: serde_json::to_vec(&body).unwrap(),
            }));
            transport
        }

        fn with_error(error: TransportError) -> Self {
            let transport = Self::default();
            *transport.response.lock().unwrap() = Some(Err(error));
            transport
        }

        fn requests(&self) -> Vec<HttpRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl HttpTransport for MockTransport {
        fn execute(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
            self.requests.lock().unwrap().push(request.clone());
            self.response
                .lock()
                .unwrap()
                .take()
                .unwrap_or(Err(TransportError::Network("нет ответа".to_owned())))
        }
    }

    fn body(request: &HttpRequest) -> Value {
        serde_json::from_slice(&request.body).expect("JSON")
    }

    #[test]
    fn yandex_request_has_expected_fields() {
        let transport =
            MockTransport::with_response(200, json!({"translations": [{"text": "привет"}]}));
        let profile = Profile::yandex("yandex-key");
        let cancel = AtomicBool::new(false);
        let result = translate(&profile, "hello", &transport, &cancel).expect("перевод");
        assert_eq!(result.text, "привет");
        assert_eq!(result.provider, ProviderKind::Yandex);

        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(
            request.url,
            "https://translate.api.cloud.yandex.net/translate/v2/translate"
        );
        assert!(request
            .headers
            .iter()
            .any(|(name, value)| name == "Authorization" && value == "Api-Key yandex-key"));
        let body = body(request);
        assert_eq!(body["texts"], json!(["hello"]));
        assert_eq!(body["targetLanguageCode"], json!("ru"));
        assert!(body.get("sourceLanguageCode").is_none());
    }

    #[test]
    fn google_request_has_expected_fields_and_explicit_source() {
        let transport = MockTransport::with_response(
            200,
            json!({"data": {"translations": [{"translatedText": "привет"}]}}),
        );
        let profile = Profile::google("google-key").with_source("en");
        let cancel = AtomicBool::new(false);
        let result = translate(&profile, "hello", &transport, &cancel).expect("перевод");
        assert_eq!(result.text, "привет");
        assert_eq!(result.provider, ProviderKind::Google);

        let request = &transport.requests()[0];
        assert!(request.url.contains("key=google-key"));
        assert!(request.url.contains("/language/translate/v2"));
        let body = body(request);
        assert_eq!(body["q"], json!(["hello"]));
        assert_eq!(body["target"], json!("ru"));
        assert_eq!(body["format"], json!("text"));
        assert_eq!(body["source"], json!("en"));
        assert!(!request
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("authorization")));
    }

    #[test]
    fn switching_profile_uses_only_new_key() {
        let transport = MockTransport::with_response(
            200,
            json!({"data": {"translations": [{"translatedText": "ok"}]}}),
        );
        let mut hub = ProviderHub::new();
        hub.add_profile(Profile::yandex("yandex-key"));
        hub.add_profile(Profile::google("google-key"));
        hub.set_active(ProviderKind::Google);
        let cancel = AtomicBool::new(false);
        hub.translate("text", &transport, &cancel).expect("перевод");
        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].url.contains("google-key"));
        assert!(!requests[0].url.contains("yandex-key"));
        assert!(!requests[0]
            .headers
            .iter()
            .any(|(_, value)| value.contains("yandex-key")));
    }

    #[test]
    fn connection_check_uses_fixed_phrase() {
        let transport =
            MockTransport::with_response(200, json!({"translations": [{"text": "ok"}]}));
        let profile = Profile::yandex("key");
        let cancel = AtomicBool::new(false);
        check_connection(&profile, &transport, &cancel).expect("проверка");
        let body = body(&transport.requests()[0]);
        assert_eq!(body["texts"], json!([PROBE_TEXT]));
    }

    #[test]
    fn errors_are_distinct_and_no_second_request() {
        let cancel = AtomicBool::new(false);
        let unauthorized = MockTransport::with_response(401, json!({}));
        assert_eq!(
            translate(&Profile::google("bad"), "text", &unauthorized, &cancel),
            Err(ProviderError::Unauthorized)
        );
        let limited = MockTransport::with_response(429, json!({}));
        assert_eq!(
            translate(&Profile::google("key"), "text", &limited, &cancel),
            Err(ProviderError::RateLimited)
        );
        let timeout = MockTransport::with_error(TransportError::Timeout);
        assert_eq!(
            translate(&Profile::yandex("key"), "text", &timeout, &cancel),
            Err(ProviderError::Timeout)
        );
        assert_eq!(unauthorized.requests().len(), 1);
        assert_eq!(limited.requests().len(), 1);
        assert_eq!(timeout.requests().len(), 1);
    }

    #[test]
    fn cancellation_prevents_request() {
        let cancel = AtomicBool::new(true);
        let transport =
            MockTransport::with_response(200, json!({"translations": [{"text": "ok"}]}));
        assert_eq!(
            translate(&Profile::yandex("key"), "text", &transport, &cancel),
            Err(ProviderError::Cancelled)
        );
        assert!(transport.requests().is_empty());
    }

    #[test]
    fn missing_key_is_reported_before_network() {
        let transport = MockTransport::with_response(200, json!({}));
        let cancel = AtomicBool::new(false);
        assert!(matches!(
            translate(&Profile::google(""), "text", &transport, &cancel),
            Err(ProviderError::MissingProfile(_))
        ));
        assert!(transport.requests().is_empty());
    }
}
