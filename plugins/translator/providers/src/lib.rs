//! HTTP-адаптеры перевода: Yandex Translate API v2 и Google Cloud Translation
//! Basic v2. Транспорт отделён трейтом, поэтому поля запроса, авторизация,
//! разбор ответа и ошибки проверяются на тестовом HTTP-адаптере.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Короткая фиксированная фраза для проверки подключения.
pub const PROBE_TEXT: &str = "Util Maker";
/// Таймаут одного перевода.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Автоопределение исходного языка.
pub const AUTO_SOURCE_LANGUAGE: &str = "auto";
/// Целевой язык по умолчанию.
pub const DEFAULT_TARGET_LANGUAGE: &str = "ru";
/// Ссылка на секрет профиля Яндекса.
pub const YANDEX_SECRET_REF: &str = "translator/yandex/apiKey";
/// Ссылка на секрет профиля Google.
pub const GOOGLE_SECRET_REF: &str = "translator/google/apiKey";

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

    /// Стабильный ID профиля в настройках.
    pub fn id(self) -> &'static str {
        match self {
            Self::Yandex => "yandex",
            Self::Google => "google",
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        match value {
            "yandex" => Some(Self::Yandex),
            "google" => Some(Self::Google),
            _ => None,
        }
    }

    /// Ссылка на запись системного хранилища с ключом этого профиля.
    pub fn default_secret_ref(self) -> &'static str {
        match self {
            Self::Yandex => YANDEX_SECRET_REF,
            Self::Google => GOOGLE_SECRET_REF,
        }
    }
}

/// Сохраняемая часть профиля: языки и ссылка на ключ. Сам ключ хранится
/// отдельно и в настройках не появляется.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSettings {
    pub id: String,
    /// `None` — автоопределение исходного языка сервисом.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_language: Option<String>,
    #[serde(default = "default_target_language")]
    pub target_language: String,
    #[serde(default)]
    pub secret_ref: String,
}

fn default_target_language() -> String {
    DEFAULT_TARGET_LANGUAGE.to_owned()
}

impl ProfileSettings {
    pub fn yandex() -> Self {
        Self {
            id: ProviderKind::Yandex.id().to_owned(),
            source_language: None,
            target_language: DEFAULT_TARGET_LANGUAGE.to_owned(),
            secret_ref: YANDEX_SECRET_REF.to_owned(),
        }
    }

    pub fn google() -> Self {
        Self {
            id: ProviderKind::Google.id().to_owned(),
            source_language: None,
            target_language: DEFAULT_TARGET_LANGUAGE.to_owned(),
            secret_ref: GOOGLE_SECRET_REF.to_owned(),
        }
    }

    pub fn kind(&self) -> Result<ProviderKind, ProviderError> {
        ProviderKind::from_id(&self.id).ok_or_else(|| {
            ProviderError::InvalidSettings(format!("неизвестный профиль: {}", self.id))
        })
    }

    /// Явный исходный язык; `None` означает автоопределение.
    pub fn source(&self) -> Option<String> {
        self.source_language
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case(AUTO_SOURCE_LANGUAGE))
            .map(str::to_owned)
    }

    pub fn effective_secret_ref(&self) -> Result<String, ProviderError> {
        let kind = self.kind()?;
        if self.secret_ref.trim().is_empty() {
            Ok(kind.default_secret_ref().to_owned())
        } else {
            Ok(self.secret_ref.clone())
        }
    }

    fn from_value(value: &Value, fallback_id: Option<&str>) -> Result<Self, ProviderError> {
        let object = value.as_object().ok_or_else(|| {
            ProviderError::InvalidSettings("профиль должен быть объектом".to_owned())
        })?;
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .or(fallback_id)
            .ok_or_else(|| ProviderError::InvalidSettings("у профиля нет id".to_owned()))?;
        let kind = ProviderKind::from_id(id)
            .ok_or_else(|| ProviderError::InvalidSettings(format!("неизвестный профиль: {id}")))?;
        let source_language =
            language_field(object, &["sourceLang", "sourceLanguage"]).unwrap_or(None);
        let target_language = string_field(object, &["targetLang", "targetLanguage"])
            .unwrap_or_else(|| DEFAULT_TARGET_LANGUAGE.to_owned());
        let secret_ref = string_field(object, &["secretRef"])
            .unwrap_or_else(|| kind.default_secret_ref().to_owned());
        Ok(Self {
            id: id.to_owned(),
            source_language,
            target_language,
            secret_ref,
        })
    }
}

