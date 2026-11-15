//! Установка проверенного `.umpkg` в профиль пользователя.
//!
//! Пакет проверяется и распаковывается из одних и тех же байтов. Содержимое
//! версии сначала пишется во временный каталог, затем каталог переименовывается,
//! и только после этого атомарно публикуется активная запись в `installed.json`.
//! Отказ проверки, распаковки или записи индекса не меняет действующую запись.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use util_maker_sdk::manifest::Manifest;
use zip::read::ZipFile;
use zip::ZipArchive;

use crate::{
    verify_reader, PackageError, PackagePreview, TrustStore, MANIFEST_ENTRY, SIGNATURE_ENTRY,
};

const INDEX_FILE: &str = "installed.json";
const INDEX_VERSION: u32 = 1;
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Ошибка установки. Любая ошибка оставляет прежнюю активную версию на месте.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("не удалось прочитать пакет или профиль: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Verification(#[from] PackageError),
    #[error("состояние установленных пакетов повреждено: {0}")]
    State(String),
    #[error("JSON состояния установленных пакетов: {0}")]
    Json(#[from] serde_json::Error),
    #[error(
        "пакет {id} уже установлен с издателем {installed}, новый пакет от издателя {incoming}"
    )]
    PublisherConflict {
        id: String,
        installed: String,
        incoming: String,
    },
    #[error("версия {version} пакета {id} уже установлена с другим содержимым")]
    VersionConflict { id: String, version: String },
    #[error("каталог версии уже существует: {0}")]
    DestinationExists(String),
}

/// Установленный пакет. `executable` указывает на активную версию.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledPackage {
    pub id: String,
    pub publisher: String,
    pub version: String,
    pub manifest: Manifest,
    pub enabled: bool,
    pub executable: PathBuf,
    pub package_sha256: String,
    pub platform: String,
}

/// Результат установки или идемпотентного повтора.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallResult {
    pub package: InstalledPackage,
    pub preview: PackagePreview,
    pub already_installed: bool,
    pub previous_version: Option<String>,
}

