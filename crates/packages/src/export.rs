//! Экспорт подготовленного каталога плагина в подписанный `.umpkg`.
//!
//! Экспортёр не запускает модуль и не распаковывает пакет. Он читает манифест,
//! собирает файлы полезной нагрузки, добавляет SHA-256, подписывает итоговые
//! байты манифеста Ed25519 и записывает ZIP атомарно: прежний выходной файл
//! остаётся на месте, если экспорт завершился ошибкой.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use semver::Version;
use sha2::{Digest, Sha256};
use util_maker_sdk::manifest::{Manifest, PackageFile};
use util_maker_sdk::HOST_API_VERSION;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

use crate::{
    verify_path, PackageError, PackagePreview, TrustStore, MANIFEST_ENTRY, SIGNATURE_ENTRY,
};

/// Параметры экспорта подготовленного каталога.
pub struct ExportOptions<'a> {
    /// Каталог только с файлами полезной нагрузки. `manifest.json` и
    /// `signature.ed25519` из него игнорируются и создаются заново.
    pub source: &'a Path,
    /// Базовый манифест. Допускается без поля `files`.
    pub manifest: &'a Path,
    /// Итоговый `.umpkg`.
    pub output: &'a Path,
    /// Издатель, которому принадлежит ключ подписи.
    pub publisher: &'a str,
    /// Ключ тестового или штатного издателя.
    pub signing_key: &'a SigningKey,
    /// Необязательная замена версии в копии манифеста.
    pub version: Option<&'a str>,
    /// Если задано, в пакет попадает только точка входа этой платформы.
    pub platform: Option<&'a str>,
}

/// Результат успешного экспорта.
#[derive(Debug, Clone)]
pub struct ExportResult {
    pub package: PathBuf,
    pub sha256: String,
    pub preview: PackagePreview,
    pub file_count: usize,
    pub total_bytes: u64,
}

