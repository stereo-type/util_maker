//! Командная строка разработчика для подписанных пакетов `.umpkg`.
//!
//! Команды:
//!
//! ```text
//! util-maker-package keygen --publisher org.utilmaker \
//!     --private-key dist/keys/org.utilmaker-private.json \
//!     --public-key dist/keys/org.utilmaker-public.json
//! util-maker-package export --source prepared --manifest manifest.json \
//!     --output plugin.umpkg --private-key dist/keys/org.utilmaker-private.json \
//!     [--version 0.5.0] [--platform linux-x64]
//! util-maker-package verify --package plugin.umpkg \
//!     --public-key dist/keys/org.utilmaker-public.json \
//!     --platform linux-x64 [--host-api 1.0.0]
//! util-maker-package install --package plugin.umpkg \
//!     --public-key dist/keys/org.utilmaker-public.json --profile PROFILE \
//!     --platform linux-x64 [--host-api 1.0.0]
//! util-maker-package list --profile PROFILE
//! util-maker-package rollback --profile PROFILE --id ID
//! util-maker-package activate --profile PROFILE --id ID --version VERSION
//! util-maker-package remove --profile PROFILE --id ID
//! ```
//!
//! Закрытый ключ не печатается. `keygen` создаёт новый файл и не перезаписывает
//! существующий, чтобы случайно не потерять ключ издателя.

use std::collections::BTreeMap;
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ed25519_dalek::SigningKey;
use rand_core::OsRng;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::json;
use util_maker_packages::{
    ensure_key_is_outside_package, export_package, verify_path, ExportOptions, PackageStore,
    TrustStore,
};
use util_maker_sdk::HOST_API_VERSION;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrivateKeyFile {
    publisher: String,
    private_key: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublicKeyFile {
    publisher: String,
    public_key: String,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ошибка: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "help".to_owned());
    let flags = Flags::parse(args.collect())?;
    match command.as_str() {
        "keygen" => keygen(&flags),
        "export" => export(&flags),
        "verify" => verify(&flags),
        "install" => install(&flags),
        "list" => list(&flags),
        "rollback" => rollback(&flags),
        "activate" => activate(&flags),
        "remove" => remove(&flags),
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        other => Err(format!("неизвестная команда: {other}").into()),
    }
}

fn keygen(flags: &Flags) -> Result<(), Box<dyn Error>> {
    let publisher = flags.required("publisher")?;
    let private_path = PathBuf::from(flags.required("private-key")?);
    let public_path = PathBuf::from(flags.required("public-key")?);
    if private_path == public_path {
        return Err("закрытый и открытый ключи должны быть в разных файлах".into());
    }
    let key = SigningKey::generate(&mut OsRng);
    let private = PrivateKeyFile {
        publisher: publisher.to_owned(),
        private_key: hex::encode(key.to_bytes()),
    };
    let public = PublicKeyFile {
        publisher: publisher.to_owned(),
        public_key: hex::encode(key.verifying_key().to_bytes()),
    };
    write_new_json(&private_path, &private, 0o600)?;
    write_new_json(&public_path, &public, 0o644)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "publisher": publisher,
            "privateKey": private_path,
            "publicKey": public_path,
        }))?
    );
    Ok(())
}

fn export(flags: &Flags) -> Result<(), Box<dyn Error>> {
    let private_path = PathBuf::from(flags.required("private-key")?);
    let (publisher, key) = load_private_key(&private_path)?;
    let source = PathBuf::from(flags.required("source")?);
    let manifest = PathBuf::from(flags.required("manifest")?);
    let output = PathBuf::from(flags.required("output")?);
    let version = flags.optional("version");
    let platform = flags.optional("platform");
    ensure_key_is_outside_package(&private_path, &source, &output)?;
    let result = export_package(&ExportOptions {
        source: &source,
        manifest: &manifest,
        output: &output,
        publisher: &publisher,
        signing_key: &key,
        version,
        platform,
    })?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "package": result.package,
            "sha256": result.sha256,
            "id": result.preview.id,
            "version": result.preview.version,
            "entrypoint": result.preview.entrypoint,
            "fileCount": result.file_count,
            "totalBytes": result.total_bytes,
        }))?
    );
    Ok(())
}

fn verify(flags: &Flags) -> Result<(), Box<dyn Error>> {
    let public_path = PathBuf::from(flags.required("public-key")?);
    let (publisher, key) = load_public_key(&public_path)?;
    let package = PathBuf::from(flags.required("package")?);
    let platform = flags.required("platform")?;
    let host_api = flags.optional("host-api").unwrap_or(HOST_API_VERSION);
    let host_api = Version::parse(host_api)?;
    let mut trust = TrustStore::new();
    trust.add_publisher(&publisher, key)?;
    let verified = verify_path(&package, &trust, platform, &host_api)?;
    println!("{}", serde_json::to_string_pretty(&verified.preview)?);
    Ok(())
}