/// Профиль установленных пакетов.
///
/// Активная версия каждого ID хранится в `installed.json`, а распакованное
/// содержимое — в `<профиль>/<id>/<версия>/`. Чтение каталога не запускает
/// исполняемые файлы.
#[derive(Debug)]
pub struct PackageStore {
    root: PathBuf,
    // Сериализует установки через один экземпляр хранилища. Индекс
    // перечитывается под блокировкой, поэтому внешние изменения не теряются
    // между последовательными вызовами.
    lock: Mutex<()>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoreIndex {
    version: u32,
    #[serde(default)]
    packages: BTreeMap<String, PackageRecord>,
}

impl Default for StoreIndex {
    fn default() -> Self {
        Self {
            version: INDEX_VERSION,
            packages: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackageRecord {
    id: String,
    publisher: String,
    active_version: String,
    enabled: bool,
    versions: BTreeMap<String, VersionRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VersionRecord {
    platform: String,
    package_sha256: String,
}

impl PackageStore {
    /// Открывает или создаёт профиль. Повреждённый индекс не игнорируется.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, InstallError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        let store = Self {
            root,
            lock: Mutex::new(()),
        };
        store.load_index()?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Проверяет и устанавливает пакет по пути. Файл читается один раз: те же
    /// байты проверяются и распаковываются.
    pub fn install_path(
        &self,
        path: impl AsRef<Path>,
        trust: &TrustStore,
        platform: &str,
        host_api: &Version,
    ) -> Result<InstallResult, InstallError> {
        let bytes = fs::read(path)?;
        self.install_bytes(&bytes, trust, platform, host_api)
    }

    /// Проверяет и устанавливает пакет из байтов.
    pub fn install_bytes(
        &self,
        bytes: &[u8],
        trust: &TrustStore,
        platform: &str,
        host_api: &Version,
    ) -> Result<InstallResult, InstallError> {
        let verified = verify_reader(Cursor::new(bytes), trust, platform, host_api)?;
        let package_sha256 = hex::encode(Sha256::digest(bytes));

        let _guard = self.lock.lock().expect("блокировка установки");
        let mut index = self.load_index()?;
        let manifest = verified.manifest.clone();

        if let Some(record) = index.packages.get(&manifest.id) {
            if record.publisher != manifest.publisher {
                return Err(InstallError::PublisherConflict {
                    id: manifest.id.clone(),
                    installed: record.publisher.clone(),
                    incoming: manifest.publisher.clone(),
                });
            }
            if let Some(existing) = record.versions.get(&manifest.version) {
                if !existing
                    .package_sha256
                    .eq_ignore_ascii_case(&package_sha256)
                {
                    return Err(InstallError::VersionConflict {
                        id: manifest.id.clone(),
                        version: manifest.version.clone(),
                    });
                }
                let package = self.read_version(record, &manifest.version)?;
                return Ok(InstallResult {
                    previous_version: if record.active_version == manifest.version {
                        None
                    } else {
                        Some(record.active_version.clone())
                    },
                    package,
                    preview: verified.preview,
                    already_installed: true,
                });
            }
        }

        let version_dir = self.version_dir(&manifest.id, &manifest.version);
        if version_dir.exists() {
            return Err(InstallError::DestinationExists(
                version_dir.display().to_string(),
            ));
        }
        let parent = version_dir
            .parent()
            .ok_or_else(|| InstallError::State("у каталога версии нет родителя".to_owned()))?;
        fs::create_dir_all(parent)?;

        let staging = self.staging_dir();
        if let Err(error) = extract_package(
            bytes,
            &verified.manifest_json,
            &verified.signature,
            &manifest,
            platform,
            &staging,
        ) {
            let _ = fs::remove_dir_all(&staging);
            return Err(error.into());
        }
        if let Err(error) = fs::rename(&staging, &version_dir) {
            let _ = fs::remove_dir_all(&staging);
            return Err(error.into());
        }

        let previous_version = index
            .packages
            .get(&manifest.id)
            .map(|record| record.active_version.clone());
        let record = index
            .packages
            .entry(manifest.id.clone())
            .or_insert_with(|| PackageRecord {
                id: manifest.id.clone(),
                publisher: manifest.publisher.clone(),
                active_version: manifest.version.clone(),
                enabled: true,
                versions: BTreeMap::new(),
            });
        record.active_version = manifest.version.clone();
        record.versions.insert(
            manifest.version.clone(),
            VersionRecord {
                platform: platform.to_owned(),
                package_sha256,
            },
        );

        if let Err(error) = self.write_index(&index) {
            // Активная запись не изменилась, поэтому удаляем только что
            // распакованную версию, чтобы не оставлять неопубликованный каталог.
            let _ = fs::remove_dir_all(&version_dir);
            return Err(error);
        }

        let record = index
            .packages
            .get(&manifest.id)
            .expect("запись установленного пакета");
        let package = self.read_version(record, &manifest.version)?;
        Ok(InstallResult {
            package,
            preview: verified.preview,
            already_installed: false,
            previous_version,
        })
    }

    /// Активные записи профиля. Манифесты читаются с диска, процессы не
    /// запускаются.
    pub fn installed(&self) -> Result<Vec<InstalledPackage>, InstallError> {
        let index = self.load_index()?;
        index
            .packages
            .values()
            .map(|record| self.read_version(record, &record.active_version))
            .collect()
    }

    /// Активная запись одного ID.
    pub fn package(&self, id: &str) -> Result<Option<InstalledPackage>, InstallError> {
        let index = self.load_index()?;
        match index.packages.get(id) {
            Some(record) => Ok(Some(self.read_version(record, &record.active_version)?)),
            None => Ok(None),
        }
    }

    /// Меняет сохранённое состояние пакета. Возвращает `true`, если состояние
    /// действительно изменилось.
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<bool, InstallError> {
        let _guard = self.lock.lock().expect("блокировка установки");
        let mut index = self.load_index()?;
        let record = index
            .packages
            .get_mut(id)
            .ok_or_else(|| InstallError::State(format!("пакет {id} не установлен в профиле")))?;
        if record.enabled == enabled {
            return Ok(false);
        }
        record.enabled = enabled;
        self.write_index(&index)?;
        Ok(true)
    }

    fn load_index(&self) -> Result<StoreIndex, InstallError> {
        let path = self.index_path();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(StoreIndex::default());
            }
            Err(error) => return Err(error.into()),
        };
        let index: StoreIndex = serde_json::from_slice(&bytes)?;
        if index.version != INDEX_VERSION {
            return Err(InstallError::State(format!(
                "неподдерживаемая версия индекса {}",
                index.version
            )));
        }
        for (id, record) in &index.packages {
            if record.id != *id {
                return Err(InstallError::State(format!(
                    "запись {id} содержит другой ID {}",
                    record.id
                )));
            }
            if record.publisher.trim().is_empty() {
                return Err(InstallError::State(format!(
                    "у пакета {id} не указан издатель"
                )));
            }
            if !record.versions.contains_key(&record.active_version) {
                return Err(InstallError::State(format!(
                    "активная версия {} пакета {id} отсутствует в индексе",
                    record.active_version
                )));
            }
            for (version, entry) in &record.versions {
                Version::parse(version).map_err(|error| {
                    InstallError::State(format!("версия {version} пакета {id}: {error}"))
                })?;
                if entry.platform.trim().is_empty() {
                    return Err(InstallError::State(format!(
                        "у версии {version} пакета {id} не указана платформа"
                    )));
                }
                if entry.package_sha256.len() != 64
                    || !entry
                        .package_sha256
                        .chars()
                        .all(|value| value.is_ascii_hexdigit())
                {
                    return Err(InstallError::State(format!(
                        "у версии {version} пакета {id} некорректный SHA-256"
                    )));
                }
            }
        }
        Ok(index)
    }

    fn write_index(&self, index: &StoreIndex) -> Result<(), InstallError> {
        let path = self.index_path();
        let temporary = self.root.join(format!(
            ".{INDEX_FILE}.{}.{}.tmp",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let mut bytes = serde_json::to_vec_pretty(index)?;
        bytes.push(b'\n');
        {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        #[cfg(windows)]
        if path.exists() {
            // std::fs::rename на Windows не заменяет существующий файл.
            fs::remove_file(&path)?;
        }
        if let Err(error) = fs::rename(&temporary, &path) {
            let _ = fs::remove_file(&temporary);
            return Err(error.into());
        }
        Ok(())
    }

    fn read_version(
        &self,
        record: &PackageRecord,
        version: &str,
    ) -> Result<InstalledPackage, InstallError> {
        let entry = record.versions.get(version).ok_or_else(|| {
            InstallError::State(format!(
                "версия {version} пакета {} отсутствует в индексе",
                record.id
            ))
        })?;
        let directory = self.version_dir(&record.id, version);
        let manifest = Manifest::load(&directory.join(MANIFEST_ENTRY)).map_err(|error| {
            InstallError::State(format!(
                "манифест установленного пакета {}: {error}",
                record.id
            ))
        })?;
        if manifest.id != record.id
            || manifest.publisher != record.publisher
            || manifest.version != version
        {
            return Err(InstallError::State(format!(
                "манифест {} не совпадает с записью установленного пакета",
                directory.display()
            )));
        }
        let entrypoint = manifest
            .resolved_entrypoint(&entry.platform)
            .map_err(|error| {
                InstallError::State(format!(
                    "точка входа {} для {}: {error}",
                    record.id, entry.platform
                ))
            })?;
        let executable = directory.join(entrypoint);
        if !executable.is_file() {
            return Err(InstallError::State(format!(
                "не найден исполняемый файл {}",
                executable.display()
            )));
        }
        Ok(InstalledPackage {
            id: record.id.clone(),
            publisher: record.publisher.clone(),
            version: version.to_owned(),
            manifest,
            enabled: record.enabled,
            executable,
            package_sha256: entry.package_sha256.clone(),
            platform: entry.platform.clone(),
        })
    }

    fn index_path(&self) -> PathBuf {
        self.root.join(INDEX_FILE)
    }

    fn version_dir(&self, id: &str, version: &str) -> PathBuf {
        self.root.join(id).join(version)
    }

    fn staging_dir(&self) -> PathBuf {
        self.root.join(format!(
            ".install-{}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::SeqCst)
        ))
    }
}

fn extract_package(
    bytes: &[u8],
    manifest_bytes: &[u8],
    signature_bytes: &[u8],
    manifest: &Manifest,
    platform: &str,
    destination: &Path,
) -> Result<(), PackageError> {
    fs::create_dir(destination)?;
    write_file(&destination.join(MANIFEST_ENTRY), manifest_bytes, 0o644)?;
    write_file(&destination.join(SIGNATURE_ENTRY), signature_bytes, 0o644)?;

    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    for file in &manifest.files {
        let mut entry = archive.by_name(&file.path).map_err(|error| match error {
            zip::result::ZipError::FileNotFound => PackageError::MissingFile(file.path.clone()),
            other => PackageError::Zip(other),
        })?;
        if entry.is_dir() || is_symlink(&entry) {
            return Err(PackageError::UnsafePath(file.path.clone()));
        }
        let mode = entry.unix_mode().unwrap_or(0o644) & 0o777;
        let target = destination.join(&file.path);
        let parent = target
            .parent()
            .ok_or_else(|| PackageError::UnsafePath(file.path.clone()))?;
        fs::create_dir_all(parent)?;
        let mut output = File::create(&target)?;
        io::copy(&mut entry, &mut output)?;
        output.sync_all()?;
        set_mode(&target, mode)?;
    }
    if let Ok(entrypoint) = manifest.resolved_entrypoint(platform) {
        make_executable(&destination.join(entrypoint))?;
    }
    Ok(())
}

fn write_file(path: &Path, bytes: &[u8], mode: u32) -> Result<(), PackageError> {
    let mut file = File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    set_mode(path, mode)
}

fn is_symlink<R: Read>(entry: &ZipFile<'_, R>) -> bool {
    entry
        .unix_mode()
        .map(|mode| mode & 0o170000 == 0o120000)
        .unwrap_or(false)
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), PackageError> {
    use std::os::unix::fs::PermissionsExt;
    let current = fs::metadata(path)?.permissions().mode();
    fs::set_permissions(path, fs::Permissions::from_mode(current | 0o755))?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), PackageError> {
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), PackageError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<(), PackageError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use util_maker_sdk::manifest::PackageFile;
    use util_maker_sdk::Action;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    const PUBLISHER: &str = "org.utilmaker";
    const MODULE_ID: &str = "org.utilmaker.control";
    const HOST_API: &str = ">=1.0.0 <2.0.0";

    fn signer(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn trust(signers: &[(&str, &SigningKey)]) -> TrustStore {
        let mut trust = TrustStore::new();
        for (publisher, signer) in signers {
            trust
                .add_publisher(publisher, signer.verifying_key().to_bytes())
                .expect("ключ");
        }
        trust
    }

    fn manifest(publisher: &str, version: &str, payload: &[u8]) -> Manifest {
        Manifest {
            manifest_version: 1,
            id: MODULE_ID.to_owned(),
            publisher: publisher.to_owned(),
            name: "Контрольный пакет".to_owned(),
            version: version.to_owned(),
            host_api: HOST_API.to_owned(),
            entrypoints: BTreeMap::from([("linux-x64".to_owned(), "bin/module".to_owned())]),
            icon: None,
            capabilities: Vec::new(),
            actions: vec![Action {
                id: "ping".to_owned(),
                name: "Проверить".to_owned(),
                description: None,
                recommended_hotkeys: Vec::new(),
            }],
            settings: None,
            files: vec![PackageFile {
                path: "bin/module".to_owned(),
                sha256: hex::encode(Sha256::digest(payload)),
            }],
        }
    }

    fn package(manifest: &Manifest, payload: &[u8], signer: &SigningKey) -> Vec<u8> {
        let manifest_bytes = serde_json::to_vec(manifest).expect("манифест");
        let signature = signer.sign(&manifest_bytes);
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        writer
            .start_file(MANIFEST_ENTRY, options)
            .expect("манифест");
        writer.write_all(&manifest_bytes).expect("запись");
        writer
            .start_file(SIGNATURE_ENTRY, options)
            .expect("подпись");
        writer.write_all(&signature.to_bytes()).expect("запись");
        writer.start_file("bin/module", options).expect("файл");
        writer.write_all(payload).expect("запись");
        writer.finish().expect("архив").into_inner()
    }

    fn install(
        store: &PackageStore,
        bytes: &[u8],
        trust: &TrustStore,
    ) -> Result<InstallResult, InstallError> {
        store.install_bytes(
            bytes,
            trust,
            "linux-x64",
            &Version::parse("1.0.0").expect("версия"),
        )
    }

    #[test]
    fn installs_package_and_reopens_profile_without_launching() {
        let temp = tempfile::tempdir().expect("каталог");
        let signer = signer(1);
        let trust = trust(&[(PUBLISHER, &signer)]);
        let manifest = manifest(PUBLISHER, "0.2.0", b"module bytes");
        let bytes = package(&manifest, b"module bytes", &signer);

        let store = PackageStore::open(temp.path()).expect("профиль");
        let result = install(&store, &bytes, &trust).expect("установка");
        assert!(!result.already_installed);
        assert_eq!(result.package.id, MODULE_ID);
        assert_eq!(result.package.version, "0.2.0");
        assert!(result.package.enabled);
        assert_eq!(
            result.package.executable,
            temp.path().join(MODULE_ID).join("0.2.0").join("bin/module")
        );
        assert_eq!(
            fs::read(&result.package.executable).expect("файл"),
            b"module bytes"
        );

        let reopened = PackageStore::open(temp.path()).expect("повторное открытие");
        let installed = reopened.installed().expect("каталог");
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].manifest.id, MODULE_ID);
        assert_eq!(installed[0].manifest.version, "0.2.0");
        assert_eq!(installed[0].executable, result.package.executable);
        assert_eq!(
            reopened.package(MODULE_ID).expect("пакет").unwrap().version,
            "0.2.0"
        );
    }

    #[test]
    fn corrupt_package_is_rejected_without_publishing() {
        let temp = tempfile::tempdir().expect("каталог");
        let signer = signer(2);
        let trust = trust(&[(PUBLISHER, &signer)]);
        let manifest = manifest(PUBLISHER, "0.2.0", b"expected");
        let bytes = package(&manifest, b"tampered", &signer);

        let store = PackageStore::open(temp.path()).expect("профиль");
        let error = install(&store, &bytes, &trust).expect_err("отказ");
        assert!(matches!(
            error,
            InstallError::Verification(PackageError::HashMismatch(_))
        ));
        assert!(store.installed().expect("каталог").is_empty());
        assert!(!temp.path().join(MODULE_ID).exists());
    }

    #[test]
    fn repeated_install_of_same_bytes_keeps_one_record() {
        let temp = tempfile::tempdir().expect("каталог");
        let signer = signer(3);
        let trust = trust(&[(PUBLISHER, &signer)]);
        let manifest = manifest(PUBLISHER, "0.2.0", b"module bytes");
        let bytes = package(&manifest, b"module bytes", &signer);

        let store = PackageStore::open(temp.path()).expect("профиль");
        let first = install(&store, &bytes, &trust).expect("первая установка");
        let second = install(&store, &bytes, &trust).expect("повтор");
        assert!(!first.already_installed);
        assert!(second.already_installed);
        assert_eq!(first.package.id, second.package.id);
        assert_eq!(first.package.version, second.package.version);
        assert_eq!(first.package.executable, second.package.executable);
        assert_eq!(first.package.package_sha256, second.package.package_sha256);
        assert_eq!(store.installed().expect("каталог").len(), 1);
        assert_eq!(
            fs::read_dir(temp.path().join(MODULE_ID))
                .expect("ID")
                .count(),
            1
        );
    }

    #[test]
    fn same_version_from_another_publisher_is_rejected() {
        let temp = tempfile::tempdir().expect("каталог");
        let first_signer = signer(4);
        let other_signer = signer(5);
        let trust = trust(&[
            ("org.utilmaker", &first_signer),
            ("org.example", &other_signer),
        ]);
        let first = manifest(PUBLISHER, "0.2.0", b"first");
        let other = manifest("org.example", "0.2.0", b"other");

        let store = PackageStore::open(temp.path()).expect("профиль");
        install(&store, &package(&first, b"first", &first_signer), &trust).expect("первый");
        let error = install(&store, &package(&other, b"other", &other_signer), &trust)
            .expect_err("смена издателя");
        assert!(matches!(error, InstallError::PublisherConflict { .. }));
        let installed = store.installed().expect("каталог");
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].publisher, PUBLISHER);
    }

