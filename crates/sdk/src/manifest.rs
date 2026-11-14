//! Манифест микроприложения: обязательные поля, проверка и безопасный путь точки входа.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

/// Версия схемы манифеста, которую понимает этот SDK.
pub const MANIFEST_VERSION: u32 = 1;
/// Версия схемы пользовательских настроек в манифесте.
pub const SETTINGS_SCHEMA_VERSION: u32 = 1;

/// Файл полезной нагрузки пакета с ожидаемым SHA-256.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PackageFile {
    pub path: String,
    pub sha256: String,
}

/// Описание пользовательских настроек модуля. Поле необязательно, поэтому
/// манифесты без формы читаются без изменений.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SettingsSchema {
    pub schema_version: u32,
    /// Поле верхнего уровня, выбирающее активный профиль.
    #[serde(default)]
    pub profile_field: Option<String>,
    #[serde(default)]
    pub fields: Vec<SettingField>,
    #[serde(default)]
    pub profiles: Vec<SettingsProfile>,
    #[serde(default)]
    pub commands: Vec<FormCommand>,
}

/// Тип значения поля формы.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingKind {
    Text,
    Number,
    Boolean,
    Select,
    Secret,
}

/// Вариант выбора для поля типа `select`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SettingOption {
    pub value: String,
    pub label: String,
}

/// Поле формы. Секретное поле содержит только ссылку на запись системного
/// хранилища; значение секрета в манифесте не хранится.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SettingField {
    pub id: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: SettingKind,
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    #[serde(default)]
    pub options: Vec<SettingOption>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub secret_ref: Option<String>,
}

/// Именованный профиль настроек, например провайдер перевода.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SettingsProfile {
    pub id: String,
    pub name: String,
    pub fields: Vec<SettingField>,
}

/// Команда формы, которую модуль выполняет без отдельного действия.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FormCommand {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Поле, задающее профиль, если команда работает с профилями.
    #[serde(default)]
    pub profile_field: Option<String>,
    /// Поля, значения которых передаются команде.
    #[serde(default)]
    pub fields: Vec<String>,
}

/// Публичное состояние формы: значения обычных полей и только признаки
/// наличия сохранённых секретов.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FormState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_profile: Option<String>,
    #[serde(default)]
    pub values: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub secrets: BTreeMap<String, SecretPresence>,
}

/// Состояние секретного поля без самого значения.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SecretPresence {
    pub secret_ref: String,
    pub present: bool,
}

/// Описание микроприложения из `.umpkg`/каталога установки.
///
/// Поля объявлены в camelCase, чтобы файл читался без преобразований.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub manifest_version: u32,
    pub id: String,
    pub publisher: String,
    pub name: String,
    pub version: String,
    pub host_api: String,
    #[serde(default)]
    pub entrypoints: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub actions: Vec<Action>,
    #[serde(default)]
    pub settings: Option<SettingsSchema>,
    #[serde(default)]
    pub files: Vec<PackageFile>,
}

/// Действие, которое модуль объявляет в манифесте.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Action {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub recommended_hotkeys: Vec<String>,
}

impl Action {
    /// Человекочитаемое описание для интерфейса.
    pub fn description(&self) -> &str {
        self.description.as_deref().unwrap_or("")
    }
}

/// Ошибка чтения или проверки манифеста.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("не удалось прочитать манифест {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("манифест не является корректным JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(String),
}

impl Manifest {
    /// Разбирает манифест из JSON без проверки полей.
    pub fn from_json(text: &str) -> Result<Self, ManifestError> {
        Ok(serde_json::from_str(text)?)
    }