/// Ошибка экспорта. Все проверки выполняются до замены готового файла.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("не удалось прочитать каталог пакета: {0}")]
    Io(#[from] std::io::Error),
    #[error("манифест пакета: {0}")]
    Manifest(#[from] util_maker_sdk::ManifestError),
    #[error("JSON манифеста: {0}")]
    Json(#[from] serde_json::Error),
    #[error("архив пакета: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("небезопасный путь в пакете: {0}")]
    UnsafePath(String),
    #[error("в каталоге пакета нерегулярный файл: {0}")]
    NotAFile(String),
    #[error("неизвестная платформа: {0}")]
    UnknownPlatform(String),
    #[error("нет точки входа {platform}: {path}")]
    MissingEntrypoint { platform: String, path: String },
    #[error("нет файла {path}, указанного в манифесте")]
    MissingFile { path: String },
    #[error("выходной файл не должен находиться внутри каталога пакета")]
    OutputInsideSource,
    #[error("закрытый ключ не должен находиться внутри каталога пакета: {0}")]
    PrivateKeyInsideSource(String),
    #[error("выходной файл не должен совпадать с закрытым ключом")]
    OutputIsPrivateKey,
    #[error("ключ не принадлежит издателю {0}")]
    PublisherMismatch(String),
    #[error("созданный пакет не прошёл проверку: {0}")]
    Verification(#[from] PackageError),
    #[error("некорректная метка времени ZIP: {0}")]
    Timestamp(String),
}

#[derive(Debug)]
struct PayloadFile {
    path: String,
    source: PathBuf,
    mode: u32,
}

impl PayloadFile {
    fn package_file(&self) -> Result<PackageFile, ExportError> {
        Ok(PackageFile {
            path: self.path.clone(),
            sha256: hash_file(&self.source)?,
        })
    }
}

fn hash_file(path: &Path) -> Result<String, ExportError> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Проверяет, что закрытый ключ не попадёт в полезную нагрузку и что выходной
/// файл не заменит сам ключ. Каталоги сравниваются после `canonicalize`, чтобы
/// символические ссылки не обходили проверку.
pub fn ensure_key_is_outside_package(
    private_key: &Path,
    source: &Path,
    output: &Path,
) -> Result<(), ExportError> {
    let source = source.canonicalize()?;
    let key = private_key.canonicalize()?;
    if key.starts_with(&source) {
        return Err(ExportError::PrivateKeyInsideSource(
            private_key.display().to_string(),
        ));
    }
    if output.canonicalize().is_ok_and(|output| output == key) {
        return Err(ExportError::OutputIsPrivateKey);
    }
    Ok(())
}

/// Экспортирует каталог и проверяет результат тем же кодом, что и установка.
pub fn export_package(options: &ExportOptions<'_>) -> Result<ExportResult, ExportError> {
    let mut manifest = Manifest::load(options.manifest)?;
    if manifest.publisher != options.publisher {
        return Err(ExportError::PublisherMismatch(manifest.publisher));
    }
    if let Some(version) = options.version {
        manifest.version = version.to_owned();
    }
    if let Some(platform) = options.platform {
        if !manifest.entrypoints.contains_key(platform) {
            return Err(ExportError::UnknownPlatform(platform.to_owned()));
        }
        manifest.entrypoints.retain(|name, _| name == platform);
    }
    manifest.validate()?;

    let source = options.source.canonicalize()?;
    if !source.is_dir() {
        return Err(ExportError::NotAFile(source.display().to_string()));
    }

    let output = prepare_output(options.output, &source)?;
    let files = collect_payload(&source)?;
    if files.is_empty() {
        return Err(ExportError::MissingFile {
            path: "файлы полезной нагрузки".to_owned(),
        });
    }
    let listed: BTreeSet<&str> = files.iter().map(|file| file.path.as_str()).collect();
    for (platform, entrypoint) in &manifest.entrypoints {
        if !listed.contains(entrypoint.as_str()) {
            return Err(ExportError::MissingEntrypoint {
                platform: platform.clone(),
                path: entrypoint.clone(),
            });
        }
    }
    if let Some(icon) = &manifest.icon {
        if !listed.contains(icon.as_str()) {
            return Err(ExportError::MissingFile { path: icon.clone() });
        }
    }
    manifest.files = files
        .iter()
        .map(PayloadFile::package_file)
        .collect::<Result<Vec<_>, _>>()?;
    manifest.validate()?;

    let manifest_bytes = manifest_json(&manifest)?;
    let signature = options.signing_key.sign(&manifest_bytes);
    let temporary = write_archive(&output, &files, &manifest_bytes, &signature.to_bytes())?;

    let mut trust = TrustStore::new();
    trust.add_publisher(
        options.publisher,
        verifying_key_bytes(options.signing_key.verifying_key()),
    )?;
    let platform = options
        .platform
        .map(str::to_owned)
        .or_else(|| manifest.entrypoints.keys().next().cloned())
        .ok_or_else(|| ExportError::UnknownPlatform("точка входа не задана".to_owned()))?;
    let host_api = Version::parse(HOST_API_VERSION)
        .map_err(|error| ExportError::Verification(PackageError::Malformed(error.to_string())))?;
    let verified = match verify_path(&temporary, &trust, &platform, &host_api) {
        Ok(verified) => verified,
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            return Err(error.into());
        }
    };
    if verified.manifest.version != manifest.version {
        let _ = fs::remove_file(&temporary);
        return Err(ExportError::Verification(PackageError::Malformed(
            "версия проверенного пакета не совпала с манифестом".to_owned(),
        )));
    }
    if let Err(error) = fs::rename(&temporary, &output) {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    let mut package = File::open(&output)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut package, &mut hasher)?;

    Ok(ExportResult {
        package: output,
        sha256: hex::encode(hasher.finalize()),
        file_count: verified.preview.file_count,
        total_bytes: verified.preview.total_bytes,
        preview: verified.preview,
    })
}

fn verifying_key_bytes(key: VerifyingKey) -> [u8; 32] {
    key.to_bytes()
}

fn manifest_json(manifest: &Manifest) -> Result<Vec<u8>, ExportError> {
    let mut bytes = serde_json::to_vec_pretty(manifest)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn prepare_output(output: &Path, source: &Path) -> Result<PathBuf, ExportError> {
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let canonical_parent = parent.canonicalize()?;
    let file_name = output
        .file_name()
        .ok_or_else(|| ExportError::UnsafePath(output.display().to_string()))?;
    let prepared = canonical_parent.join(file_name);
    if prepared.starts_with(source) {
        return Err(ExportError::OutputInsideSource);
    }
    Ok(prepared)
}

fn collect_payload(root: &Path) -> Result<Vec<PayloadFile>, ExportError> {
    let mut files = Vec::new();
    visit(root, root, &mut files)?;
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

fn visit(root: &Path, directory: &Path, files: &mut Vec<PayloadFile>) -> Result<(), ExportError> {
    let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(ExportError::UnsafePath(relative_name(root, &path)?));
        }
        if metadata.is_dir() {
            visit(root, &path, files)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(ExportError::NotAFile(relative_name(root, &path)?));
        }
        let relative = relative_name(root, &path)?;
        if relative == MANIFEST_ENTRY || relative == SIGNATURE_ENTRY {
            continue;
        }
        files.push(PayloadFile {
            path: relative,
            source: path,
            mode: file_mode(&metadata),
        });
    }
    Ok(())
}

fn relative_name(root: &Path, path: &Path) -> Result<String, ExportError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| ExportError::UnsafePath(path.display().to_string()))?;
    let mut parts = Vec::new();
    for component in relative.components() {
        let name = component
            .as_os_str()
            .to_str()
            .ok_or_else(|| ExportError::UnsafePath(relative.display().to_string()))?;
        if name.is_empty() || name == "." || name == ".." || name.contains('\\') {
            return Err(ExportError::UnsafePath(relative.display().to_string()));
        }
        parts.push(name);
    }
    if parts.is_empty() {
        return Err(ExportError::UnsafePath(relative.display().to_string()));
    }
    Ok(parts.join("/"))
}

