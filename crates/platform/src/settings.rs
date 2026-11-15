//! Пользовательские настройки модулей и секреты профилей.
//!
//! Обычные значения лежат в JSON рядом с данными приложения, а секреты — в
//! системном хранилище текущего пользователя. В `settings.json` попадают
//! только ссылки на секреты, поэтому журнал и DTO настроек не раскрывают ключ.
//! На Windows используется Credential Manager, на остальных ОС — отдельный
//! файл с правами владельца для тестов и Linux-проверок.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Ошибка хранения настроек или секрета.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettingsError {
    #[error("настройки модуля: {0}")]
    Io(String),
    #[error("настройки модуля повреждены: {0}")]
    Invalid(String),
    #[error("хранилище секретов: {0}")]
    Secret(String),
}

/// Сохранённые значения одного модуля.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StoredSettings {
    #[serde(default)]
    pub active_profile: Option<String>,
    #[serde(default)]
    pub values: BTreeMap<String, Value>,
    #[serde(default)]
    pub profiles: BTreeMap<String, BTreeMap<String, Value>>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SecretIndex {
    #[serde(default)]
    refs: Vec<String>,
}

/// Системное хранилище секретов. Значение никогда не сериализуется в
/// настройки и не возвращается в публичных DTO.
pub trait SecretStore: Send + Sync {
    fn read(&self, module_id: &str, secret_ref: &str) -> Result<Option<String>, SettingsError>;
    fn write(&self, module_id: &str, secret_ref: &str, value: &str) -> Result<(), SettingsError>;
    fn delete(&self, module_id: &str, secret_ref: &str) -> Result<(), SettingsError>;
}

/// Хранилище настроек и секретов одного пользователя.
pub struct SettingsStore {
    root: PathBuf,
    secrets: Arc<dyn SecretStore>,
}