    #[test]
    fn same_version_with_different_bytes_is_rejected() {
        let temp = tempfile::tempdir().expect("каталог");
        let signer = signer(6);
        let trust = trust(&[(PUBLISHER, &signer)]);
        let first = manifest(PUBLISHER, "0.2.0", b"first");
        let second = manifest(PUBLISHER, "0.2.0", b"second");

        let first_package = package(&first, b"first", &signer);
        let store = PackageStore::open(temp.path()).expect("профиль");
        install(&store, &first_package, &trust).expect("первый");
        let error = install(&store, &package(&second, b"second", &signer), &trust)
            .expect_err("подмена версии");
        assert!(matches!(error, InstallError::VersionConflict { .. }));
        let installed = store.installed().expect("каталог");
        assert_eq!(
            installed[0].package_sha256,
            hex::encode(Sha256::digest(&first_package))
        );
    }

    #[test]
    fn state_is_persisted_and_reopens() {
        let temp = tempfile::tempdir().expect("каталог");
        let signer = signer(7);
        let trust = trust(&[(PUBLISHER, &signer)]);
        let manifest = manifest(PUBLISHER, "0.2.0", b"module bytes");
        let bytes = package(&manifest, b"module bytes", &signer);

        let store = PackageStore::open(temp.path()).expect("профиль");
        install(&store, &bytes, &trust).expect("установка");
        assert!(store.set_enabled(MODULE_ID, false).expect("отключение"));
        assert!(!store.set_enabled(MODULE_ID, false).expect("повтор"));

        let reopened = PackageStore::open(temp.path()).expect("повторное открытие");
        let installed = reopened.installed().expect("каталог");
        assert_eq!(installed.len(), 1);
        assert!(!installed[0].enabled);
    }
}