fn install(flags: &Flags) -> Result<(), Box<dyn Error>> {
    let public_path = PathBuf::from(flags.required("public-key")?);
    let (publisher, key) = load_public_key(&public_path)?;
    let package = PathBuf::from(flags.required("package")?);
    let profile = PathBuf::from(flags.required("profile")?);
    let platform = flags.required("platform")?;
    let host_api = flags.optional("host-api").unwrap_or(HOST_API_VERSION);
    let host_api = Version::parse(host_api)?;
    let mut trust = TrustStore::new();
    trust.add_publisher(&publisher, key)?;
    let store = PackageStore::open(&profile)?;
    let result = store.install_path(&package, &trust, platform, &host_api)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "profile": profile,
            "id": result.package.id,
            "version": result.package.version,
            "publisher": result.package.publisher,
            "enabled": result.package.enabled,
            "executable": result.package.executable,
            "packageSha256": result.package.package_sha256,
            "alreadyInstalled": result.already_installed,
            "previousVersion": result.previous_version,
        }))?
    );
    Ok(())
}

fn list(flags: &Flags) -> Result<(), Box<dyn Error>> {
    let profile = PathBuf::from(flags.required("profile")?);
    let store = PackageStore::open(&profile)?;
    println!("{}", serde_json::to_string_pretty(&store.installed()?)?);
    Ok(())
}

fn rollback(flags: &Flags) -> Result<(), Box<dyn Error>> {
    let profile = PathBuf::from(flags.required("profile")?);
    let id = flags.required("id")?;
    let store = PackageStore::open(&profile)?;
    let package = store.rollback(id)?;
    println!("{}", serde_json::to_string_pretty(&package)?);
    Ok(())
}

fn activate(flags: &Flags) -> Result<(), Box<dyn Error>> {
    let profile = PathBuf::from(flags.required("profile")?);
    let id = flags.required("id")?;
    let version = flags.required("version")?;
    let store = PackageStore::open(&profile)?;
    let package = store.activate(id, version)?;
    println!("{}", serde_json::to_string_pretty(&package)?);
    Ok(())
}

fn remove(flags: &Flags) -> Result<(), Box<dyn Error>> {
    let profile = PathBuf::from(flags.required("profile")?);
    let id = flags.required("id")?;
    let store = PackageStore::open(&profile)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({"removed": store.remove(id)?}))?
    );
    Ok(())
}

fn load_private_key(path: &Path) -> Result<(String, SigningKey), Box<dyn Error>> {
    let text = fs::read_to_string(path)?;
    let key: PrivateKeyFile = serde_json::from_str(&text)?;
    let seed: [u8; 32] = hex::decode(&key.private_key)?
        .try_into()
        .map_err(|_| "закрытый ключ должен быть 32 байта в hex")?;
    Ok((key.publisher, SigningKey::from_bytes(&seed)))
}

fn load_public_key(path: &Path) -> Result<(String, [u8; 32]), Box<dyn Error>> {
    let text = fs::read_to_string(path)?;
    let key: PublicKeyFile = serde_json::from_str(&text)?;
    let bytes: [u8; 32] = hex::decode(&key.public_key)?
        .try_into()
        .map_err(|_| "открытый ключ должен быть 32 байта в hex")?;
    Ok((key.publisher, bytes))
}

fn write_new_json<T: Serialize>(path: &Path, value: &T, mode: u32) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    let mut file = options.open(path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(())
}

struct Flags(BTreeMap<String, String>);

impl Flags {
    fn parse(args: Vec<String>) -> Result<Self, Box<dyn Error>> {
        let mut flags = BTreeMap::new();
        let mut args = args.into_iter();
        while let Some(argument) = args.next() {
            let name = argument
                .strip_prefix("--")
                .ok_or_else(|| format!("ожидался флаг --имя, получено: {argument}"))?;
            let value = args
                .next()
                .ok_or_else(|| format!("у флага --{name} нет значения"))?;
            flags.insert(name.to_owned(), value);
        }
        Ok(Self(flags))
    }

    fn required(&self, name: &str) -> Result<&str, Box<dyn Error>> {
        self.0
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| format!("не задан обязательный флаг --{name}").into())
    }

    fn optional(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }
}

fn print_help() {
    println!("util-maker-package — ключи, экспорт и проверка .umpkg");
    println!();
    println!("  keygen --publisher P --private-key FILE --public-key FILE");
    println!(
        "  export --source DIR --manifest FILE --output FILE --private-key FILE [--version V] [--platform P]"
    );
    println!("  verify --package FILE --public-key FILE --platform P [--host-api V]");
    println!(
        "  install --package FILE --public-key FILE --profile DIR --platform P [--host-api V]"
    );
    println!("  list --profile DIR");
}