impl SettingsStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let secrets = platform_secret_store(&root);
        Self { root, secrets }
    }

    #[cfg(test)]
    pub fn with_secret_store(root: impl Into<PathBuf>, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            root: root.into(),
            secrets,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Значения профиля вместе с активным профилем. Если профиль не задан,
    /// используется сохранённый активный профиль.
    pub fn read(&self, module_id: &str, profile: Option<&str>) -> Result<Value, SettingsError> {
        let stored = self.load(module_id)?;
        let selected = profile
            .map(str::to_owned)
            .or_else(|| stored.active_profile.clone());
        let mut values = stored.values.clone();
        if let Some(profile) = selected.as_deref() {
            if let Some(profile_values) = stored.profiles.get(profile) {
                values.extend(profile_values.clone());
            }
        }
        if let Some(active) = selected {
            values.insert("activeProfile".to_owned(), Value::String(active));
        }
        let mut object = Map::new();
        for (key, value) in values {
            object.insert(key, value);
        }
        Ok(Value::Object(object))
    }

    /// Сохраняет обычные значения. `active_profile` и `profile` обычно
    /// совпадают: активный профиль хранится на верхнем уровне, а языки — в
    /// выбранном профиле.
    pub fn save(
        &self,
        module_id: &str,
        active_profile: Option<&str>,
        profile: Option<&str>,
        values: &BTreeMap<String, Value>,
    ) -> Result<(), SettingsError> {
        let mut stored = self.load(module_id)?;
        if let Some(active) = active_profile {
            if !active.trim().is_empty() {
                stored.active_profile = Some(active.to_owned());
            }
        }
        match profile {
            Some(profile) if !profile.trim().is_empty() => {
                stored
                    .profiles
                    .entry(profile.to_owned())
                    .or_default()
                    .extend(values.clone());
            }
            _ => stored.values.extend(values.clone()),
        }
        self.store(module_id, &stored)
    }

    pub fn active_profile(&self, module_id: &str) -> Result<Option<String>, SettingsError> {
        Ok(self.load(module_id)?.active_profile)
    }

    /// Плоские значения активного или указанного профиля.
    pub fn values(
        &self,
        module_id: &str,
        profile: Option<&str>,
    ) -> Result<BTreeMap<String, Value>, SettingsError> {
        let value = self.read(module_id, profile)?;
        match value {
            Value::Object(object) => Ok(object.into_iter().collect()),
            _ => Ok(BTreeMap::new()),
        }
    }

    /// Ссылки на сохранённые секреты без значений.
    pub fn saved_secret_refs(&self, module_id: &str) -> Result<Vec<String>, SettingsError> {
        self.secret_refs(module_id)
    }

    pub fn secret_present(&self, module_id: &str, secret_ref: &str) -> Result<bool, SettingsError> {
        Ok(self
            .secret_refs(module_id)?
            .iter()
            .any(|item| item == secret_ref))
    }

    pub fn read_secret(
        &self,
        module_id: &str,
        secret_ref: &str,
    ) -> Result<Option<String>, SettingsError> {
        if !self.secret_present(module_id, secret_ref)? {
            return Ok(None);
        }
        self.secrets.read(module_id, secret_ref)
    }

    /// Заменяет секрет. Пустое значение не стирает существующий ключ.
    pub fn write_secret(
        &self,
        module_id: &str,
        secret_ref: &str,
        value: &str,
    ) -> Result<(), SettingsError> {
        if value.is_empty() {
            return Ok(());
        }
        self.secrets.write(module_id, secret_ref, value)?;
        let mut refs = self.secret_refs(module_id)?;
        if !refs.iter().any(|item| item == secret_ref) {
            refs.push(secret_ref.to_owned());
            refs.sort();
            self.store_secret_refs(module_id, &refs)?;
        }
        Ok(())
    }

    pub fn delete_secret(&self, module_id: &str, secret_ref: &str) -> Result<(), SettingsError> {
        self.secrets.delete(module_id, secret_ref)?;
        let mut refs = self.secret_refs(module_id)?;
        refs.retain(|item| item != secret_ref);
        self.store_secret_refs(module_id, &refs)
    }

    /// Удаляет настройки и все секреты модуля. Повторный вызов безопасен.
    pub fn remove_module(&self, module_id: &str) -> Result<(), SettingsError> {
        for secret_ref in self.secret_refs(module_id)? {
            let _ = self.secrets.delete(module_id, &secret_ref);
        }
        let directory = self.module_directory(module_id);
        match std::fs::remove_dir_all(directory) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(SettingsError::Io(error.to_string())),
        }
    }

    fn module_directory(&self, module_id: &str) -> PathBuf {
        self.root.join(sanitize_component(module_id))
    }

    fn settings_path(&self, module_id: &str) -> PathBuf {
        self.module_directory(module_id).join("settings.json")
    }

    fn secrets_path(&self, module_id: &str) -> PathBuf {
        self.module_directory(module_id).join("secrets.json")
    }

    fn load(&self, module_id: &str) -> Result<StoredSettings, SettingsError> {
        let path = self.settings_path(module_id);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|error| SettingsError::Invalid(format!("{}: {error}", path.display()))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(StoredSettings::default())
            }
            Err(error) => Err(SettingsError::Io(error.to_string())),
        }
    }

    fn store(&self, module_id: &str, stored: &StoredSettings) -> Result<(), SettingsError> {
        let directory = self.module_directory(module_id);
        std::fs::create_dir_all(&directory)
            .map_err(|error| SettingsError::Io(error.to_string()))?;
        let path = self.settings_path(module_id);
        let text = serde_json::to_vec_pretty(stored)
            .map_err(|error| SettingsError::Invalid(error.to_string()))?;
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, text).map_err(|error| SettingsError::Io(error.to_string()))?;
        std::fs::rename(&temporary, &path).map_err(|error| SettingsError::Io(error.to_string()))
    }

    fn secret_refs(&self, module_id: &str) -> Result<Vec<String>, SettingsError> {
        let path = self.secrets_path(module_id);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str::<SecretIndex>(&text)
                .map(|index| index.refs)
                .map_err(|error| SettingsError::Invalid(format!("{}: {error}", path.display()))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(SettingsError::Io(error.to_string())),
        }
    }

    fn store_secret_refs(&self, module_id: &str, refs: &[String]) -> Result<(), SettingsError> {
        let directory = self.module_directory(module_id);
        std::fs::create_dir_all(&directory)
            .map_err(|error| SettingsError::Io(error.to_string()))?;
        let path = self.secrets_path(module_id);
        let text = serde_json::to_vec_pretty(&SecretIndex {
            refs: refs.to_vec(),
        })
        .map_err(|error| SettingsError::Invalid(error.to_string()))?;
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, text).map_err(|error| SettingsError::Io(error.to_string()))?;
        std::fs::rename(&temporary, &path).map_err(|error| SettingsError::Io(error.to_string()))
    }
}

