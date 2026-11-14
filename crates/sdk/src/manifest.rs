//! Манифест микроприложения: обязательные поля, проверка и безопасный путь точки входа.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

/// Версия схемы манифеста, которую понимает этот SDK.
pub const MANIFEST_VERSION: u32 = 1;

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

    const CONTROL: &str = include_str!("../../control-module/manifest.json");

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
            PathBuf::from("bin/control-module.exe")
        );
    }

    #[test]
    fn rejects_parent_directory_entrypoint() {
        let text = CONTROL.replace("bin/control-module.exe", "../control-module.exe");
        let manifest = Manifest::from_json(&text).unwrap();
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn rejects_unknown_manifest_version() {
        let text = CONTROL.replace("\"manifestVersion\": 1", "\"manifestVersion\": 2");
        let manifest = Manifest::from_json(&text).unwrap();
        assert!(manifest.validate().is_err());
    }
}