/// Настройки всех профилей и активный профиль.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_profile: Option<String>,
    #[serde(default)]
    pub profiles: Vec<ProfileSettings>,
}

impl ProviderSettings {
    /// Оба профиля с автоопределением и целевым русским.
    pub fn defaults() -> Self {
        Self {
            active_profile: Some(ProviderKind::Yandex.id().to_owned()),
            profiles: vec![ProfileSettings::yandex(), ProfileSettings::google()],
        }
    }

    /// Читает состояние формы: профили с языками и ссылками на ключи либо
    /// плоские значения активного профиля.
    pub fn from_value(value: &Value) -> Result<Self, ProviderError> {
        if value.is_null() {
            return Ok(Self::defaults());
        }
        let object = value.as_object().ok_or_else(|| {
            ProviderError::InvalidSettings("настройки должны быть объектом".to_owned())
        })?;
        let active = string_field(object, &["activeProfile"]);
        let mut profiles = Vec::new();
        match object.get("profiles") {
            None => {}
            Some(Value::Array(items)) => {
                for item in items {
                    profiles.push(ProfileSettings::from_value(item, None)?);
                }
            }
            Some(Value::Object(items)) => {
                for (id, item) in items {
                    profiles.push(ProfileSettings::from_value(item, Some(id))?);
                }
            }
            Some(_) => {
                return Err(ProviderError::InvalidSettings(
                    "profiles должен быть массивом или объектом".to_owned(),
                ))
            }
        }
        if profiles.is_empty() {
            profiles = Self::defaults().profiles;
        }
        let flat = object
            .get("values")
            .and_then(Value::as_object)
            .unwrap_or(object);
        let source = language_field(flat, &["sourceLang", "sourceLanguage"]);
        let target = string_field(flat, &["targetLang", "targetLanguage"]);
        let selected = active
            .clone()
            .unwrap_or_else(|| ProviderKind::Yandex.id().to_owned());
        if let Some(profile) = profiles.iter_mut().find(|profile| profile.id == selected) {
            if let Some(source) = source {
                profile.source_language = source;
            }
            if let Some(target) = target {
                profile.target_language = target;
            }
        }
        let settings = Self {
            active_profile: active,
            profiles,
        };
        settings.validate()?;
        Ok(settings)
    }

    /// Проверяет уникальность профилей, языки и ссылки на секреты.
    pub fn validate(&self) -> Result<(), ProviderError> {
        let mut seen = BTreeSet::new();
        for profile in &self.profiles {
            let kind = profile.kind()?;
            if !seen.insert(kind) {
                return Err(ProviderError::InvalidSettings(format!(
                    "профиль {} объявлен дважды",
                    profile.id
                )));
            }
            profile.effective_secret_ref()?;
            if profile.target_language.trim().is_empty() {
                return Err(ProviderError::InvalidSettings(format!(
                    "у профиля {} не задан целевой язык",
                    profile.id
                )));
            }
        }
        if let Some(active) = &self.active_profile {
            let kind = ProviderKind::from_id(active).ok_or_else(|| {
                ProviderError::InvalidSettings(format!("неизвестный активный профиль: {active}"))
            })?;
            if !seen.contains(&kind) {
                return Err(ProviderError::InvalidSettings(format!(
                    "активный профиль {active} не найден"
                )));
            }
        }
        Ok(())
    }

    pub fn active_kind(&self) -> Result<ProviderKind, ProviderError> {
        let id = self
            .active_profile
            .as_deref()
            .unwrap_or(ProviderKind::Yandex.id());
        ProviderKind::from_id(id)
            .ok_or_else(|| ProviderError::InvalidSettings(format!("неизвестный профиль: {id}")))
    }

    pub fn profile(&self, kind: ProviderKind) -> Option<&ProfileSettings> {
        self.profiles.iter().find(|profile| profile.id == kind.id())
    }
}