fn sanitize_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(not(windows))]
fn platform_secret_store(root: &Path) -> Arc<dyn SecretStore> {
    Arc::new(FileSecretStore::new(root.join("secrets")))
}

#[cfg(windows)]
fn platform_secret_store(_root: &Path) -> Arc<dyn SecretStore> {
    Arc::new(CredentialStore)
}

/// Файловое хранилище для Linux-проверок. Значения не попадают в настройки.
#[derive(Debug)]
pub struct FileSecretStore {
    root: PathBuf,
}

impl FileSecretStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn path(&self, module_id: &str) -> PathBuf {
        self.root
            .join(format!("{}.json", sanitize_component(module_id)))
    }

    fn load(&self, module_id: &str) -> Result<BTreeMap<String, String>, SettingsError> {
        let path = self.path(module_id);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|error| SettingsError::Secret(format!("{}: {error}", path.display()))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => Err(SettingsError::Secret(error.to_string())),
        }
    }

    fn store(
        &self,
        module_id: &str,
        values: &BTreeMap<String, String>,
    ) -> Result<(), SettingsError> {
        std::fs::create_dir_all(&self.root)
            .map_err(|error| SettingsError::Secret(error.to_string()))?;
        let path = self.path(module_id);
        let text = serde_json::to_vec_pretty(values)
            .map_err(|error| SettingsError::Secret(error.to_string()))?;
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, text)
            .map_err(|error| SettingsError::Secret(error.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&temporary, &path).map_err(|error| SettingsError::Secret(error.to_string()))
    }
}

impl SecretStore for FileSecretStore {
    fn read(&self, module_id: &str, secret_ref: &str) -> Result<Option<String>, SettingsError> {
        Ok(self.load(module_id)?.get(secret_ref).cloned())
    }

    fn write(&self, module_id: &str, secret_ref: &str, value: &str) -> Result<(), SettingsError> {
        let mut values = self.load(module_id)?;
        values.insert(secret_ref.to_owned(), value.to_owned());
        self.store(module_id, &values)
    }

    fn delete(&self, module_id: &str, secret_ref: &str) -> Result<(), SettingsError> {
        let mut values = self.load(module_id)?;
        if values.remove(secret_ref).is_some() {
            self.store(module_id, &values)?;
        }
        Ok(())
    }
}

#[cfg(windows)]
struct CredentialStore;

#[cfg(windows)]
impl CredentialStore {
    fn target(module_id: &str, secret_ref: &str) -> Vec<u16> {
        format!("UtilMaker:{module_id}:{secret_ref}")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect()
    }
}

