//! Проверка локального пакета `.umpkg` до установки.
//!
//! Пакет — ZIP-архив с `manifest.json`, отсоединённой подписью
//! `signature.ed25519` и файлами полезной нагрузки, перечисленными в манифесте
//! с SHA-256. Проверка ничего не распаковывает и не запускает: модуль
//! активируется только после успешной проверки.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use semver::Version;
use sha2::{Digest, Sha256};
use util_maker_sdk::manifest::Manifest;
use util_maker_sdk::ManifestError;
use zip::ZipArchive;

/// Имя манифеста внутри архива.
pub const MANIFEST_ENTRY: &str = "manifest.json";
/// Имя отсоединённой подписи Ed25519.
pub const SIGNATURE_ENTRY: &str = "signature.ed25519";

/// Ошибка проверки пакета. Все варианты достижимы до активации версии.
#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    #[error("не удалось прочитать пакет: {0}")]
    Io(#[from] std::io::Error),
    #[error("архив пакета повреждён: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("манифест пакета: {0}")]
    Manifest(#[from] ManifestError),
    #[error("в пакете нет обязательного файла {0}")]
    MissingEntry(&'static str),
    #[error("издатель {0} не входит в список доверенных")]
    UnknownPublisher(String),
    #[error("подпись манифеста неверна")]
    InvalidSignature,
    #[error("небезопасный путь в пакете: {0}")]
    UnsafePath(String),
    #[error("файл {0} отсутствует в архиве")]
    MissingFile(String),
    #[error("SHA-256 файла {0} не совпадает")]
    HashMismatch(String),
    #[error("в пакете есть файл, не указанный в манифесте: {0}")]
    ExtraFile(String),
    #[error("нет точки входа для платформы {0}")]
    UnsupportedPlatform(String),
    #[error("несовместимая версия API: {0}")]
    IncompatibleApi(String),
    #[error("нарушение формата пакета: {0}")]
    Malformed(String),
}

/// Доверенные открытые ключи издателей.
pub struct TrustStore {
    keys: BTreeMap<String, VerifyingKey>,
}

impl TrustStore {
    pub fn new() -> Self {
        Self {
            keys: BTreeMap::new(),
        }
    }

    /// Добавляет открытый ключ издателя по его стабильному ID.
    pub fn add_publisher(&mut self, publisher: &str, key: [u8; 32]) -> Result<(), PackageError> {
        let key = VerifyingKey::from_bytes(&key)
            .map_err(|error| PackageError::Malformed(format!("ключ издателя: {error}")))?;
        self.keys.insert(publisher.to_owned(), key);
        Ok(())
    }

    pub fn contains(&self, publisher: &str) -> bool {
        self.keys.contains_key(publisher)
    }
}

impl Default for TrustStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Сведения для предварительного просмотра до установки.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackagePreview {
    pub id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
    pub capabilities: Vec<String>,
    pub actions: Vec<PreviewAction>,
    pub entrypoint: String,
    pub file_count: usize,
    pub total_bytes: u64,
}

/// Действие в предварительном просмотре.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewAction {
    pub id: String,
    pub name: String,
}

/// Проверенный пакет. Файлы не распакованы и модуль не запускался.
#[derive(Debug, Clone)]
pub struct VerifiedPackage {
    pub preview: PackagePreview,
    pub manifest: Manifest,
}

/// Проверяет пакет по пути.
pub fn verify_path(
    path: &Path,
    trust: &TrustStore,
    platform: &str,
    host_api: &Version,
) -> Result<VerifiedPackage, PackageError> {
    let file = File::open(path)?;
    verify_reader(file, trust, platform, host_api)
}

/// Проверяет пакет из произвольного потока (используется тестами и UI).
pub fn verify_reader<R: Read + Seek>(
    reader: R,
    trust: &TrustStore,
    platform: &str,
    host_api: &Version,
) -> Result<VerifiedPackage, PackageError> {
    let mut archive = ZipArchive::new(reader)?;
    let manifest_bytes = read_entry(&mut archive, MANIFEST_ENTRY)?
        .ok_or(PackageError::MissingEntry(MANIFEST_ENTRY))?;
    let signature_bytes = read_entry(&mut archive, SIGNATURE_ENTRY)?
        .ok_or(PackageError::MissingEntry(SIGNATURE_ENTRY))?;

    let manifest = Manifest::from_json(
        std::str::from_utf8(&manifest_bytes)
            .map_err(|error| PackageError::Malformed(format!("манифест не UTF-8: {error}")))?,
    )?;
    manifest.validate()?;
    validate_manifest_paths(&manifest)?;

    let key = trust
        .keys
        .get(&manifest.publisher)
        .ok_or_else(|| PackageError::UnknownPublisher(manifest.publisher.clone()))?;
    verify_signature(key, &manifest_bytes, &signature_bytes)?;

    let entrypoint = manifest
        .resolved_entrypoint(platform)
        .map_err(|error| PackageError::UnsupportedPlatform(error.to_string()))?;
    let entrypoint = entrypoint.to_string_lossy().into_owned();
    if !is_safe_relative(&entrypoint) {
        return Err(PackageError::UnsafePath(entrypoint));
    }
    if !manifest.files.iter().any(|file| file.path == entrypoint) {
        return Err(PackageError::Malformed(
            "точка входа отсутствует в списке файлов пакета".to_owned(),
        ));
    }
    if !manifest.supports_host_api(host_api)? {
        return Err(PackageError::IncompatibleApi(format!(
            "пакет требует hostApi {}, хост {}",
            manifest.host_api, host_api
        )));
    }

    validate_archive_entries(&mut archive, &manifest)?;
    let mut total_bytes = 0u64;
    for file in &manifest.files {
        let (actual, size) = hash_entry(&mut archive, &file.path)?;
        if !actual.eq_ignore_ascii_case(&file.sha256) {
            return Err(PackageError::HashMismatch(file.path.clone()));
        }
        total_bytes += size;
    }

    let preview = PackagePreview {
        id: manifest.id.clone(),
        name: manifest.name.clone(),
        publisher: manifest.publisher.clone(),
        version: manifest.version.clone(),
        capabilities: manifest.capabilities.clone(),
        actions: manifest
            .actions
            .iter()
            .map(|action| PreviewAction {
                id: action.id.clone(),
                name: action.name.clone(),
            })
            .collect(),
        entrypoint,
        file_count: manifest.files.len(),
        total_bytes,
    };
    Ok(VerifiedPackage { preview, manifest })
}

fn validate_manifest_paths(manifest: &Manifest) -> Result<(), PackageError> {
    let mut seen = BTreeSet::new();
    for file in &manifest.files {
        if !is_safe_relative(&file.path) {
            return Err(PackageError::UnsafePath(file.path.clone()));
        }
        if file.path == MANIFEST_ENTRY || file.path == SIGNATURE_ENTRY {
            return Err(PackageError::UnsafePath(file.path.clone()));
        }
        if !seen.insert(file.path.as_str()) {
            return Err(PackageError::Malformed(format!(
                "файл {} указан дважды",
                file.path
            )));
        }
        if file.sha256.len() != 64 || !file.sha256.chars().all(|value| value.is_ascii_hexdigit()) {
            return Err(PackageError::Malformed(format!(
                "некорректный SHA-256 для {}",
                file.path
            )));
        }
    }
    if manifest.files.is_empty() {
        return Err(PackageError::Malformed(
            "в манифесте нет файлов полезной нагрузки".to_owned(),
        ));
    }
    Ok(())
}

fn validate_archive_entries<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    manifest: &Manifest,
) -> Result<(), PackageError> {
    let listed: BTreeSet<&str> = manifest
        .files
        .iter()
        .map(|file| file.path.as_str())
        .chain([MANIFEST_ENTRY, SIGNATURE_ENTRY])
        .collect();
    let mut seen = BTreeSet::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let name = entry.name().to_owned();
        if entry.is_dir() {
            continue;
        }
        if !is_safe_relative(&name) {
            return Err(PackageError::UnsafePath(name));
        }
        if is_symlink(&entry) {
            return Err(PackageError::UnsafePath(format!(
                "символическая ссылка {name}"
            )));
        }
        if !seen.insert(name.clone()) {
            return Err(PackageError::Malformed(format!(
                "файл {name} встречается дважды"
            )));
        }
        if !listed.contains(name.as_str()) {
            return Err(PackageError::ExtraFile(name));
        }
    }
    Ok(())
}

