//! Реестр установленных модулей и маршрутизация действия к процессу.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use semver::Version;
use serde_json::Value;
use util_maker_sdk::{
    error_codes, Action, InvocationSource, InvokeStatus, Manifest, HOST_API_VERSION,
};

use crate::error::CoreError;
use crate::ipc::{Logger, ModuleProcess};

/// Пара `moduleId + actionId`, по которой ядро принимает вызов.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ActionRef {
    pub module_id: String,
    pub action_id: String,
}

impl ActionRef {
    pub fn new(module_id: impl Into<String>, action_id: impl Into<String>) -> Self {
        Self {
            module_id: module_id.into(),
            action_id: action_id.into(),
        }
    }
}

/// Установленный модуль: манифест и проверенная точка входа.
#[derive(Debug, Clone)]
pub struct ModuleEntry {
    pub manifest: Manifest,
    pub executable: PathBuf,
}

/// Конечный результат вызова с измеренной длительностью.
#[derive(Debug, Clone)]
pub struct InvokeOutcome {
    pub call_id: String,
    pub module_id: String,
    pub action_id: String,
    pub action_name: String,
    pub status: InvokeStatus,
    pub message: String,
    pub data: Option<Value>,
    pub elapsed: Duration,
}

/// Реестр активных модулей. Список действий доступен без запуска процессов.
#[derive(Debug, Default)]
pub struct Registry {
    modules: BTreeMap<String, ModuleEntry>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Добавляет проверенный модуль. Повторный ID отклоняется.
    pub fn add(&mut self, manifest: Manifest, executable: PathBuf) -> Result<(), CoreError> {
        manifest.validate()?;
        let host = Version::parse(HOST_API_VERSION)
            .map_err(|error| CoreError::IncompatibleApi(error.to_string()))?;
        if !manifest.supports_host_api(&host)? {
            return Err(CoreError::IncompatibleApi(format!(
                "модуль {} требует hostApi {}, хост предлагает {}",
                manifest.id, manifest.host_api, host
            )));
        }
        if self.modules.contains_key(&manifest.id) {
            return Err(CoreError::Protocol(format!(
                "модуль {} уже зарегистрирован",
                manifest.id
            )));
        }
        self.modules.insert(
            manifest.id.clone(),
            ModuleEntry {
                manifest,
                executable,
            },
        );
        Ok(())
    }

    pub fn module(&self, module_id: &str) -> Option<&ModuleEntry> {
        self.modules.get(module_id)
    }

    pub fn modules(&self) -> impl Iterator<Item = &ModuleEntry> {
        self.modules.values()
    }

    /// Находит владельца действия в манифестах без запуска процесса.
    pub fn action(&self, target: &ActionRef) -> Option<(&ModuleEntry, &Action)> {
        let entry = self.modules.get(&target.module_id)?;
        let action = entry.manifest.action(&target.action_id)?;
        Some((entry, action))
    }

    /// Запускает модуль, согласует API и передаёт одно действие.
    ///
    /// Процесс завершается в любом исходе, поэтому успешный вызов не оставляет
    /// работающих дочерних процессов.
    pub fn invoke(
        &self,
        target: &ActionRef,
        source: InvocationSource,
        timeout: Duration,
        logger: Option<Logger>,
    ) -> Result<InvokeOutcome, CoreError> {
        let (entry, action) = self.action(target).ok_or_else(|| {
            CoreError::ActionNotFound(format!("{}.{}", target.module_id, target.action_id))
        })?;
        let started = Instant::now();
        let outcome_logger = logger.clone();
        let mut process = match logger {
            Some(logger) => ModuleProcess::spawn_with_logger(&entry.executable, logger)?,
            None => ModuleProcess::spawn(&entry.executable)?,
        };
        let initialized = process.initialize(&entry.manifest.id)?;
        if initialized.module_id != entry.manifest.id {
            process.shutdown();
            return Err(CoreError::Protocol(format!(
                "модуль представился как {}, ожидался {}",
                initialized.module_id, entry.manifest.id
            )));
        }
        let pid = process.pid();
        let call_id = make_call_id();
        let result = process.invoke(&call_id, &action.id, source, timeout);
        process.shutdown();
        let result = result?;
        let elapsed = started.elapsed();
        let message = match result.status {
            InvokeStatus::Ok => result
                .message
                .clone()
                .unwrap_or_else(|| "Действие выполнено".to_owned()),
            InvokeStatus::Cancelled => "Действие отменено".to_owned(),
        };
        if let Some(logger) = outcome_logger.as_ref() {
            logger(&format!(
                "[invoke] module={} action={} source={} outcome={:?} elapsed_ms={} module_pid={}",
                entry.manifest.id,
                action.id,
                source.as_str(),
                result.status,
                elapsed.as_millis(),
                pid
            ));
        }
        Ok(InvokeOutcome {
            call_id,
            module_id: entry.manifest.id.clone(),
            action_id: action.id.clone(),
            action_name: action.name.clone(),
            status: result.status,
            message,
            data: result.data,
            elapsed,
        })
    }
}

fn make_call_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("call-{}-{sequence}", std::process::id())
}

/// Возвращает true для ошибки, которой модуль обозначил отмену вызова.
pub fn is_cancellation(error: &CoreError) -> bool {
    matches!(error, CoreError::Module { code, .. } if *code == error_codes::CANCELLED)
}