    /// Читает и проверяет манифест по пути.
    pub fn load(path: &Path) -> Result<Self, ManifestError> {
        let text = std::fs::read_to_string(path).map_err(|source| ManifestError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let manifest = Self::from_json(&text)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Проверяет обязательные поля и инварианты манифеста.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.manifest_version != MANIFEST_VERSION {
            return Err(invalid(format!(
                "неподдерживаемая версия схемы манифеста: {}",
                self.manifest_version
            )));
        }
        for (field, value) in [
            ("id", &self.id),
            ("publisher", &self.publisher),
            ("name", &self.name),
            ("version", &self.version),
            ("hostApi", &self.host_api),
        ] {
            if value.trim().is_empty() {
                return Err(invalid(format!("поле {field} не заполнено")));
            }
        }
        if !is_identifier(&self.id) {
            return Err(invalid(format!("недопустимый ID модуля: {}", self.id)));
        }
        if !is_identifier(&self.publisher) {
            return Err(invalid(format!(
                "недопустимый издатель: {}",
                self.publisher
            )));
        }
        Version::parse(&self.version)
            .map_err(|error| invalid(format!("версия пакета {}: {error}", self.version)))?;
        parse_version_req(&self.host_api)
            .map_err(|error| invalid(format!("диапазон hostApi {}: {error}", self.host_api)))?;
        if self.name.chars().count() > 128 {
            return Err(invalid("отображаемое имя длиннее 128 символов"));
        }
        if self.entrypoints.is_empty() {
            return Err(invalid("в манифесте нет ни одной точки входа"));
        }
        for (platform, entrypoint) in &self.entrypoints {
            if !is_platform_key(platform) {
                return Err(invalid(format!("недопустимая платформа: {platform}")));
            }
            validate_relative_path(entrypoint)
                .map_err(|reason| invalid(format!("точка входа {platform}: {reason}")))?;
        }
        if let Some(icon) = &self.icon {
            validate_relative_path(icon).map_err(|reason| invalid(format!("иконка: {reason}")))?;
        }
        let mut names = BTreeSet::new();
        for capability in &self.capabilities {
            if capability.trim().is_empty() {
                return Err(invalid("пустое имя возможности"));
            }
            if !names.insert(capability) {
                return Err(invalid(format!(
                    "возможность {capability} объявлена дважды"
                )));
            }
        }
        if self.actions.is_empty() {
            return Err(invalid("в манифесте нет ни одного действия"));
        }
        let mut actions = BTreeSet::new();
        for action in &self.actions {
            if !is_identifier(&action.id) {
                return Err(invalid(format!("недопустимый ID действия: {}", action.id)));
            }
            if action.name.trim().is_empty() {
                return Err(invalid(format!("действие {} без названия", action.id)));
            }
            if !actions.insert(action.id.as_str()) {
                return Err(invalid(format!("действие {} объявлено дважды", action.id)));
            }
        }
        if let Some(settings) = &self.settings {
            settings.validate()?;
        }
        Ok(())
    }

    /// Требование к версии API хоста в виде диапазона semver.
    pub fn host_api_requirement(&self) -> Result<VersionReq, ManifestError> {
        parse_version_req(&self.host_api)
            .map_err(|error| invalid(format!("диапазон hostApi {}: {error}", self.host_api)))
    }

    /// Совместим ли модуль с указанной версией API хоста.
    pub fn supports_host_api(&self, host: &Version) -> Result<bool, ManifestError> {
        Ok(self.host_api_requirement()?.matches(host))
    }

    /// Точка входа для ключа вида `windows-x64`.
    pub fn entrypoint_for(&self, platform: &str) -> Option<&str> {
        self.entrypoints.get(platform).map(String::as_str)
    }

    /// Действие по стабильному ID.
    pub fn action(&self, id: &str) -> Option<&Action> {
        self.actions.iter().find(|action| action.id == id)
    }

    /// Проверенный относительный путь точки входа для платформы.
    pub fn resolved_entrypoint(&self, platform: &str) -> Result<PathBuf, ManifestError> {
        let entrypoint = self
            .entrypoint_for(platform)
            .ok_or_else(|| invalid(format!("нет точки входа для платформы {platform}")))?;
        validate_relative_path(entrypoint)
            .map_err(|reason| invalid(format!("точка входа {platform}: {reason}")))?;
        Ok(PathBuf::from(entrypoint))
    }
}

impl SettingsSchema {
    /// Проверяет схему формы и все ссылки между профилями, полями и командами.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.schema_version != SETTINGS_SCHEMA_VERSION {
            return Err(invalid(format!(
                "неподдерживаемая версия схемы настроек: {}",
                self.schema_version
            )));
        }
        if self.fields.is_empty() && self.profiles.is_empty() && self.commands.is_empty() {
            return Err(invalid("схема настроек пуста"));
        }

        let mut top_fields = BTreeSet::new();
        let mut secret_refs = BTreeSet::new();
        for field in &self.fields {
            validate_setting_field(field, "settings")?;
            if !top_fields.insert(field.id.as_str()) {
                return Err(invalid(format!(
                    "поле настроек {} объявлено дважды",
                    field.id
                )));
            }
            register_secret_ref(field, &mut secret_refs)?;
        }