#[cfg(unix)]
fn file_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o111 != 0 {
        0o755
    } else {
        0o644
    }
}

#[cfg(not(unix))]
fn file_mode(_metadata: &fs::Metadata) -> u32 {
    0o644
}

fn write_archive(
    output: &Path,
    files: &[PayloadFile],
    manifest: &[u8],
    signature: &[u8],
) -> Result<PathBuf, ExportError> {
    let temporary = output.with_extension(format!("tmp-{}", std::process::id()));
    if let Err(error) = write_archive_at(&temporary, files, manifest, signature) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(temporary)
}

fn write_archive_at(
    path: &Path,
    files: &[PayloadFile],
    manifest: &[u8],
    signature: &[u8],
) -> Result<(), ExportError> {
    let file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let timestamp = DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0)
        .map_err(|error| ExportError::Timestamp(error.to_string()))?;
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .last_modified_time(timestamp);
    let mut writer = ZipWriter::new(file);
    writer.start_file(MANIFEST_ENTRY, options.unix_permissions(0o644))?;
    writer.write_all(manifest)?;
    writer.start_file(SIGNATURE_ENTRY, options.unix_permissions(0o644))?;
    writer.write_all(signature)?;
    for payload in files {
        writer.start_file(&payload.path, options.unix_permissions(payload.mode))?;
        let mut source = File::open(&payload.source)?;
        std::io::copy(&mut source, &mut writer)?;
    }
    writer.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::Cursor;
    use tempfile::TempDir;
    use util_maker_sdk::{Action, MANIFEST_VERSION};

    const PUBLISHER: &str = "org.utilmaker";
    const MODULE_ID: &str = "org.utilmaker.control";

    fn signing_key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn manifest() -> Manifest {
        Manifest {
            manifest_version: MANIFEST_VERSION,
            id: MODULE_ID.to_owned(),
            publisher: PUBLISHER.to_owned(),
            name: "Контрольный пакет".to_owned(),
            version: "0.4.1".to_owned(),
            host_api: ">=1.0.0 <2.0.0".to_owned(),
            entrypoints: BTreeMap::from([
                (
                    "windows-x64".to_owned(),
                    "bin/control-module.exe".to_owned(),
                ),
                ("linux-x64".to_owned(), "bin/control-module".to_owned()),
            ]),
            icon: Some("assets/icon.png".to_owned()),
            capabilities: Vec::new(),
            actions: vec![Action {
                id: "ping".to_owned(),
                name: "Проверить связь".to_owned(),
                description: None,
                recommended_hotkeys: Vec::new(),
            }],
            settings: None,
            files: Vec::new(),
        }
    }

    fn workspace() -> TempDir {
        let temp = tempfile::tempdir().expect("временный каталог");
        let source = temp.path().join("source");
        fs::create_dir_all(source.join("bin")).expect("каталог bin");
        fs::create_dir_all(source.join("assets")).expect("каталог assets");
        fs::write(source.join("bin/control-module"), b"linux payload").expect("модуль");
        fs::write(source.join("assets/icon.png"), b"icon").expect("иконка");
        fs::write(
            source.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest()).expect("манифест"),
        )
        .expect("запись манифеста");
        temp
    }

    fn options<'a>(
        source: &'a Path,
        manifest: &'a Path,
        output: &'a Path,
        key: &'a SigningKey,
    ) -> ExportOptions<'a> {
        ExportOptions {
            source,
            manifest,
            output,
            publisher: PUBLISHER,
            signing_key: key,
            version: Some("0.5.0"),
            platform: Some("linux-x64"),
        }
    }

    #[test]
    fn exports_single_platform_with_new_version_and_hashes() {
        let temp = workspace();
        let source = temp.path().join("source");
        let output = temp.path().join("control.umpkg");
        let key = signing_key(7);
        let result = export_package(&options(
            &source,
            &source.join("manifest.json"),
            &output,
            &key,
        ))
        .expect("экспорт");
        assert_eq!(result.preview.id, MODULE_ID);
        assert_eq!(result.preview.version, "0.5.0");
        assert_eq!(result.preview.entrypoint, "bin/control-module");
        assert_eq!(result.file_count, 2);

        let mut trust = TrustStore::new();
        trust
            .add_publisher(PUBLISHER, key.verifying_key().to_bytes())
            .expect("ключ");
        let verified =
            verify_path(&output, &trust, "linux-x64", &Version::new(1, 0, 0)).expect("проверка");
        assert_eq!(verified.preview.version, "0.5.0");
        assert_eq!(verified.manifest.entrypoints.len(), 1);
        assert!(verified.manifest.entrypoints.contains_key("linux-x64"));
    }

    #[test]
    fn missing_entrypoint_keeps_previous_output() {
        let temp = workspace();
        let source = temp.path().join("source");
        let output = temp.path().join("control.umpkg");
        let key = signing_key(7);
        let manifest = source.join("manifest.json");
        export_package(&options(&source, &manifest, &output, &key)).expect("первый экспорт");
        let previous = fs::read(&output).expect("прежний пакет");

        fs::remove_file(source.join("bin/control-module")).expect("удаление модуля");
        let error = export_package(&options(&source, &manifest, &output, &key))
            .expect_err("экспорт без модуля");
        assert!(matches!(error, ExportError::MissingEntrypoint { .. }));
        assert_eq!(fs::read(&output).expect("пакет сохранился"), previous);
        let mut trust = TrustStore::new();
        trust
            .add_publisher(PUBLISHER, key.verifying_key().to_bytes())
            .expect("ключ");
        verify_path(&output, &trust, "linux-x64", &Version::new(1, 0, 0))
            .expect("прежний пакет остался пригодным");
    }

    #[test]
    fn private_key_inside_source_is_rejected() {
        let temp = workspace();
        let source = temp.path().join("source");
        let key_path = source.join("private-key.json");
        fs::write(&key_path, b"secret").expect("ключ");
        let error =
            ensure_key_is_outside_package(&key_path, &source, &temp.path().join("control.umpkg"))
                .expect_err("ключ внутри каталога");
        assert!(matches!(error, ExportError::PrivateKeyInsideSource(_)));
    }

    #[test]
    fn output_matching_private_key_is_rejected() {
        let temp = workspace();
        let source = temp.path().join("source");
        let key_path = temp.path().join("private-key.json");
        fs::write(&key_path, b"secret").expect("ключ");
        let error = ensure_key_is_outside_package(&key_path, &source, &key_path)
            .expect_err("выход совпал с ключом");
        assert!(matches!(error, ExportError::OutputIsPrivateKey));
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_link_is_rejected() {
        use std::os::unix::fs::symlink;

        let temp = workspace();
        let source = temp.path().join("source");
        let outside = temp.path().join("outside.txt");
        fs::write(&outside, b"secret").expect("внешний файл");
        symlink(&outside, source.join("bin/link")).expect("ссылка");
        let output = temp.path().join("control.umpkg");
        let key = signing_key(7);
        let error = export_package(&options(
            &source,
            &source.join("manifest.json"),
            &output,
            &key,
        ))
        .expect_err("символическая ссылка");
        assert!(matches!(error, ExportError::UnsafePath(_)));
        assert!(!output.exists());
    }

    #[test]
    fn output_inside_source_is_rejected() {
        let temp = workspace();
        let source = temp.path().join("source");
        let output = source.join("control.umpkg");
        let key = signing_key(7);
        let error = export_package(&options(
            &source,
            &source.join("manifest.json"),
            &output,
            &key,
        ))
        .expect_err("выход внутри каталога");
        assert!(matches!(error, ExportError::OutputInsideSource));
    }

    #[test]
    fn archive_contains_manifest_signature_and_sorted_payload() {
        let temp = workspace();
        let source = temp.path().join("source");
        let output = temp.path().join("control.umpkg");
        let key = signing_key(7);
        export_package(&options(
            &source,
            &source.join("manifest.json"),
            &output,
            &key,
        ))
        .expect("экспорт");
        let bytes = fs::read(&output).expect("пакет");
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).expect("zip");
        let names = (0..archive.len())
            .map(|index| archive.by_index(index).expect("файл").name().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "manifest.json",
                "signature.ed25519",
                "assets/icon.png",
                "bin/control-module"
            ]
        );
    }
}