#[cfg(windows)]
impl SecretStore for CredentialStore {
    fn read(&self, module_id: &str, secret_ref: &str) -> Result<Option<String>, SettingsError> {
        use windows::core::PCWSTR;
        use windows::Win32::Security::Credentials::{
            CredFree, CredReadW, CREDENTIALW, CRED_TYPE_GENERIC,
        };
        let target = Self::target(module_id, secret_ref);
        let mut pointer: *mut CREDENTIALW = std::ptr::null_mut();
        let result = unsafe {
            CredReadW(
                PCWSTR(target.as_ptr()),
                CRED_TYPE_GENERIC,
                None,
                &mut pointer,
            )
        };
        if result.is_err() {
            return Ok(None);
        }
        if pointer.is_null() {
            return Ok(None);
        }
        let bytes = unsafe {
            let credential = &*pointer;
            if credential.CredentialBlob.is_null() || credential.CredentialBlobSize == 0 {
                Vec::new()
            } else {
                std::slice::from_raw_parts(
                    credential.CredentialBlob,
                    credential.CredentialBlobSize as usize,
                )
                .to_vec()
            }
        };
        unsafe { CredFree(pointer.cast()) };
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|error| SettingsError::Secret(format!("ключ не является UTF-8: {error}")))
    }

    fn write(&self, module_id: &str, secret_ref: &str, value: &str) -> Result<(), SettingsError> {
        use windows::core::PWSTR;
        use windows::Win32::Security::Credentials::{
            CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
        };
        let mut target = Self::target(module_id, secret_ref);
        let mut user: Vec<u16> = "util-maker"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut blob = value.as_bytes().to_vec();
        let credential = CREDENTIALW {
            Flags: Default::default(),
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(target.as_mut_ptr()),
            Comment: PWSTR(std::ptr::null_mut()),
            LastWritten: Default::default(),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            AttributeCount: 0,
            Attributes: std::ptr::null_mut(),
            TargetAlias: PWSTR(std::ptr::null_mut()),
            UserName: PWSTR(user.as_mut_ptr()),
        };
        unsafe { CredWriteW(&credential, 0) }
            .map_err(|error| SettingsError::Secret(error.to_string()))
    }

    fn delete(&self, module_id: &str, secret_ref: &str) -> Result<(), SettingsError> {
        use windows::core::PCWSTR;
        use windows::Win32::Security::Credentials::{CredDeleteW, CRED_TYPE_GENERIC};
        let target = Self::target(module_id, secret_ref);
        let result = unsafe { CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None) };
        if result.is_err() {
            // Удаление отсутствующей записи безопасно повторять.
            return Ok(());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store(name: &str) -> SettingsStore {
        let root =
            std::env::temp_dir().join(format!("util-maker-settings-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        SettingsStore::new(root)
    }

    #[test]
    fn profiles_are_isolated_and_active_profile_selects_values() {
        let store = store("profiles");
        let yandex = BTreeMap::from([
            ("sourceLang".to_owned(), json!("auto")),
            ("targetLang".to_owned(), json!("ru")),
        ]);
        let google = BTreeMap::from([
            ("sourceLang".to_owned(), json!("en")),
            ("targetLang".to_owned(), json!("de")),
        ]);
        store
            .save("org.example", Some("yandex"), Some("yandex"), &yandex)
            .expect("яндекс");
        store
            .save("org.example", None, Some("google"), &google)
            .expect("гугл");

        let active = store.read("org.example", None).expect("активный");
        assert_eq!(active["activeProfile"], json!("yandex"));
        assert_eq!(active["targetLang"], json!("ru"));

        let google = store.read("org.example", Some("google")).expect("гугл");
        assert_eq!(google["sourceLang"], json!("en"));
        assert_eq!(google["targetLang"], json!("de"));
    }

    #[test]
    fn secrets_are_separate_from_settings_and_removed_with_module() {
        let store = store("secrets");
        store
            .write_secret("org.example", "translator/yandex/apiKey", "secret-key")
            .expect("секрет");
        assert!(store
            .secret_present("org.example", "translator/yandex/apiKey")
            .expect("признак"));
        assert_eq!(
            store
                .read_secret("org.example", "translator/yandex/apiKey")
                .expect("чтение"),
            Some("secret-key".to_owned())
        );
        let settings =
            std::fs::read_to_string(store.settings_path("org.example")).unwrap_or_default();
        assert!(!settings.contains("secret-key"));

        store.remove_module("org.example").expect("очистка");
        assert_eq!(
            store
                .read_secret("org.example", "translator/yandex/apiKey")
                .expect("после очистки"),
            None
        );
        store
            .remove_module("org.example")
            .expect("повторная очистка");
    }

    #[test]
    fn different_modules_do_not_share_secrets() {
        let store = store("isolation");
        store
            .write_secret("org.example.one", "apiKey", "one")
            .expect("первый");
        store
            .write_secret("org.example.two", "apiKey", "two")
            .expect("второй");
        assert_eq!(
            store
                .read_secret("org.example.one", "apiKey")
                .expect("первый"),
            Some("one".to_owned())
        );
        assert_eq!(
            store
                .read_secret("org.example.two", "apiKey")
                .expect("второй"),
            Some("two".to_owned())
        );
    }
}