fn is_symlink<R: Read>(entry: &zip::read::ZipFile<'_, R>) -> bool {
    entry
        .unix_mode()
        .map(|mode| mode & 0o170000 == 0o120000)
        .unwrap_or(false)
}

fn is_safe_relative(value: &str) -> bool {
    if value.is_empty()
        || value.starts_with('/')
        || value.starts_with('\\')
        || value.contains('\\')
        || value.contains(':')
    {
        return false;
    }
    value
        .split('/')
        .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn verify_signature(
    key: &VerifyingKey,
    manifest_bytes: &[u8],
    signature_bytes: &[u8],
) -> Result<(), PackageError> {
    let signature_bytes: [u8; 64] = signature_bytes
        .try_into()
        .map_err(|_| PackageError::InvalidSignature)?;
    let signature = Signature::from_bytes(&signature_bytes);
    key.verify(manifest_bytes, &signature)
        .map_err(|_| PackageError::InvalidSignature)
}

fn read_entry<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
) -> Result<Option<Vec<u8>>, PackageError> {
    let mut entry = match archive.by_name(name) {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(error) => return Err(PackageError::Zip(error)),
    };
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

fn hash_entry<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
) -> Result<(String, u64), PackageError> {
    let mut entry = archive.by_name(name).map_err(|error| match error {
        zip::result::ZipError::FileNotFound => PackageError::MissingFile(name.to_owned()),
        other => PackageError::Zip(other),
    })?;
    if entry.is_dir() || is_symlink(&entry) {
        return Err(PackageError::UnsafePath(name.to_owned()));
    }
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 8192];
    loop {
        let read = entry.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total += read as u64;
    }
    Ok((hex::encode(hasher.finalize()), total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use std::collections::BTreeMap;
    use std::io::{Cursor, Write};
    use util_maker_sdk::manifest::PackageFile;
    use util_maker_sdk::Action;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    const PUBLISHER: &str = "org.utilmaker";
    const MODULE_ID: &str = "org.utilmaker.control";

    fn hash(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    fn signing_key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn trust_for(signer: &SigningKey) -> TrustStore {
        let mut trust = TrustStore::new();
        trust
            .add_publisher(PUBLISHER, signer.verifying_key().to_bytes())
            .expect("ключ");
        trust
    }

    fn manifest(host_api: &str, files: Vec<PackageFile>) -> Manifest {
        Manifest {
            manifest_version: 1,
            id: MODULE_ID.to_owned(),
            publisher: PUBLISHER.to_owned(),
            name: "Контрольный пакет".to_owned(),
            version: "0.2.0".to_owned(),
            host_api: host_api.to_owned(),
            entrypoints: BTreeMap::from([("windows-x64".to_owned(), "bin/module.exe".to_owned())]),
            icon: None,
            capabilities: vec!["screen.capture".to_owned()],
            actions: vec![Action {
                id: "ping".to_owned(),
                name: "Проверить".to_owned(),
                description: None,
                recommended_hotkeys: Vec::new(),
            }],
            files,
        }
    }

    fn package(
        manifest: &Manifest,
        payload: &[u8],
        signer: &SigningKey,
        extra: &[(&str, &[u8])],
    ) -> Vec<u8> {
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
        writer.start_file("bin/module.exe", options).expect("файл");
        writer.write_all(payload).expect("запись");
        for (path, bytes) in extra {
            writer.start_file(*path, options).expect("лишний файл");
            writer.write_all(bytes).expect("запись");
        }
        writer.finish().expect("архив").into_inner()
    }

    fn valid_file(payload: &[u8]) -> PackageFile {
        PackageFile {
            path: "bin/module.exe".to_owned(),
            sha256: hash(payload),
        }
    }

    fn host() -> Version {
        Version::new(1, 0, 0)
    }

    #[test]
    fn valid_package_returns_preview_without_launch() {
        let signer = signing_key(7);
        let payload = b"module payload";
        let manifest = manifest(">=1.0.0 <2.0.0", vec![valid_file(payload)]);
        let archive = package(&manifest, payload, &signer, &[]);
        let verified = verify_reader(
            Cursor::new(archive),
            &trust_for(&signer),
            "windows-x64",
            &host(),
        )
        .expect("пакет должен пройти проверку");
        assert_eq!(verified.preview.id, MODULE_ID);
        assert_eq!(verified.preview.name, "Контрольный пакет");
        assert_eq!(verified.preview.version, "0.2.0");
        assert_eq!(verified.preview.file_count, 1);
        assert_eq!(verified.preview.actions[0].id, "ping");
        assert_eq!(verified.preview.total_bytes, payload.len() as u64);
    }

    #[test]
    fn tampered_payload_is_rejected() {
        let signer = signing_key(7);
        let manifest = manifest(">=1.0.0 <2.0.0", vec![valid_file(b"good")]);
        let archive = package(&manifest, b"tampered", &signer, &[]);
        let error = verify_reader(
            Cursor::new(archive),
            &trust_for(&signer),
            "windows-x64",
            &host(),
        )
        .expect_err("изменённая нагрузка");
        assert!(matches!(error, PackageError::HashMismatch(_)));
    }

    #[test]
    fn foreign_signature_is_rejected() {
        let signer = signing_key(7);
        let other = signing_key(9);
        let manifest = manifest(">=1.0.0 <2.0.0", vec![valid_file(b"payload")]);
        let archive = package(&manifest, b"payload", &other, &[]);
        let error = verify_reader(
            Cursor::new(archive),
            &trust_for(&signer),
            "windows-x64",
            &host(),
        )
        .expect_err("чужая подпись");
        assert!(matches!(error, PackageError::InvalidSignature));
    }

    #[test]
    fn incompatible_api_is_rejected() {
        let signer = signing_key(7);
        let payload = b"payload";
        let manifest = manifest(">=2.0.0 <3.0.0", vec![valid_file(payload)]);
        let archive = package(&manifest, payload, &signer, &[]);
        let error = verify_reader(
            Cursor::new(archive),
            &trust_for(&signer),
            "windows-x64",
            &host(),
        )
        .expect_err("несовместимый API");
        assert!(matches!(error, PackageError::IncompatibleApi(_)));
    }

    #[test]
    fn absolute_and_parent_paths_are_rejected() {
        let signer = signing_key(7);
        for path in ["/etc/passwd", "../module.exe", "bin/../../module.exe"] {
            let manifest = manifest(
                ">=1.0.0 <2.0.0",
                vec![PackageFile {
                    path: path.to_owned(),
                    sha256: hash(b"payload"),
                }],
            );
            let archive = package(&manifest, b"payload", &signer, &[]);
            let error = verify_reader(
                Cursor::new(archive),
                &trust_for(&signer),
                "windows-x64",
                &host(),
            )
            .expect_err("небезопасный путь");
            assert!(
                matches!(error, PackageError::UnsafePath(_)),
                "{path}: {error}"
            );
        }
    }

    #[test]
    fn unknown_publisher_and_extra_file_are_rejected() {
        let signer = signing_key(7);
        let payload = b"payload";
        let manifest = manifest(">=1.0.0 <2.0.0", vec![valid_file(payload)]);
        let archive = package(&manifest, payload, &signer, &[("evil.txt", b"x")]);
        let error = verify_reader(
            Cursor::new(archive),
            &trust_for(&signer),
            "windows-x64",
            &host(),
        )
        .expect_err("лишний файл");
        assert!(matches!(error, PackageError::ExtraFile(_)));

        let archive = package(&manifest, payload, &signer, &[]);
        let error = verify_reader(
            Cursor::new(archive),
            &TrustStore::new(),
            "windows-x64",
            &host(),
        )
        .expect_err("неизвестный издатель");
        assert!(matches!(error, PackageError::UnknownPublisher(_)));
    }

    #[test]
    fn failed_verification_keeps_active_preview() {
        let signer = signing_key(7);
        let payload = b"payload";
        let manifest = manifest(">=1.0.0 <2.0.0", vec![valid_file(payload)]);
        let active = verify_reader(
            Cursor::new(package(&manifest, payload, &signer, &[])),
            &trust_for(&signer),
            "windows-x64",
            &host(),
        )
        .expect("активная версия")
        .preview;

        let broken = package(&manifest, b"broken", &signer, &[]);
        assert!(verify_reader(
            Cursor::new(broken),
            &trust_for(&signer),
            "windows-x64",
            &host()
        )
        .is_err());
        assert_eq!(active.id, MODULE_ID);
        assert_eq!(active.version, "0.2.0");
    }
}
