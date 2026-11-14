//! Публичный исход сбоя вызова для оболочки.
//!
//! В отличие от внутренней [`CoreError`], здесь всегда есть адрес
//! `moduleId + actionId + callId`. Текст исключения, stderr и пользовательские
//! данные не копируются: сообщение проходит через [`crate::diagnostics::sanitize`]
//! и ограничивается по длине.

use std::time::Duration;

use util_maker_sdk::error_codes;

use crate::diagnostics::sanitize;
use crate::error::CoreError;

/// Машинная категория сбоя, которую оболочка показывает и журналирует.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureCategory {
    ModuleCrashed,
    ModuleReported,
    Cancelled,
    Timeout,
    Protocol,
    Spawn,
    IncompatibleApi,
    ActionUnavailable,
    ModuleDisabled,
    ModuleNotFound,
    Host,
    Internal,
}

impl FailureCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ModuleCrashed => "module_crashed",
            Self::ModuleReported => "module_reported_error",
            Self::Cancelled => "cancelled",
            Self::Timeout => "module_timeout",
            Self::Protocol => "protocol_violation",
            Self::Spawn => "module_spawn_failed",
            Self::IncompatibleApi => "host_api_incompatible",
            Self::ActionUnavailable => "action_unavailable",
            Self::ModuleDisabled => "module_disabled",
            Self::ModuleNotFound => "module_not_found",
            Self::Host => "host_error",
            Self::Internal => "internal_error",
        }
    }
}

/// Конечный сбой одного вызова. Поля позволяют адресовать отключение именно
/// тому модулю, который завершился, даже если параллельно пришёл результат
/// другого вызова.
#[derive(Debug)]
pub struct InvokeFailure {
    pub call_id: String,
    pub module_id: String,
    pub action_id: String,
    pub action_name: Option<String>,
    pub category: FailureCategory,
    pub message: String,
    pub elapsed: Duration,
    pub rpc_code: Option<i64>,
    source: Box<CoreError>,
}

impl InvokeFailure {
    pub fn new(
        call_id: impl Into<String>,
        module_id: impl Into<String>,
        action_id: impl Into<String>,
        action_name: Option<String>,
        elapsed: Duration,
        source: CoreError,
    ) -> Self {
        let category = category_for(&source);
        let rpc_code = match &source {
            CoreError::Module { code, .. } => Some(*code),
            _ => None,
        };
        let message = safe_message(&source, category);
        Self {
            call_id: call_id.into(),
            module_id: module_id.into(),
            action_id: action_id.into(),
            action_name,
            category,
            message,
            elapsed,
            rpc_code,
            source: Box::new(source),
        }
    }

    pub fn code(&self) -> &'static str {
        self.category.as_str()
    }

    /// Внутренняя ошибка для совместимости с существующим API реестра.
    pub fn into_core(self) -> CoreError {
        *self.source
    }

    pub fn source(&self) -> &CoreError {
        &self.source
    }

    /// Безопасная строка для журнала: без stderr и пользовательских данных.
    pub fn log_line(&self) -> String {
        format!(
            "[invoke] module={} action={} call={} outcome=error category={} rpc_code={} elapsed_ms={} message={}",
            self.module_id,
            self.action_id,
            self.call_id,
            self.category.as_str(),
            self.rpc_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "-".to_owned()),
            self.elapsed.as_millis(),
            self.message
        )
    }
}

fn category_for(error: &CoreError) -> FailureCategory {
    match error {
        CoreError::Spawn { .. } | CoreError::Io(_) => FailureCategory::Spawn,
        CoreError::Timeout { .. } => FailureCategory::Timeout,
        CoreError::ProcessExited(_) => FailureCategory::ModuleCrashed,
        CoreError::Protocol(_) | CoreError::MessageTooLarge | CoreError::Json(_) => {
            FailureCategory::Protocol
        }
        CoreError::Module { code, .. } if *code == error_codes::CANCELLED => {
            FailureCategory::Cancelled
        }
        CoreError::Module { .. } => FailureCategory::ModuleReported,
        CoreError::IncompatibleApi(_) | CoreError::Manifest(_) => FailureCategory::IncompatibleApi,
        CoreError::ActionNotFound(_) => FailureCategory::ActionUnavailable,
        CoreError::ModuleDisabled(_) => FailureCategory::ModuleDisabled,
        CoreError::UnknownModule(_) => FailureCategory::ModuleNotFound,
        CoreError::Cancelled { .. } => FailureCategory::Cancelled,
    }
}

fn safe_message(error: &CoreError, category: FailureCategory) -> String {
    let text = match category {
        FailureCategory::ModuleCrashed => "плагин завершился до ответа".to_owned(),
        FailureCategory::Cancelled => "действие отменено".to_owned(),
        FailureCategory::Timeout => "плагин не ответил вовремя".to_owned(),
        FailureCategory::ModuleReported => match error {
            CoreError::Module { message, .. } => sanitize(message),
            _ => "плагин вернул ошибку".to_owned(),
        },
        FailureCategory::ActionUnavailable => match error {
            CoreError::ActionNotFound(value) => format!("действие {value} недоступно"),
            _ => "действие недоступно".to_owned(),
        },
        FailureCategory::ModuleDisabled => match error {
            CoreError::ModuleDisabled(value) => format!("плагин {value} отключён"),
            _ => "плагин отключён".to_owned(),
        },
        FailureCategory::ModuleNotFound => match error {
            CoreError::UnknownModule(value) => format!("плагин {value} не установлен"),
            _ => "плагин не установлен".to_owned(),
        },
        _ => sanitize(&error.to_string()),
    };
    truncate(&text, 400)
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut result: String = text.chars().take(limit).collect();
    result.push('…');
    result
}