        let mut profiles = BTreeSet::new();
        let mut profile_ids = BTreeSet::new();
        for profile in &self.profiles {
            if !is_identifier(&profile.id) {
                return Err(invalid(format!("недопустимый ID профиля: {}", profile.id)));
            }
            if profile.name.trim().is_empty() {
                return Err(invalid(format!("профиль {} без названия", profile.id)));
            }
            if profile.name.chars().count() > 128 {
                return Err(invalid(format!(
                    "название профиля {} длиннее 128 символов",
                    profile.id
                )));
            }
            if profile.fields.is_empty() {
                return Err(invalid(format!("профиль {} не содержит полей", profile.id)));
            }
            if !profiles.insert(profile.id.as_str()) {
                return Err(invalid(format!("профиль {} объявлен дважды", profile.id)));
            }
            profile_ids.insert(profile.id.as_str());
            let mut fields = BTreeSet::new();
            for field in &profile.fields {
                validate_setting_field(field, &format!("профиль {}", profile.id))?;
                if !fields.insert(field.id.as_str()) {
                    return Err(invalid(format!(
                        "поле {} объявлено дважды в профиле {}",
                        field.id, profile.id
                    )));
                }
                register_secret_ref(field, &mut secret_refs)?;
            }
        }

        for profile in &self.profiles {
            for field in &profile.fields {
                if top_fields.contains(field.id.as_str()) {
                    return Err(invalid(format!(
                        "поле {} не может быть одновременно верхним и профильным",
                        field.id
                    )));
                }
            }
        }

        if let Some(profile_field) = &self.profile_field {
            let field = self.top_field(profile_field).ok_or_else(|| {
                invalid(format!("поле активного профиля {profile_field} не найдено"))
            })?;
            if field.kind != SettingKind::Select {
                return Err(invalid(format!(
                    "поле активного профиля {profile_field} должно быть select"
                )));
            }
            if profiles.is_empty() {
                return Err(invalid("активный профиль указан, но профилей нет"));
            }
            let options: BTreeSet<&str> = field
                .options
                .iter()
                .map(|option| option.value.as_str())
                .collect();
            if options != profile_ids {
                return Err(invalid(format!(
                    "варианты поля {profile_field} должны совпадать с ID профилей"
                )));
            }
            if let Some(default) = field.default.as_ref().and_then(serde_json::Value::as_str) {
                if !profile_ids.contains(default) {
                    return Err(invalid(format!(
                        "значение по умолчанию {default} не является профилем"
                    )));
                }
            }
        } else if self.profiles.len() > 1 {
            return Err(invalid(
                "для нескольких профилей нужно поле profileField с активным профилем",
            ));
        }

        let mut command_ids = BTreeSet::new();
        for command in &self.commands {
            if !is_local_identifier(&command.id) {
                return Err(invalid(format!(
                    "недопустимый ID команды формы: {}",
                    command.id
                )));
            }
            if command.name.trim().is_empty() {
                return Err(invalid(format!("команда {} без названия", command.id)));
            }
            if !command_ids.insert(command.id.as_str()) {
                return Err(invalid(format!("команда {} объявлена дважды", command.id)));
            }
            if let Some(profile_field) = &command.profile_field {
                if self.profile_field.as_deref() != Some(profile_field.as_str()) {
                    return Err(invalid(format!(
                        "команда {} ссылается на неизвестное поле профиля {profile_field}",
                        command.id
                    )));
                }
                if !self.top_field(profile_field).is_some_and(|field| {
                    field.kind == SettingKind::Select && !field.options.is_empty()
                }) {
                    return Err(invalid(format!(
                        "команда {} ссылается на поле профиля без вариантов",
                        command.id
                    )));
                }
            }
            for field_id in &command.fields {
                let found = if self.profiles.is_empty() {
                    self.top_field(field_id).is_some()
                } else if command.profile_field.is_some() {
                    self.profiles
                        .iter()
                        .all(|profile| profile.fields.iter().any(|field| field.id == *field_id))
                } else {
                    self.top_field(field_id).is_some()
                };
                if !found {
                    return Err(invalid(format!(
                        "команда {} ссылается на неизвестное поле {field_id}",
                        command.id
                    )));
                }
            }
        }
        Ok(())
    }

    /// Поле верхнего уровня по ID.
    pub fn top_field(&self, id: &str) -> Option<&SettingField> {
        self.fields.iter().find(|field| field.id == id)
    }

    /// Профиль по ID.
    pub fn profile(&self, id: &str) -> Option<&SettingsProfile> {
        self.profiles.iter().find(|profile| profile.id == id)
    }

    /// Собирает публичное состояние формы. Значения секретных полей никогда не
    /// попадают в результат, даже если переданы в `values`.
    pub fn form_state(
        &self,
        active_profile: Option<&str>,
        values: &BTreeMap<String, serde_json::Value>,
        saved_secrets: &BTreeSet<String>,
    ) -> Result<FormState, ManifestError> {
        let selected = match active_profile {
            Some(id) => Some(
                self.profile(id)
                    .ok_or_else(|| invalid(format!("профиль настроек {id} не найден")))?,
            ),
            None => self.profiles.first(),
        };
        let mut state = FormState {
            active_profile: selected.map(|profile| profile.id.clone()),
            ..FormState::default()
        };
        let fields = self.fields.iter().chain(
            selected
                .into_iter()
                .flat_map(|profile| profile.fields.iter()),
        );
        for field in fields {
            if field.kind == SettingKind::Secret {
                if let Some(secret_ref) = &field.secret_ref {
                    state.secrets.insert(
                        field.id.clone(),
                        SecretPresence {
                            secret_ref: secret_ref.clone(),
                            present: saved_secrets.contains(secret_ref),
                        },
                    );
                }
                continue;
            }
            let value = values
                .get(&field.id)
                .cloned()
                .or_else(|| field.default.clone());
            if let Some(value) = value {
                state.values.insert(field.id.clone(), value);
            }
        }
        Ok(state)
    }
}

