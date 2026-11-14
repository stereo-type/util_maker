//! Журнал прототипа: `<каталог установки>\logs\util-maker.log`.
//!
//! Основной путь — рядом с исполняемым файлом; если каталог установки недоступен
//! для записи, используется `%LOCALAPPDATA%\Util Maker\logs`. Формат строки —
//! ISO-время UTC, тип сборки и сообщение. Исходный текст, переводы, снимки и
//! ключи в журнал не попадают.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use util_maker_core::Logger;

const MAX_LOG_BYTES: u64 = 1024 * 1024;

/// Тип сборки, заданный через `UTIL_MAKER_BUILD` при сборке.
pub const BUILD_KIND: &str = env!("UTIL_MAKER_BUILD");

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
static LOG_LOCK: Mutex<()> = Mutex::new(());

/// Выбирает каталог журнала: рядом с приложением, иначе в данных пользователя.
pub fn init() {
    let directory = log_directory();
    if fs::create_dir_all(&directory).is_err() {
        eprintln!("не удалось создать каталог журнала {}", directory.display());
        return;
    }
    let path = directory.join("util-maker.log");
    let _ = LOG_PATH.set(path.clone());
    let executable = std::env::current_exe()
        .map(|value| value.display().to_string())
        .unwrap_or_else(|_| "неизвестно".to_owned());
    write(&format!(
        "старт build={BUILD_KIND} exe={executable} log={}",
        path.display()
    ));
}

fn log_directory() -> PathBuf {
    if let Some(directory) = std::env::var_os("UTIL_MAKER_LOG_DIR") {
        return PathBuf::from(directory);
    }
    if let Ok(executable) = std::env::current_exe() {
        if let Some(parent) = executable.parent() {
            let candidate = parent.join("logs");
            if fs::create_dir_all(&candidate).is_ok() {
                return candidate;
            }
        }
    }
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_STATE_HOME")
                .map(PathBuf::from)
                .map(|path| path.join("util-maker"))
        })
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("Util Maker").join("logs")
}

pub fn build_kind() -> &'static str {
    BUILD_KIND
}

/// Подробная запись для dev-сборки.
pub fn debug(message: &str) {
    if BUILD_KIND == "dev" {
        write(&format!("DEBUG {message}"));
    }
}

pub fn log_path() -> String {
    LOG_PATH
        .get()
        .map(|path| path.display().to_string())
        .unwrap_or_default()
}

pub fn write(message: &str) {
    let Some(path) = LOG_PATH.get() else {
        eprintln!("{message}");
        return;
    };
    let _guard = LOG_LOCK.lock();
    rotate_if_needed(path);
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(mut file) => {
            let _ = writeln!(file, "{} {message}", timestamp());
        }
        Err(error) => eprintln!("не удалось записать журнал: {error}"),
    }
}

/// Приёмник диагностики процесса модуля для ядра.
pub fn logger() -> Logger {
    Arc::new(|line: &str| write(line))
}

fn rotate_if_needed(path: &PathBuf) {
    if let Ok(metadata) = fs::metadata(path) {
        if metadata.len() > MAX_LOG_BYTES {
            let rotated = path.with_extension("log.1");
            let _ = fs::rename(path, rotated);
        }
    }
}

fn timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = now.as_secs() as i64;
    let millis = now.subsec_millis();
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60
    )
}

/// Переводит дни от 1970-01-01 в григорианскую дату (алгоритм Ховарда Хиннанта).
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (year + if month <= 2 { 1 } else { 0 }, month, day)
}

#[cfg(test)]
mod tests {
    use super::civil_from_days;

    #[test]
    fn converts_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_000), (2022, 1, 8));
    }
}
