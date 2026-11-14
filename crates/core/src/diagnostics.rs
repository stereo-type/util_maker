//! Структурированный локальный журнал с ротацией.
//!
//! Запись содержит только служебные поля: версию, модуль, действие,
//! длительность, исход и код ошибки. Исходный текст, перевод, снимок, буфер,
//! ключи и заголовки авторизации в структуру не входят; свободный текст ошибки
//! проходит через [`sanitize`].

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Одна запись журнала.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub time_ms: u64,
    pub app_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl LogEntry {
    /// Запись об исходе вызова. Пользовательские данные сюда не передаются.
    pub fn invocation(
        app_version: impl Into<String>,
        module_id: impl Into<String>,
        action_id: impl Into<String>,
        duration_ms: u64,
        outcome: impl Into<String>,
        error_code: Option<&str>,
    ) -> Self {
        Self {
            time_ms: now_ms(),
            app_version: app_version.into(),
            module_id: Some(module_id.into()),
            action_id: Some(action_id.into()),
            duration_ms: Some(duration_ms),
            outcome: outcome.into(),
            error_code: error_code.map(str::to_owned),
            message: None,
        }
    }

    /// Добавляет очищенное пояснение ошибки.
    pub fn with_message(mut self, message: &str) -> Self {
        self.message = Some(sanitize(message));
        self
    }
}

/// Журнал текущего пользователя. Хранит одну активную запись и один архив.
pub struct DiagnosticsLog {
    path: PathBuf,
    file: File,
    max_bytes: u64,
    written: u64,
}

impl DiagnosticsLog {
    /// Открывает журнал в каталоге пользователя, создавая каталог при необходимости.
    pub fn open(directory: &Path, max_bytes: u64) -> io::Result<Self> {
        fs::create_dir_all(directory)?;
        let path = directory.join("util-maker.jsonl");
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let written = file.metadata()?.len();
        Ok(Self {
            path,
            file,
            max_bytes,
            written,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Добавляет запись, при необходимости ротируя файл.
    pub fn record(&mut self, entry: &LogEntry) -> io::Result<()> {
        let line = serde_json::to_string(entry)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if self.max_bytes > 0
            && self.written > 0
            && self.written + line.len() as u64 + 1 > self.max_bytes
        {
            self.rotate()?;
        }
        self.file.write_all(line.as_bytes())?;
        self.file.write_all(b"\n")?;
        self.file.flush()?;
        self.written += line.len() as u64 + 1;
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        let archived = self.path.with_extension("jsonl.1");
        let _ = fs::remove_file(&archived);
        fs::rename(&self.path, &archived)?;
        self.file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.path)?;
        self.written = 0;
        Ok(())
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Удаляет из свободного текста URL с параметрами и заголовки авторизации.
///
/// Ключи и токены не пытаются распознаваться по виду значения: если в тексте
/// встречается заголовок авторизации, сообщение скрывается целиком.
pub fn sanitize(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    if lower.contains("authorization")
        || lower.contains("api-key")
        || lower.contains("apikey")
        || lower.contains("api_key")
    {
        return "[скрыто: заголовок авторизации]".to_owned();
    }
    let mut tokens = Vec::new();
    for token in text.split_whitespace() {
        if let Some(position) = token.find('?') {
            if token.contains("://") {
                tokens.push(format!("{}?[скрыто]", &token[..position]));
                continue;
            }
        }
        let lowered = token.to_ascii_lowercase();
        let mut redacted = token.to_owned();
        for marker in ["key=", "token=", "secret="] {
            if let Some(position) = lowered.find(marker) {
                redacted = format!("{}[скрыто]", &token[..position + marker.len()]);
                break;
            }
        }
        tokens.push(redacted);
    }
    tokens.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_directory() -> PathBuf {
        let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
        let directory = std::env::temp_dir().join(format!(
            "util-maker-diagnostics-{}-{unique}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        directory
    }

    fn read_lines(path: &Path) -> Vec<LogEntry> {
        fs::read_to_string(path)
            .expect("журнал")
            .lines()
            .map(|line| serde_json::from_str(line).expect("JSON-запись"))
            .collect()
    }

    #[test]
    fn entry_contains_version_module_action_duration_and_code() {
        let directory = temp_directory();
        let mut log = DiagnosticsLog::open(&directory, 1_000_000).expect("журнал");
        let entry = LogEntry::invocation(
            "0.1.0",
            "org.utilmaker.control",
            "ping",
            42,
            "error",
            Some("module_timeout"),
        );
        log.record(&entry).expect("запись");
        let lines = read_lines(log.path());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].app_version, "0.1.0");
        assert_eq!(lines[0].module_id.as_deref(), Some("org.utilmaker.control"));
        assert_eq!(lines[0].action_id.as_deref(), Some("ping"));
        assert_eq!(lines[0].duration_ms, Some(42));
        assert_eq!(lines[0].error_code.as_deref(), Some("module_timeout"));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn secrets_and_user_data_are_not_written() {
        let message = "Ошибка HTTP 401: Authorization: Api-Key secret-key-123; \
                       URL https://translate.example/v2?key=secret-key-123&text=контрольная%20фраза";
        let cleaned = sanitize(message);
        assert!(!cleaned.contains("secret-key-123"));
        assert!(!cleaned.contains("контрольная"));
        assert!(!cleaned.contains("Api-Key"));
        assert!(!cleaned.to_ascii_lowercase().contains("authorization"));

        let directory = temp_directory();
        let mut log = DiagnosticsLog::open(&directory, 1_000_000).expect("журнал");
        let entry = LogEntry::invocation(
            "0.1.0",
            "org.utilmaker.translator",
            "translate",
            9,
            "error",
            Some("unauthorized"),
        )
        .with_message(message);
        log.record(&entry).expect("запись");
        let contents = fs::read_to_string(log.path()).expect("журнал");
        assert!(!contents.contains("secret-key-123"));
        assert!(!contents.contains("контрольная"));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn url_query_is_removed_without_header() {
        let cleaned =
            sanitize("запрос https://example.test/translate?text=секрет&lang=ru не удался");
        assert!(!cleaned.contains("секрет"));
        assert!(cleaned.contains("https://example.test/translate?[скрыто]"));
    }

    #[test]
    fn rotation_keeps_latest_event_in_current_log() {
        let directory = temp_directory();
        let mut log = DiagnosticsLog::open(&directory, 260).expect("журнал");
        for index in 0..8 {
            let entry =
                LogEntry::invocation("0.1.0", "org.utilmaker.control", "ping", index, "ok", None);
            log.record(&entry).expect("запись");
        }
        let archived = directory.join("util-maker.jsonl.1");
        assert!(archived.exists(), "архив не создан");
        let current = read_lines(log.path());
        assert!(!current.is_empty());
        assert_eq!(
            current.last().expect("последняя запись").duration_ms,
            Some(7)
        );
        let _ = fs::remove_dir_all(directory);
    }
}