fn validate_setting_field(field: &SettingField, region: &str) -> Result<(), ManifestError> {
    if !is_local_identifier(&field.id) {
        return Err(invalid(format!(
            "недопустимый ID поля настроек в {region}: {}",
            field.id
        )));
    }
    if field.label.trim().is_empty() {
        return Err(invalid(format!("поле {} без названия", field.id)));
    }
    if field.label.chars().count() > 128 {
        return Err(invalid(format!(
            "название поля {} длиннее 128 символов",
            field.id
        )));
    }
    if field.kind == SettingKind::Secret {
        if field.secret_ref.as_deref().is_none_or(str::is_empty) {
            return Err(invalid(format!(
                "секретное поле {} должно ссылаться на запись хранилища",
                field.id
            )));
        }
        if field.default.is_some() {
            return Err(invalid(format!(
                "секретное поле {} не может иметь значение по умолчанию",
                field.id
            )));
        }
        if !field.options.is_empty() {
            return Err(invalid(format!(
                "секретное поле {} не может иметь вариантов",
                field.id
            )));
        }
        return Ok(());
    }
    if field.secret_ref.is_some() {
        return Err(invalid(format!(
            "обычное поле {} не может ссылаться на секрет",
            field.id
        )));
    }
    let mut options = BTreeSet::new();
    for option in &field.options {
        if option.value.trim().is_empty() || option.label.trim().is_empty() {
            return Err(invalid(format!(
                "поле {} содержит пустой вариант",
                field.id
            )));
        }
        if !options.insert(option.value.as_str()) {
            return Err(invalid(format!(
                "поле {} содержит повтор варианта {}",
                field.id, option.value
            )));
        }
    }
    if field.kind == SettingKind::Select && field.options.is_empty() {
        return Err(invalid(format!(
            "поле {} типа select без вариантов",
            field.id
        )));
    }
    if field.kind != SettingKind::Select && !field.options.is_empty() {
        return Err(invalid(format!(
            "поле {} не select, но содержит варианты",
            field.id
        )));
    }
    if let Some(default) = &field.default {
        let valid = match field.kind {
            SettingKind::Text => default.is_string(),
            SettingKind::Number => default.is_number(),
            SettingKind::Boolean => default.is_boolean(),
            SettingKind::Select => default
                .as_str()
                .is_some_and(|value| options.contains(value)),
            SettingKind::Secret => false,
        };
        if !valid {
            return Err(invalid(format!(
                "значение по умолчанию поля {} не соответствует типу или вариантам",
                field.id
            )));
        }
    }
    Ok(())
}

