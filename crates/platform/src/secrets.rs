//! Секреты профилей: изоляция по модулю/профилю и защита средствами ОС.
//!
//! В Windows значение шифруется DPAPI текущего пользователя и только затем
//! попадает в файл профиля. На других ОС адаптер недоступен.

use std::fs;
use std::path::{Path, PathBuf};

/// Стабильный адрес секрета. Значение никогда не входит в DTO и журнал.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretKey {
    pub module_id: String,
    pub profile: String,
    pub name: String,
}

impl SecretKey {
    pub fn new(
        module_id: impl Into<String>,
        profile: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            module_id: module_id.into(),
            profile: profile.into(),
            name: name.into(),
        }
    }

    /// Безопасное имя файла: компоненты кодируются, разделители путей исключены.
    pub fn file_name(&self) -> String {
        format!(
            "{}__{}__{}.bin",
            encode(&self.module_id),
            encode(&self.profile),
            encode(&self.name)
        )
    }

    fn module_prefix(&self) -> String {
        format!("{}__", encode(&self.module_id))
    }
}

fn encode(value: &str) -> String {
    let mut output = String::with_capacity(value.len() * 2);
    for byte in value.as_bytes() {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecretError {
    #[error("секрет не найден")]
    NotFound,
    #[error("секрет недоступен: {0}")]
    Unavailable(String),
    #[error("ошибка хранилища: {0}")]
    Storage(String),
}

pub trait SecretStore {
    fn set(&mut self, key: &SecretKey, value: &str) -> Result<(), SecretError>;
    fn get(&self, key: &SecretKey) -> Result<String, SecretError>;
    fn remove(&mut self, key: &SecretKey) -> Result<(), SecretError>;
    fn remove_module(&mut self, module_id: &str) -> Result<(), SecretError>;
}

/// DPAPI-хранилище текущего пользователя.
pub struct DpapiSecretStore {
    root: PathBuf,
}

impl DpapiSecretStore {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, SecretError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root).map_err(|error| SecretError::Storage(error.to_string()))?;
        Ok(Self { root })
    }

    fn path(&self, key: &SecretKey) -> PathBuf {
        self.root.join(key.file_name())
    }
}

impl SecretStore for DpapiSecretStore {
    fn set(&mut self, key: &SecretKey, value: &str) -> Result<(), SecretError> {
        let encrypted = protect(value.as_bytes())?;
        let path = self.path(key);
        let temp = path.with_extension("bin.tmp");
        fs::write(&temp, encrypted).map_err(|error| SecretError::Storage(error.to_string()))?;
        fs::rename(temp, path).map_err(|error| SecretError::Storage(error.to_string()))
    }

    fn get(&self, key: &SecretKey) -> Result<String, SecretError> {
        let path = self.path(key);
        let bytes = fs::read(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                SecretError::NotFound
            } else {
                SecretError::Storage(error.to_string())
            }
        })?;
        let plain = unprotect(&bytes)?;
        String::from_utf8(plain).map_err(|error| SecretError::Storage(error.to_string()))
    }

    fn remove(&mut self, key: &SecretKey) -> Result<(), SecretError> {
        let path = self.path(key);
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(SecretError::Storage(error.to_string())),
        }
    }

    fn remove_module(&mut self, module_id: &str) -> Result<(), SecretError> {
        let prefix = SecretKey::new(module_id, "", "").module_prefix();
        for entry in
            fs::read_dir(&self.root).map_err(|error| SecretError::Storage(error.to_string()))?
        {
            let entry = entry.map_err(|error| SecretError::Storage(error.to_string()))?;
            let name = entry.file_name();
            if name.to_string_lossy().starts_with(&prefix) {
                let path = entry.path();
                fs::remove_file(path).map_err(|error| SecretError::Storage(error.to_string()))?;
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
fn protect(value: &[u8]) -> Result<Vec<u8>, SecretError> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};

    let input = CRYPT_INTEGER_BLOB {
        cbData: value.len() as u32,
        pbData: value.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(&input, None, None, None, None, 0, &mut output)
            .map_err(|error| SecretError::Unavailable(error.to_string()))?;
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData as *mut core::ffi::c_void)));
        Ok(bytes)
    }
}

#[cfg(windows)]
fn unprotect(value: &[u8]) -> Result<Vec<u8>, SecretError> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};

    let input = CRYPT_INTEGER_BLOB {
        cbData: value.len() as u32,
        pbData: value.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(&input, None, None, None, None, 0, &mut output)
            .map_err(|error| SecretError::Unavailable(error.to_string()))?;
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData as *mut core::ffi::c_void)));
        Ok(bytes)
    }
}

#[cfg(not(windows))]
fn protect(_value: &[u8]) -> Result<Vec<u8>, SecretError> {
    Err(SecretError::Unavailable(
        "DPAPI доступен только в Windows".to_owned(),
    ))
}

#[cfg(not(windows))]
fn unprotect(_value: &[u8]) -> Result<Vec<u8>, SecretError> {
    Err(SecretError::Unavailable(
        "DPAPI доступен только в Windows".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_isolated_and_safe() {
        let first = SecretKey::new("org.example", "yandex", "apiKey");
        let second = SecretKey::new("org.example", "google", "apiKey");
        let other = SecretKey::new("org.other", "yandex", "apiKey");
        assert_ne!(first.file_name(), second.file_name());
        assert_ne!(first.file_name(), other.file_name());
        assert!(!first.file_name().contains('/'));
        assert!(!first.file_name().contains('\\'));
    }

    #[test]
    fn module_removal_uses_encoded_prefix() {
        let key = SecretKey::new("org.example", "yandex", "apiKey");
        assert_eq!(key.module_prefix(), format!("{}__", encode("org.example")));
    }

    #[cfg(not(windows))]
    #[test]
    fn dpapi_is_unavailable_on_linux() {
        let mut store = DpapiSecretStore::open(std::env::temp_dir().join("um-secrets")).unwrap();
        let key = SecretKey::new("org.example", "yandex", "apiKey");
        assert!(matches!(
            store.set(&key, "secret"),
            Err(SecretError::Unavailable(_))
        ));
    }
}
