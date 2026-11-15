//! Операции менеджера пакетов для desktop-хоста.
//!
//! Хранилище профиля и каталог активных модулей разделены: после установки
//! активная версия публикуется в каталог, который читает `CatalogState`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use semver::Version;
use serde::{Deserialize, Serialize};
use util_maker_core::ipc::ModuleProcess;
use util_maker_packages::{
    verify_path, InstalledModule, PackageError, PackagePreview, PackageStore, TrustStore,
};
use util_maker_sdk::HOST_API_VERSION;

const KEYS_FILE: &str = "trusted-keys.json";

#[derive(Debug, Default, Serialize, Deserialize)]
struct TrustedKeys {
    #[serde(default)]
    publishers: BTreeMap<String, String>,
}

/// Хранилище пакетов, доверенные ключи и каталог модулей хоста.
pub struct PackageManager {
    store: PackageStore,
    trust: TrustStore,
    catalog_root: PathBuf,
    keys_path: PathBuf,
    keys: TrustedKeys,
}

impl PackageManager {
    pub fn open(data_directory: &Path, catalog_root: PathBuf) -> Result<Self, String> {
        let profile = data_directory.join("packages");
        let store =
            PackageStore::open(&profile).map_err(|error| format!("хранилище пакетов: {error}"))?;
        let keys_path = data_directory.join(KEYS_FILE);
        let keys = if keys_path.exists() {
            serde_json::from_slice(&std::fs::read(&keys_path).map_err(|error| error.to_string())?)
                .map_err(|error| format!("доверенные ключи: {error}"))?
        } else {
            TrustedKeys::default()
        };
        let mut trust = TrustStore::new();
        for (publisher, value) in &keys.publishers {
            if let Ok(bytes) = parse_key(value) {
                let _ = trust.add_publisher(publisher, bytes);
            }
        }
        Ok(Self {
            store,
            trust,
            catalog_root,
            keys_path,
            keys,
        })
    }

    pub fn preview(
        &mut self,
        package: &Path,
        publisher: &str,
        key_hex: &str,
    ) -> Result<PackagePreview, PackageError> {
        let key = parse_key(key_hex)
            .map_err(|error| PackageError::Malformed(format!("ключ издателя: {error}")))?;
        self.trust.add_publisher(publisher, key)?;
        let verified = verify_path(package, &self.trust, platform(), &host_version())?;
        Ok(verified.preview)
    }

    /// Устанавливает проверенный пакет, сохраняет ключ и публикует модуль.
    pub fn install(
        &mut self,
        package: &Path,
        publisher: &str,
        key_hex: &str,
    ) -> Result<InstalledModule, PackageError> {
        let key = parse_key(key_hex)
            .map_err(|error| PackageError::Malformed(format!("ключ издателя: {error}")))?;
        self.trust.add_publisher(publisher, key)?;
        let installed = self
            .store
            .install(package, &self.trust, platform(), &host_version())?;
        if let Err(error) = handshake(&installed) {
            // Кандидат не прошёл рукопожатие: возвращаем предыдущую пару.
            if self.store.rollback(&installed.id).is_err() {
                let _ = self.store.remove(&installed.id);
            }
            if let Some(previous) = self.store.module(&installed.id) {
                let _ = self.store.publish_to(&previous.id, &self.catalog_root);
            }
            return Err(PackageError::Install(format!(
                "кандидат не прошёл рукопожатие: {error}"
            )));
        }
        self.store.publish_to(&installed.id, &self.catalog_root)?;
        self.keys
            .publishers
            .insert(publisher.to_owned(), key_hex.to_ascii_lowercase());
        self.save_keys()?;
        Ok(installed)
    }

    pub fn rollback(&mut self, id: &str) -> Result<InstalledModule, PackageError> {
        let module = self.store.rollback(id)?;
        self.store.publish_to(id, &self.catalog_root)?;
        Ok(module)
    }

    pub fn remove(&mut self, id: &str) -> Result<bool, PackageError> {
        let removed = self.store.remove(id)?;
        let directory = self.catalog_root.join(id);
        if directory.exists() {
            std::fs::remove_dir_all(directory).map_err(PackageError::Io)?;
        }
        Ok(removed)
    }

    fn save_keys(&self) -> Result<(), PackageError> {
        let bytes = serde_json::to_vec_pretty(&self.keys)
            .map_err(|error| PackageError::Install(format!("доверенные ключи: {error}")))?;
        std::fs::write(&self.keys_path, bytes).map_err(PackageError::Io)
    }
}

fn handshake(installed: &InstalledModule) -> Result<(), String> {
    let mut process = ModuleProcess::spawn(&installed.entrypoint)
        .map_err(|error| format!("запуск {}: {error}", installed.entrypoint.display()))?;
    process
        .initialize(&installed.id)
        .map_err(|error| format!("initialize: {error}"))?;
    process.shutdown();
    Ok(())
}

fn parse_key(value: &str) -> Result<[u8; 32], String> {
    let clean = value.trim().trim_start_matches("0x");
    if clean.len() != 64 || !clean.chars().all(|character| character.is_ascii_hexdigit()) {
        return Err("ожидается 64 шестнадцатеричных символа".to_owned());
    }
    let mut key = [0u8; 32];
    for (index, chunk) in clean.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let text = std::str::from_utf8(chunk).map_err(|error| error.to_string())?;
        key[index] = u8::from_str_radix(text, 16).map_err(|error| error.to_string())?;
    }
    Ok(key)
}

fn platform() -> &'static str {
    if cfg!(windows) {
        "windows-x64"
    } else {
        "linux-x64"
    }
}

fn host_version() -> Version {
    Version::parse(HOST_API_VERSION).unwrap_or_else(|_| Version::new(1, 0, 0))
}
