//! Ошибки ядра: запуск процесса, протокол, таймауты и ошибки модуля.

use std::time::Duration;

use util_maker_sdk::{ManifestError, MAX_MESSAGE_BYTES};

/// Конечная ошибка вызова или запуска модуля.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("не удалось запустить модуль {path}: {source}")]
    Spawn {
        path: String,
        source: std::io::Error,
    },
    #[error("модуль не ответил на {method} за {timeout:?}")]
    Timeout { method: String, timeout: Duration },
    #[error("процесс модуля завершился: {0}")]
    ProcessExited(String),
    #[error("нарушение протокола: {0}")]
    Protocol(String),
    #[error("сообщение превышает предел {MAX_MESSAGE_BYTES} байт")]
    MessageTooLarge,
    #[error("модуль вернул ошибку {code}: {message}")]
    Module { code: i64, message: String },
    #[error("несовместимая версия API: {0}")]
    IncompatibleApi(String),
    #[error("действие {0} не найдено")]
    ActionNotFound(String),
    #[error("точка расширения {0} не найдена")]
    ExtensionNotFound(String),
    #[error("вклад расширения несовместим: {0}")]
    ExtensionIncompatible(String),
    #[error("плагин {0} отключён")]
    ModuleDisabled(String),
    #[error("плагин {0} не установлен")]
    UnknownModule(String),
    #[error("вызов {call_id} отменён")]
    Cancelled { call_id: String },
    #[error("манифест модуля отклонён: {0}")]
    Manifest(#[from] ManifestError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("внутренняя ошибка протокола: {0}")]
    Json(#[from] serde_json::Error),
}

impl CoreError {
    /// Стабильный машинный код для журнала и диагностики.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Spawn { .. } => "module_spawn_failed",
            Self::Timeout { .. } => "module_timeout",
            Self::ProcessExited(_) => "module_exited",
            Self::Protocol(_) => "protocol_violation",
            Self::MessageTooLarge => "message_too_large",
            Self::Module { .. } => "module_error",
            Self::IncompatibleApi(_) => "host_api_incompatible",
            Self::ActionNotFound(_) => "action_not_found",
            Self::ExtensionNotFound(_) => "extension_not_found",
            Self::ExtensionIncompatible(_) => "extension_incompatible",
            Self::ModuleDisabled(_) => "module_disabled",
            Self::UnknownModule(_) => "module_not_found",
            Self::Cancelled { .. } => "cancelled",
            Self::Manifest(_) => "manifest_invalid",
            Self::Io(_) => "io_error",
            Self::Json(_) => "json_error",
        }
    }

    /// Был ли вызов завершён по таймауту.
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::Timeout { .. })
    }
}
