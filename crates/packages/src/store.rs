//! Установка проверенного пакета в профиль пользователя.
//!
//! Пакет распаковывается только после проверки подписи, путей и хешей. Версия
//! сначала пишется во временный каталог и публикуется атомарной заменой; запись
//! каталога и `installed.json` не оставляет частичную установку.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use semver::Version;
use serde::{Deserialize, Serialize};
use util_maker_sdk::Manifest;
use zip::ZipArchive;

use crate::{verify_path, PackageError, TrustStore, MANIFEST_ENTRY, SIGNATURE_ENTRY};

const STATE_FILE: &str = "installed.json";

/// Опубликованная версия модуля.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledModule {
    pub id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
    pub entrypoint: PathBuf,
    pub manifest_path: PathBuf,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreState {
    #[serde(default)]
    modules: BTreeMap<String, InstalledModule>,
    #[serde(default)]
    previous: BTreeMap<String, InstalledModule>,
}

/// Каталог установленных версий и активная запись профиля.
pub struct PackageStore {
    root: PathBuf,
    state: StoreState,
}

impl PackageStore {
    /// Открывает профиль и читает `installed.json`. Каталог создаётся при необходимости.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, PackageError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(root.join("modules")).map_err(PackageError::Io)?;
        let state_path = root.join(STATE_FILE);
        let state = if state_path.exists() {
            let bytes = fs::read(&state_path).map_err(PackageError::Io)?;
            serde_json::from_slice(&bytes)
                .map_err(|error| PackageError::Install(format!("installed.json: {error}")))?
        } else {
            StoreState::default()
        };
        Ok(Self { root, state })
    }

    pub fn modules(&self) -> Vec<InstalledModule> {
        self.state.modules.values().cloned().collect()
    }

    pub fn module(&self, id: &str) -> Option<&InstalledModule> {
        self.state.modules.get(id)
    }

    pub fn previous(&self, id: &str) -> Option<&InstalledModule> {
        self.state.previous.get(id)
    }

    /// Возвращает предыдущую версию и её метаданные.
    pub fn rollback(&mut self, id: &str) -> Result<InstalledModule, PackageError> {
        let previous =
            self.state.previous.remove(id).ok_or_else(|| {
                PackageError::Install(format!("нет предыдущей версии модуля {id}"))
            })?;
        let active = self.state.modules.insert(id.to_owned(), previous.clone());
        if let Err(error) = self.write_state() {
            self.state
                .modules
                .insert(id.to_owned(), active.expect("активная версия"));
            self.state.previous.insert(id.to_owned(), previous);
            return Err(error);
        }
        Ok(previous)
    }

    /// Проверяет пакет и публикует его версию. Повторные те же байты идемпотентны.
    pub fn install(
        &mut self,
        package: &Path,
        trust: &TrustStore,
        platform: &str,
        host_api: &Version,
    ) -> Result<InstalledModule, PackageError> {
        let verified = verify_path(package, trust, platform, host_api)?;
        let manifest = verified.manifest;
        let preview = verified.preview;
        if let Some(existing) = self.state.modules.get(&manifest.id) {
            if existing.publisher != manifest.publisher {
                return Err(PackageError::PublisherConflict(format!(
                    "модуль {} уже установлен от издателя {}, а пакет подписан {}",
                    manifest.id, existing.publisher, manifest.publisher
                )));
            }
            if existing.version == manifest.version {
                return Ok(existing.clone());
            }
        }

        let module_dir = self.root.join("modules").join(&manifest.id);
        let target = module_dir.join(&manifest.version);
        let temp = module_dir.join(format!(".tmp-{}", manifest.version));
        if temp.exists() {
            fs::remove_dir_all(&temp).map_err(PackageError::Io)?;
        }
        fs::create_dir_all(&temp).map_err(PackageError::Io)?;

        let result = self.extract(package, &manifest, &temp);
        if let Err(error) = result {
            let _ = fs::remove_dir_all(&temp);
            return Err(error);
        }

        if target.exists() {
            let _ = fs::remove_dir_all(&temp);
        } else {
            fs::rename(&temp, &target).map_err(PackageError::Io)?;
        }

        let entrypoint = target.join(
            manifest
                .resolved_entrypoint(platform)
                .map_err(|error| PackageError::UnsupportedPlatform(error.to_string()))?,
        );
        let manifest_path = target.join(MANIFEST_ENTRY);
        let installed = InstalledModule {
            id: manifest.id.clone(),
            name: manifest.name.clone(),
            publisher: manifest.publisher.clone(),
            version: manifest.version.clone(),
            entrypoint,
            manifest_path,
        };
        let previous_active = self.state.modules.get(&installed.id).cloned();
        let previous_map = self.state.previous.clone();
        if let Some(old) = previous_active
            .as_ref()
            .filter(|old| old.version != installed.version)
        {
            self.state
                .previous
                .insert(installed.id.clone(), old.clone());
        }
        self.state
            .modules
            .insert(installed.id.clone(), installed.clone());
        if let Err(error) = self.write_state() {
            // Не оставляем активную запись, если метаданные не записались.
            self.state.previous = previous_map;
            if let Some(old) = previous_active {
                self.state.modules.insert(installed.id.clone(), old);
            } else {
                self.state.modules.remove(&installed.id);
            }
            let _ = fs::remove_dir_all(&target);
            return Err(error);
        }
        let _ = preview;
        Ok(installed)
    }

    fn extract(
        &self,
        package: &Path,
        manifest: &Manifest,
        directory: &Path,
    ) -> Result<(), PackageError> {
        let file = File::open(package).map_err(PackageError::Io)?;
        let mut archive = ZipArchive::new(file)?;
        for payload in &manifest.files {
            let mut entry = archive
                .by_name(&payload.path)
                .map_err(|_| PackageError::MissingFile(payload.path.clone()))?;
            let destination = directory.join(&payload.path);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(PackageError::Io)?;
            }
            let mut output = File::create(&destination).map_err(PackageError::Io)?;
            std::io::copy(&mut entry, &mut output).map_err(PackageError::Io)?;
        }
        let manifest_json = serde_json::to_vec_pretty(manifest)
            .map_err(|error| PackageError::Install(format!("манифест: {error}")))?;
        fs::write(directory.join(MANIFEST_ENTRY), manifest_json).map_err(PackageError::Io)?;
        let _ = SIGNATURE_ENTRY;
        Ok(())
    }

    fn write_state(&self) -> Result<(), PackageError> {
        let path = self.root.join(STATE_FILE);
        let temp = self.root.join(format!("{STATE_FILE}.tmp"));
        let bytes = serde_json::to_vec_pretty(&self.state)
            .map_err(|error| PackageError::Install(format!("installed.json: {error}")))?;
        {
            let mut file = File::create(&temp).map_err(PackageError::Io)?;
            file.write_all(&bytes).map_err(PackageError::Io)?;
            file.sync_all().map_err(PackageError::Io)?;
        }
        fs::rename(&temp, &path).map_err(PackageError::Io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::{export_package, ExportOptions};
    use ed25519_dalek::SigningKey;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("util-maker-{prefix}-{nanos}"))
    }

    fn package(
        id: &str,
        publisher: &str,
        version: &str,
        seed: u8,
    ) -> (PathBuf, TrustStore, PathBuf) {
        let root = unique("store");
        let source = root.join("source");
        fs::create_dir_all(source.join("assets")).unwrap();
        fs::write(source.join("plugin"), b"binary").unwrap();
        fs::write(source.join("assets/icon.png"), b"icon").unwrap();
        let manifest = format!(
            r#"{{"manifestVersion":1,"id":"{id}","publisher":"{publisher}","name":"Тест","version":"{version}","hostApi":">=1.0.0 <2.0.0","entrypoints":{{"windows-x64":"plugin.exe","linux-x64":"plugin"}},"icon":"assets/icon.png","capabilities":[],"actions":[{{"id":"run","name":"Запуск"}}]}}"#
        );
        let manifest_path = root.join("manifest.json");
        fs::write(&manifest_path, manifest).unwrap();
        let output = root.join(format!("{id}-{version}.umpkg"));
        let signing_key = SigningKey::from_bytes(&[seed; 32]);
        export_package(&ExportOptions {
            source: &source,
            manifest: &manifest_path,
            output: &output,
            publisher,
            signing_key: &signing_key,
            version: None,
            platform: Some("linux-x64"),
        })
        .unwrap();
        let mut trust = TrustStore::new();
        trust
            .add_publisher(publisher, signing_key.verifying_key().to_bytes())
            .unwrap();
        (output, trust, root)
    }

    #[test]
    fn installs_and_reopens_profile_without_running_module() {
        let (package_path, trust, root) = package("com.example.one", "com.example", "0.1.0", 1);
        let profile = root.join("profile");
        let mut store = PackageStore::open(&profile).unwrap();
        let installed = store
            .install(&package_path, &trust, "linux-x64", &Version::new(1, 0, 0))
            .unwrap();
        assert_eq!(installed.version, "0.1.0");
        assert!(installed.entrypoint.ends_with("plugin"));
        assert!(installed.entrypoint.is_file());

        let reopened = PackageStore::open(&profile).unwrap();
        assert_eq!(reopened.modules(), vec![installed.clone()]);
    }

    #[test]
    fn repeated_install_of_same_bytes_keeps_one_record() {
        let (package_path, trust, root) = package("com.example.two", "com.example", "0.1.0", 2);
        let profile = root.join("profile");
        let mut store = PackageStore::open(&profile).unwrap();
        let first = store
            .install(&package_path, &trust, "linux-x64", &Version::new(1, 0, 0))
            .unwrap();
        let second = store
            .install(&package_path, &trust, "linux-x64", &Version::new(1, 0, 0))
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(store.modules().len(), 1);
    }

    #[test]
    fn another_publisher_cannot_replace_known_id() {
        let (first, trust_one, root) = package("com.example.three", "com.example", "0.1.0", 3);
        let (second, trust_two, _) = package("com.example.three", "other.example", "0.2.0", 4);
        let profile = root.join("profile");
        let mut store = PackageStore::open(&profile).unwrap();
        store
            .install(&first, &trust_one, "linux-x64", &Version::new(1, 0, 0))
            .unwrap();
        let error = store
            .install(&second, &trust_two, "linux-x64", &Version::new(1, 0, 0))
            .unwrap_err();
        assert!(matches!(error, PackageError::PublisherConflict(_)));
        assert_eq!(store.modules().len(), 1);
    }

    #[test]
    fn update_keeps_previous_version_for_rollback() {
        let (v1, trust, root) = package("com.example.rollback", "com.example", "0.1.0", 6);
        let (v2, _, _) = package("com.example.rollback", "com.example", "0.2.0", 6);
        let profile = root.join("profile");
        let mut store = PackageStore::open(&profile).unwrap();
        store
            .install(&v1, &trust, "linux-x64", &Version::new(1, 0, 0))
            .unwrap();
        store
            .install(&v2, &trust, "linux-x64", &Version::new(1, 0, 0))
            .unwrap();
        assert_eq!(
            store.module("com.example.rollback").unwrap().version,
            "0.2.0"
        );
        assert_eq!(
            store.previous("com.example.rollback").unwrap().version,
            "0.1.0"
        );
        let rolled = store.rollback("com.example.rollback").unwrap();
        assert_eq!(rolled.version, "0.1.0");
        assert_eq!(
            store.module("com.example.rollback").unwrap().version,
            "0.1.0"
        );
        let reopened = PackageStore::open(&profile).unwrap();
        assert_eq!(
            reopened.module("com.example.rollback").unwrap().version,
            "0.1.0"
        );
    }

    #[test]
    fn corrupt_package_does_not_publish_partial_install() {
        let (package_path, trust, root) = package("com.example.four", "com.example", "0.1.0", 5);
        fs::write(&package_path, b"not a zip").unwrap();
        let profile = root.join("profile");
        let mut store = PackageStore::open(&profile).unwrap();
        assert!(store
            .install(&package_path, &trust, "linux-x64", &Version::new(1, 0, 0))
            .is_err());
        assert!(PackageStore::open(&profile).unwrap().modules().is_empty());
    }
}