fn string_field(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn language_field(
    object: &serde_json::Map<String, Value>,
    keys: &[&str],
) -> Option<Option<String>> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(Value::as_str))
        .map(|value| {
            let value = value.trim();
            if value.is_empty() || value.eq_ignore_ascii_case(AUTO_SOURCE_LANGUAGE) {
                None
            } else {
                Some(value.to_owned())
            }
        })
}

/// Профиль подключения. Ключ относится только к этому сервису.
#[derive(Clone, PartialEq, Eq)]
pub struct Profile {
    pub kind: ProviderKind,
    pub api_key: String,
    /// Ссылка на запись хранилища, из которой получен ключ.
    pub secret_ref: String,
    /// `None` — автоопределение исходного языка.
    pub source: Option<String>,
    pub target: String,
}

impl Profile {
    fn new(kind: ProviderKind, api_key: impl Into<String>) -> Self {
        Self {
            kind,
            api_key: api_key.into(),
            secret_ref: kind.default_secret_ref().to_owned(),
            source: None,
            target: DEFAULT_TARGET_LANGUAGE.to_owned(),
        }
    }

    pub fn yandex(api_key: impl Into<String>) -> Self {
        Self::new(ProviderKind::Yandex, api_key)
    }

    pub fn google(api_key: impl Into<String>) -> Self {
        Self::new(ProviderKind::Google, api_key)
    }

    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    pub fn with_target(mut self, target: impl Into<String>) -> Self {
        self.target = target.into();
        self
    }

    pub fn from_settings(
        settings: &ProfileSettings,
        api_key: impl Into<String>,
    ) -> Result<Self, ProviderError> {
        let kind = settings.kind()?;
        if settings.target_language.trim().is_empty() {
            return Err(ProviderError::InvalidSettings(
                "не задан целевой язык".to_owned(),
            ));
        }
        Ok(Self {
            kind,
            api_key: api_key.into(),
            secret_ref: settings.effective_secret_ref()?,
            source: settings.source(),
            target: settings.target_language.clone(),
        })
    }

    pub fn has_key(&self) -> bool {
        !self.api_key.trim().is_empty()
    }

    pub fn is_auto_source(&self) -> bool {
        self.source.is_none()
    }
}

/// Ключ не попадает в отладочный вывод и журнал.
impl std::fmt::Debug for Profile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Profile")
            .field("kind", &self.kind)
            .field("api_key", &"<скрыт>")
            .field("secret_ref", &self.secret_ref)
            .field("source", &self.source)
            .field("target", &self.target)
            .finish()
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
    #[error("текст перевода не передан")]
    EmptyText,
    #[error("настройки профилей некорректны: {0}")]
    InvalidSettings(String),
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
    if text.is_empty() {
        return Err(ProviderError::EmptyText);
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

    /// Собирает профили из настроек. Ключ запрашивается по ссылке на секрет;
    /// профиль без ключа остаётся в наборе, чтобы вызывающая сторона открыла
    /// именно его страницу.
    pub fn from_settings<F>(
        settings: &ProviderSettings,
        mut key_for: F,
    ) -> Result<Self, ProviderError>
    where
        F: FnMut(&str) -> Option<String>,
    {
        settings.validate()?;
        let mut hub = Self::default();
        for config in &settings.profiles {
            let secret_ref = config.effective_secret_ref()?;
            let profile = Profile::from_settings(config, key_for(&secret_ref).unwrap_or_default())?;
            let kind = profile.kind;
            hub.profiles.insert(kind, profile);
        }
        let kind = settings.active_kind()?;
        if !hub.profiles.contains_key(&kind) {
            return Err(ProviderError::InvalidSettings(format!(
                "активный профиль {} не найден",
                kind.id()
            )));
        }
        hub.active = Some(kind);
        Ok(hub)
    }

    pub fn profile(&self, kind: ProviderKind) -> Option<&Profile> {
        self.profiles.get(&kind)
    }

    pub fn active_profile(&self) -> Option<&Profile> {
        self.active.and_then(|kind| self.profiles.get(&kind))
    }

    /// Активный профиль без ключа: запрос не отправляется.
    pub fn missing_key_profile(&self) -> Option<&Profile> {
        self.active_profile().filter(|profile| !profile.has_key())
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