fn register_secret_ref<'a>(
    field: &'a SettingField,
    seen: &mut BTreeSet<&'a str>,
) -> Result<(), ManifestError> {
    if let Some(secret_ref) = &field.secret_ref {
        if !is_secret_ref(secret_ref) {
            return Err(invalid(format!(
                "недопустимая ссылка на секрет в поле {}: {secret_ref}",
                field.id
            )));
        }
        if !seen.insert(secret_ref.as_str()) {
            return Err(invalid(format!(
                "ссылка на секрет {secret_ref} объявлена дважды"
            )));
        }
    }
    Ok(())
}

fn is_secret_ref(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && value.split('/').all(is_local_identifier)
}

/// ID поля и команды формы допускает camelCase, в отличие от ID модуля.
fn is_local_identifier(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 128
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

/// Диапазон версий в манифесте записывается как в ТЗ: `>=1.0.0 <2.0.0`.
/// Крейт `semver` ожидает запятые между компараторами, поэтому пробелы
/// нормализуются перед разбором.
fn parse_version_req(value: &str) -> Result<VersionReq, semver::Error> {
    let normalized = value
        .replace(',', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(", ");
    VersionReq::parse(&normalized)
}

fn invalid(message: impl Into<String>) -> ManifestError {
    ManifestError::Invalid(message.into())
}

fn is_identifier(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 128
        && bytes[0].is_ascii_alphanumeric()
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        })
}

