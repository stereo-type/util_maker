//! Автозапуск при входе пользователя в Windows.
//!
//! Используется ветка реестра текущего пользователя `Run`; административные
//! права не нужны.

use std::path::Path;

#[cfg(windows)]
mod imp {
    use super::*;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};
    use winreg::RegKey;

    const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    const VALUE: &str = "UtilMaker";

    pub fn is_enabled(_executable: &Path) -> Result<bool, String> {
        let current_user = RegKey::predef(HKEY_CURRENT_USER);
        let Ok(run) = current_user.open_subkey_with_flags(RUN_KEY, KEY_READ) else {
            return Ok(false);
        };
        let value: Result<String, _> = run.get_value(VALUE);
        Ok(value.map(|path| !path.trim().is_empty()).unwrap_or(false))
    }

    pub fn set_enabled(enabled: bool, executable: &Path) -> Result<(), String> {
        let current_user = RegKey::predef(HKEY_CURRENT_USER);
        if enabled {
            let (run, _) = current_user
                .create_subkey(RUN_KEY)
                .map_err(|error| format!("реестр автозапуска: {error}"))?;
            let command = format!("\"{}\"", executable.display());
            run.set_value(VALUE, &command)
                .map_err(|error| format!("запись автозапуска: {error}"))?;
        } else if let Ok(run) = current_user.open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE) {
            let _ = run.delete_value(VALUE);
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn is_enabled(_executable: &Path) -> Result<bool, String> {
        Err("автозапуск поддерживается только в Windows".to_owned())
    }

    pub fn set_enabled(_enabled: bool, _executable: &Path) -> Result<(), String> {
        Err("автозапуск поддерживается только в Windows".to_owned())
    }
}

pub use imp::{is_enabled, set_enabled};