fn is_platform_key(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_lowercase()
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

/// Относительный путь внутри каталога пакета: без корня, префикса диска и `..`.
fn validate_relative_path(value: &str) -> Result<(), &'static str> {
    if value.trim().is_empty() {
        return Err("пустой путь");
    }
    if value.contains('\\') {
        return Err("обратные слэши не допускаются");
    }
    let path = Path::new(value);
    if path.is_absolute() {
        return Err("абсолютный путь не допускается");
    }
    for component in path.components() {
        match component {
            Component::Normal(part) if !part.is_empty() => {}
            Component::CurDir => return Err("сегмент `.` не допускается"),
            Component::ParentDir => return Err("выход из каталога запрещён"),
            Component::RootDir | Component::Prefix(_) => {
                return Err("абсолютный путь не допускается")
            }
            Component::Normal(_) => return Err("пустой сегмент пути"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CONTROL: &str = include_str!("../../../plugins/control/manifest.json");

    const SETTINGS: &str = r#"{
      "schemaVersion": 1,
      "profileField": "activeProfile",
      "fields": [
        {
          "id": "activeProfile",
          "label": "Активный профиль",
          "type": "select",
          "default": "yandex",
          "options": [
            { "value": "yandex", "label": "Яндекс" },
            { "value": "google", "label": "Google" }
          ]
        }
      ],
      "profiles": [
        {
          "id": "yandex",
          "name": "Яндекс",
          "fields": [
            {
              "id": "sourceLang",
              "label": "Исходный язык",
              "type": "select",
              "default": "auto",
              "options": [
                { "value": "auto", "label": "Авто" },
                { "value": "en", "label": "Английский" },
                { "value": "ru", "label": "Русский" }
              ]
            },
            {
              "id": "targetLang",
              "label": "Целевой язык",
              "type": "select",
              "default": "ru",
              "options": [
                { "value": "en", "label": "Английский" },
                { "value": "ru", "label": "Русский" }
              ]
            },
            {
              "id": "apiKey",
              "label": "API-ключ",
              "type": "secret",
              "required": true,
              "secretRef": "translator/yandex/apiKey"
            }
          ]
        },
        {
          "id": "google",
          "name": "Google",
          "fields": [
            {
              "id": "sourceLang",
              "label": "Исходный язык",
              "type": "select",
              "default": "auto",
              "options": [
                { "value": "auto", "label": "Авто" },
                { "value": "en", "label": "Английский" },
                { "value": "ru", "label": "Русский" }
              ]
            },
            {
              "id": "targetLang",
              "label": "Целевой язык",
              "type": "select",
              "default": "ru",
              "options": [
                { "value": "en", "label": "Английский" },
                { "value": "ru", "label": "Русский" }
              ]
            },
            {
              "id": "apiKey",
              "label": "API-ключ",
              "type": "secret",
              "required": true,
              "secretRef": "translator/google/apiKey"
            }
          ]
        }
      ],
      "commands": [
        {
          "id": "testConnection",
          "name": "Проверить подключение",
          "profileField": "activeProfile",
          "fields": ["sourceLang", "targetLang", "apiKey"]
        }
      ]
    }"#;

    fn control_with_settings(settings: &str) -> String {
        CONTROL.replace(
            "\"capabilities\": [],",
            &format!("\"capabilities\": [],\n  \"settings\": {settings},"),
        )
    }

    #[test]
    fn control_manifest_is_valid() {
        let manifest = Manifest::from_json(CONTROL).expect("JSON");
        manifest.validate().expect("манифест контрольного модуля");
        assert_eq!(manifest.id, "org.utilmaker.control");
        assert!(manifest
            .supports_host_api(&Version::parse("1.0.0").unwrap())
            .unwrap());
        assert!(!manifest
            .supports_host_api(&Version::parse("2.0.0").unwrap())
            .unwrap());
        assert!(manifest.action("ping").is_some());
        assert_eq!(
            manifest.resolved_entrypoint("windows-x64").unwrap(),
            PathBuf::from("control-module.exe")
        );
    }

    #[test]
    fn rejects_parent_directory_entrypoint() {
        let text = CONTROL.replace(
            "\"windows-x64\": \"control-module.exe\"",
            "\"windows-x64\": \"../control-module.exe\"",
        );
        assert_ne!(text, CONTROL);
        let manifest = Manifest::from_json(&text).unwrap();
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn rejects_unknown_manifest_version() {
        let text = CONTROL.replace("\"manifestVersion\": 1", "\"manifestVersion\": 2");
        let manifest = Manifest::from_json(&text).unwrap();
        assert!(manifest.validate().is_err());
    }
    #[test]
    fn settings_schema_is_valid_and_public_state_hides_secret() {
        let text = control_with_settings(SETTINGS);
        let manifest = Manifest::from_json(&text).unwrap();
        manifest.validate().expect("схема настроек");
        let settings = manifest.settings.as_ref().unwrap();
        assert_eq!(settings.profiles.len(), 2);

        let values = BTreeMap::from([
            ("sourceLang".to_owned(), json!("en")),
            ("apiKey".to_owned(), json!("secret-value")),
        ]);
        let saved = BTreeSet::from(["translator/yandex/apiKey".to_owned()]);
        let state = settings
            .form_state(Some("yandex"), &values, &saved)
            .expect("состояние формы");
        let serialized = serde_json::to_string(&state).unwrap();
        assert_eq!(state.active_profile.as_deref(), Some("yandex"));
        assert_eq!(state.values["sourceLang"], json!("en"));
        assert_eq!(state.values["targetLang"], json!("ru"));
        assert!(state.secrets["apiKey"].present);
        assert!(!serialized.contains("secret-value"));
        assert!(serialized.contains("translator/yandex/apiKey"));
    }

    #[test]
    fn rejects_dangling_settings_references() {
        let missing_field = SETTINGS.replace(
            "\"fields\": [\"sourceLang\", \"targetLang\", \"apiKey\"]",
            "\"fields\": [\"sourceLang\", \"missing\"]",
        );
        let manifest = Manifest::from_json(&control_with_settings(&missing_field)).unwrap();
        assert!(manifest.validate().is_err());

        let missing_profile = SETTINGS.replace(
            "\"profileField\": \"activeProfile\"",
            "\"profileField\": \"absentProfile\"",
        );
        let manifest = Manifest::from_json(&control_with_settings(&missing_profile)).unwrap();
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn rejects_secret_default_and_non_select_options() {
        let secret_default = SETTINGS.replace(
            "\"type\": \"secret\",\n              \"required\": true",
            "\"type\": \"secret\",\n              \"default\": \"plain\",\n              \"required\": true",
        );
        assert_ne!(secret_default, SETTINGS);
        let manifest = Manifest::from_json(&control_with_settings(&secret_default)).unwrap();
        assert!(manifest.validate().is_err());

        let text_with_options = SETTINGS.replace(
            "\"id\": \"targetLang\",\n              \"label\": \"Целевой язык\",\n              \"type\": \"select\"",
            "\"id\": \"targetLang\",\n              \"label\": \"Целевой язык\",\n              \"type\": \"text\"",
        );
        assert_ne!(text_with_options, SETTINGS);
        let manifest = Manifest::from_json(&control_with_settings(&text_with_options)).unwrap();
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn old_manifest_has_no_settings() {
        let manifest = Manifest::from_json(CONTROL).unwrap();
        manifest.validate().expect("старый манифест");
        assert!(manifest.settings.is_none());
    }
}
