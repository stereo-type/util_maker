//! Реестр установленных модулей и маршрутизация действия к процессу.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use util_maker_sdk::{
    error_codes, Action, DeniedHost, ExtensionContributionInfo, ExtensionInvokeRequest,
    ExtensionInvokeResult, ExtensionListResult, ExtensionPoint, HostCallContext, HostHandler,
    InvocationSource, InvokeStatus, Manifest, RpcError, HOST_API_VERSION,
};

use crate::capabilities::{CallSession, HostSession, InvocationContext, SessionHost};
use crate::error::CoreError;
use crate::failure::InvokeFailure;
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
    /// Аргументы тестового/диагностического процесса; для пакетов пусты.
    pub arguments: Vec<String>,
    pub enabled: bool,
}

impl ModuleEntry {
    pub fn new(manifest: Manifest, executable: PathBuf, enabled: bool) -> Self {
        Self {
            manifest,
            executable,
            arguments: Vec::new(),
            enabled,
        }
    }

    pub fn with_arguments(mut self, arguments: Vec<String>) -> Self {
        self.arguments = arguments;
        self
    }
}

/// Состояние одного вклада для интерфейса и родителя.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionContributionStatus {
    pub module_id: String,
    pub name: String,
    pub description: Option<String>,
    pub enabled: bool,
    pub available: bool,
    pub reason: Option<String>,
}

/// Точка расширения с выбранным и доступными вкладами.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionPointStatus {
    pub module_id: String,
    pub point_id: String,
    pub name: String,
    pub contract: String,
    pub contract_version: String,
    pub required: bool,
    pub selected: Option<String>,
    pub needs_selection: bool,
    pub contributions: Vec<ExtensionContributionStatus>,
}

/// Действие в меню. Чтение каталога не запускает процессы.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionListing {
    pub module_id: String,
    pub module_name: String,
    pub module_version: String,
    pub action_id: String,
    pub action_name: String,
    pub description: Option<String>,
    pub enabled: bool,
}

/// Конечный результат вызова с измеренной длительностью.
#[derive(Debug, Clone)]
pub struct InvokeOutcome {
    pub call_id: String,
    pub module_id: String,
    pub module_version: String,
    pub action_id: String,
    pub action_name: String,
    pub status: InvokeStatus,
    pub message: String,
    pub data: Option<Value>,
    pub elapsed: Duration,
    /// Процесс был запущен для этого вызова, то есть отклик холодный.
    pub cold: bool,
}

#[derive(Debug, Clone)]
struct ActiveCall {
    module_id: String,
    cancel: Arc<AtomicBool>,
}

/// Реестр активных модулей. Модули можно обновлять, пока другие вызовы уже
/// выполняются: каждый вызов работает с собственным снимком `ModuleEntry`.
#[derive(Debug, Default)]
pub struct Registry {
    modules: RwLock<BTreeMap<String, ModuleEntry>>,
    active: Mutex<BTreeMap<String, ActiveCall>>,
    disabled: Mutex<BTreeSet<String>>,
    selections: Mutex<BTreeMap<String, String>>,
    state_path: Option<PathBuf>,
    selection_path: Option<PathBuf>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Открывает реестр с постоянным списком отключённых пакетов.
    pub fn with_state_path(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let path = path.as_ref().to_path_buf();
        let disabled = load_disabled(&path)?;
        let selection_path = path.with_file_name("extensions.json");
        let selections = load_selections(&selection_path)?;
        Ok(Self {
            disabled: Mutex::new(disabled),
            selections: Mutex::new(selections),
            state_path: Some(path),
            selection_path: Some(selection_path),
            ..Self::default()
        })
    }

    /// Добавляет проверенный модуль. Повторный ID отклоняется.
    pub fn add(&mut self, manifest: Manifest, executable: PathBuf) -> Result<(), CoreError> {
        self.add_entry(ModuleEntry::new(manifest, executable, true))
    }

    pub fn add_entry(&self, mut entry: ModuleEntry) -> Result<(), CoreError> {
        validate_entry(&entry)?;
        let mut modules = self.modules.write().expect("реестр модулей");
        if modules.contains_key(&entry.manifest.id) {
            return Err(CoreError::Protocol(format!(
                "модуль {} уже зарегистрирован",
                entry.manifest.id
            )));
        }
        if self
            .disabled
            .lock()
            .expect("отключённые модули")
            .contains(&entry.manifest.id)
        {
            entry.enabled = false;
        }
        modules.insert(entry.manifest.id.clone(), entry);
        Ok(())
    }

    /// Атомарно заменяет каталог активных пакетов. Уже начатые вызовы
    /// продолжают работать со старым снимком, а новый вызов увидит новую
    /// версию сразу после возврата из метода.
    pub fn replace_packages(&self, entries: Vec<ModuleEntry>) -> Result<(), CoreError> {
        let mut replacement = BTreeMap::new();
        for mut entry in entries {
            validate_entry(&entry)?;
            if replacement.contains_key(&entry.manifest.id) {
                return Err(CoreError::Protocol(format!(
                    "модуль {} указан в каталоге дважды",
                    entry.manifest.id
                )));
            }
            if self
                .disabled
                .lock()
                .expect("отключённые модули")
                .contains(&entry.manifest.id)
            {
                entry.enabled = false;
            }
            replacement.insert(entry.manifest.id.clone(), entry);
        }
        *self.modules.write().expect("реестр модулей") = replacement;
        Ok(())
    }

    pub fn module(&self, module_id: &str) -> Option<ModuleEntry> {
        self.modules
            .read()
            .expect("реестр модулей")
            .get(module_id)
            .cloned()
    }

    pub fn modules(&self) -> Vec<ModuleEntry> {
        self.modules
            .read()
            .expect("реестр модулей")
            .values()
            .cloned()
            .collect()
    }

    /// Действия, доступные меню. Отключённые пакеты и их процессы не видны.
    pub fn actions(&self) -> Vec<ActionListing> {
        let modules = self.modules.read().expect("реестр модулей");
        modules
            .values()
            .filter(|entry| entry.enabled)
            .flat_map(|entry| {
                entry
                    .manifest
                    .actions
                    .iter()
                    .map(move |action| ActionListing {
                        module_id: entry.manifest.id.clone(),
                        module_name: entry.manifest.name.clone(),
                        module_version: entry.manifest.version.clone(),
                        action_id: action.id.clone(),
                        action_name: action.name.clone(),
                        description: action.description.clone(),
                        enabled: entry.enabled,
                    })
            })
            .collect()
    }

    /// Точки расширения родителя с активным и доступными вкладами. Процессы
    /// не запускаются: сведения берутся из манифестов. Отключённый родитель
    /// не показывает свои точки.
    pub fn extension_points(&self, module_id: &str) -> Vec<ExtensionPointStatus> {
        let modules = self.modules.read().expect("реестр модулей");
        let Some(parent) = modules.get(module_id) else {
            return Vec::new();
        };
        if !parent.enabled {
            return Vec::new();
        }
        parent
            .manifest
            .extension_points
            .iter()
            .map(|point| self.point_status(&modules, parent, point))
            .collect()
    }

    /// Список для родителя: только запрошенная точка и только её вклады.
    /// Этот ответ уходит в `extensions.list` и не запускает плагины.
    pub fn extension_list(
        &self,
        module_id: &str,
        point_id: &str,
    ) -> Result<ExtensionListResult, RpcError> {
        let modules = self.modules.read().expect("реестр модулей");
        let parent = modules.get(module_id).ok_or_else(|| {
            extension_error(
                error_codes::EXTENSION_NOT_FOUND,
                format!("родитель {module_id} не установлен"),
                None,
            )
        })?;
        if !parent.enabled {
            return Err(extension_error(
                error_codes::EXTENSION_NOT_FOUND,
                format!("родитель {module_id} отключён"),
                None,
            ));
        }
        let point = parent.manifest.extension_point(point_id).ok_or_else(|| {
            extension_error(
                error_codes::EXTENSION_NOT_FOUND,
                format!("точка {point_id} не объявлена модулем {module_id}"),
                None,
            )
        })?;
        let status = self.point_status(&modules, parent, point);
        Ok(ExtensionListResult {
            point_id: status.point_id.clone(),
            selected: status.selected.clone(),
            required: status.required,
            contributions: status
                .contributions
                .into_iter()
                .map(|contribution| ExtensionContributionInfo {
                    module_id: contribution.module_id,
                    name: contribution.name,
                    available: contribution.available,
                    reason: contribution.reason,
                })
                .collect(),
        })
    }

    /// Выбирает активный вклад для точки `single`. Сохранённый ID не
    /// стирается при отключении плагина: выбор снова подействует после
    /// восстановления.
    pub fn select_extension(
        &self,
        module_id: &str,
        point_id: &str,
        plugin_id: &str,
    ) -> Result<(), CoreError> {
        let modules = self.modules.read().expect("реестр модулей");
        let parent = modules
            .get(module_id)
            .ok_or_else(|| CoreError::UnknownModule(module_id.to_owned()))?;
        let point = parent
            .manifest
            .extension_point(point_id)
            .ok_or_else(|| CoreError::ExtensionNotFound(format!("{module_id}/{point_id}")))?;
        let plugin = modules
            .get(plugin_id)
            .ok_or_else(|| CoreError::UnknownModule(plugin_id.to_owned()))?;
        let contribution = plugin
            .manifest
            .provides
            .iter()
            .find(|item| item.module_id == module_id && item.point_id == point_id)
            .ok_or_else(|| {
                CoreError::ExtensionNotFound(format!("вклад {plugin_id} в {module_id}/{point_id}"))
            })?;
        if contribution.contract != point.contract
            || !interface_matches(&contribution.interface_version, &point.contract_version)
        {
            return Err(CoreError::ExtensionIncompatible(format!(
                "{plugin_id} несовместим с {module_id}/{point_id}"
            )));
        }
        let key = selection_key(module_id, point_id);
        let mut selections = self.selections.lock().expect("выбор расширений");
        let previous = selections.insert(key, plugin_id.to_owned());
        if let Err(error) = self.persist_selections(&selections) {
            match previous {
                Some(value) => {
                    selections.insert(selection_key(module_id, point_id), value);
                }
                None => {
                    selections.remove(&selection_key(module_id, point_id));
                }
            }
            return Err(error);
        }
        Ok(())
    }

    /// Запускает активный вклад в дочернем процессе со своими возможностями
    /// и настройками. Родитель не получает путь к чужому исполняемому файлу.
    pub fn invoke_extension(
        &self,
        parent: &CallSession,
        request: &ExtensionInvokeRequest,
        logger: Option<Logger>,
        host: &mut dyn SessionHost,
    ) -> Result<ExtensionInvokeResult, RpcError> {
        let parent_entry = self.module(&parent.module_id).ok_or_else(|| {
            extension_error(
                error_codes::EXTENSION_NOT_FOUND,
                format!("родитель {} не установлен", parent.module_id),
                None,
            )
        })?;
        if !parent_entry.enabled {
            return Err(extension_error(
                error_codes::EXTENSION_NOT_FOUND,
                format!("родитель {} отключён", parent.module_id),
                None,
            ));
        }
        let point = parent_entry
            .manifest
            .extension_point(&request.point_id)
            .ok_or_else(|| {
                extension_error(
                    error_codes::EXTENSION_NOT_FOUND,
                    format!(
                        "точка {} не объявлена модулем {}",
                        request.point_id, parent.module_id
                    ),
                    None,
                )
            })?
            .clone();

        let status = {
            let modules = self.modules.read().expect("реестр модулей");
            self.point_status(&modules, &parent_entry, &point)
        };
        let selected = status.selected.ok_or_else(|| {
            extension_error(
                error_codes::EXTENSION_NOT_SELECTED,
                format!("для точки {} не выбран плагин", request.point_id),
                None,
            )
        })?;
        let child = self.module(&selected).ok_or_else(|| {
            extension_error(
                error_codes::EXTENSION_FAILED,
                format!("плагин {selected} не установлен"),
                Some(&selected),
            )
        })?;
        if !child.enabled {
            return Err(extension_error(
                error_codes::EXTENSION_FAILED,
                format!("плагин {selected} отключён"),
                Some(&selected),
            ));
        }

        let timeout_ms = request.timeout_ms.unwrap_or(10_000).min(30_000);
        let timeout = Duration::from_millis(u64::from(timeout_ms.max(1)));
        let child_call_id = format!("{}/{}", parent.call_id, selected);
        let child_session = CallSession::new(
            child_call_id.clone(),
            child.manifest.id.clone(),
            "extension.invoke",
            parent.context.clone(),
        );
        let cancel = Arc::new(AtomicBool::new(false));
        self.active.lock().expect("активные вызовы").insert(
            child_call_id.clone(),
            ActiveCall {
                module_id: child.manifest.id.clone(),
                cancel: cancel.clone(),
            },
        );
        host.begin_call(&child_session);

        let parent_cancel = self
            .active
            .lock()
            .expect("активные вызовы")
            .get(&parent.call_id)
            .map(|call| call.cancel.clone());
        let stop_watch = Arc::new(AtomicBool::new(false));
        let watcher = parent_cancel.map(|flag| {
            let child_flag = cancel.clone();
            let stop = stop_watch.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    if flag.load(Ordering::SeqCst) {
                        child_flag.store(true, Ordering::SeqCst);
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            })
        });

        let result = self.run_extension(
            &child,
            &child_session,
            request,
            timeout,
            &cancel,
            logger,
            host,
        );

        stop_watch.store(true, Ordering::SeqCst);
        if let Some(watcher) = watcher {
            let _ = watcher.join();
        }
        host.finish_call(&child_call_id);
        self.active
            .lock()
            .expect("активные вызовы")
            .remove(&child_call_id);

        result.map_err(|error| map_extension_error(error, &selected))
    }

    #[allow(clippy::too_many_arguments)]
    fn run_extension(
        &self,
        child: &ModuleEntry,
        child_session: &CallSession,
        request: &ExtensionInvokeRequest,
        timeout: Duration,
        cancel: &Arc<AtomicBool>,
        logger: Option<Logger>,
        host: &mut dyn HostHandler,
    ) -> Result<ExtensionInvokeResult, CoreError> {
        let arguments: Vec<&str> = child.arguments.iter().map(String::as_str).collect();
        let process_logger = logger.unwrap_or_else(|| Arc::new(|line: &str| eprintln!("{line}")));
        let mut process =
            ModuleProcess::spawn_with_args(&child.executable, &arguments, process_logger)?;
        let initialized = process
            .initialize_with_capabilities(&child.manifest.id, &child.manifest.capabilities)?;
        if initialized.module_id != child.manifest.id {
            process.shutdown();
            return Err(CoreError::Protocol(format!(
                "плагин представился как {}, ожидался {}",
                initialized.module_id, child.manifest.id
            )));
        }
        let result = process.extension_invoke_with_host(
            &child_session.call_id,
            &request.point_id,
            &request.operation,
            request.params.clone(),
            timeout,
            cancel.as_ref(),
            host,
            &child.manifest.capabilities,
        );
        process.shutdown();
        result
    }
    /// Все действия, включая отключённые; нужно странице управления пакетами.
    pub fn all_actions(&self) -> Vec<ActionListing> {
        let modules = self.modules.read().expect("реестр модулей");
        modules
            .values()
            .flat_map(|entry| {
                entry
                    .manifest
                    .actions
                    .iter()
                    .map(move |action| ActionListing {
                        module_id: entry.manifest.id.clone(),
                        module_name: entry.manifest.name.clone(),
                        module_version: entry.manifest.version.clone(),
                        action_id: action.id.clone(),
                        action_name: action.name.clone(),
                        description: action.description.clone(),
                        enabled: entry.enabled,
                    })
            })
            .collect()
    }

    /// Отключает или включает пакет по ID. Отключение идемпотентно и сразу
    /// отменяет уже начатые вызовы этого модуля.
    pub fn set_enabled(&self, module_id: &str, enabled: bool) -> Result<bool, CoreError> {
        let mut modules = self.modules.write().expect("реестр модулей");
        let entry = modules
            .get_mut(module_id)
            .ok_or_else(|| CoreError::UnknownModule(module_id.to_owned()))?;
        if entry.enabled == enabled {
            return Ok(false);
        }
        entry.enabled = enabled;
        let mut disabled = self.disabled.lock().expect("отключённые модули");
        let changed = if enabled {
            disabled.remove(module_id)
        } else {
            disabled.insert(module_id.to_owned())
        };
        if let Err(error) = self.persist_disabled(&disabled) {
            if changed {
                if enabled {
                    disabled.insert(module_id.to_owned());
                } else {
                    disabled.remove(module_id);
                }
            }
            if let Some(entry) = modules.get_mut(module_id) {
                entry.enabled = !enabled;
            }
            return Err(error);
        }
        if !enabled {
            let active = self.active.lock().expect("активные вызовы");
            for call in active.values().filter(|call| call.module_id == module_id) {
                call.cancel.store(true, Ordering::SeqCst);
            }
        }
        Ok(true)
    }

    pub fn disabled_modules(&self) -> BTreeSet<String> {
        self.disabled.lock().expect("отключённые модули").clone()
    }

    pub fn active_calls(&self, module_id: &str) -> usize {
        self.active
            .lock()
            .expect("активные вызовы")
            .values()
            .filter(|call| call.module_id == module_id)
            .count()
    }

    /// Менеджер пакетов может безопасно переключать версию только при
    /// отсутствии вызовов старого снимка.
    pub fn can_switch(&self, module_id: &str) -> bool {
        self.active_calls(module_id) == 0
    }

    /// Регистрирует внешний активный вызов, который не запускался через
    /// [`Registry::invoke`], чтобы его можно было отменить и учесть при
    /// переключении версии. Возвращает флаг отмены.
    pub fn track_call(&self, call_id: &str, module_id: &str) -> Arc<AtomicBool> {
        let cancel = Arc::new(AtomicBool::new(false));
        self.active.lock().expect("активные вызовы").insert(
            call_id.to_owned(),
            ActiveCall {
                module_id: module_id.to_owned(),
                cancel: cancel.clone(),
            },
        );
        cancel
    }

    /// Снимает регистрацию внешнего активного вызова.
    pub fn untrack_call(&self, call_id: &str) {
        self.active.lock().expect("активные вызовы").remove(call_id);
    }

    /// Просит отменить конкретный вызов по его `callId`. Используется
    /// оболочкой при отмене родительского вызова расширения.
    pub fn cancel_call(&self, call_id: &str) {
        if let Some(call) = self.active.lock().expect("активные вызовы").get(call_id)
        {
            call.cancel.store(true, Ordering::SeqCst);
        }
    }

    /// Отменяет все текущие вызовы, например при выходе из приложения.
    pub fn cancel_all(&self) {
        for call in self.active.lock().expect("активные вызовы").values() {
            call.cancel.store(true, Ordering::SeqCst);
        }
    }

    /// Находит действие в манифестах без запуска процесса.
    pub fn action(&self, target: &ActionRef) -> Option<(ModuleEntry, Action)> {
        self.resolve(target).ok()
    }

    fn resolve(&self, target: &ActionRef) -> Result<(ModuleEntry, Action), CoreError> {
        let entry = self
            .module(&target.module_id)
            .ok_or_else(|| CoreError::UnknownModule(target.module_id.clone()))?;
        if !entry.enabled {
            return Err(CoreError::ModuleDisabled(target.module_id.clone()));
        }
        let action = entry
            .manifest
            .action(&target.action_id)
            .cloned()
            .ok_or_else(|| {
                CoreError::ActionNotFound(format!("{}.{}", target.module_id, target.action_id))
            })?;
        Ok((entry, action))
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
        self.invoke_detailed(target, source, timeout, logger)
            .map_err(|failure| (*failure).into_core())
    }

    /// Вариант [`Registry::invoke`] с публичным исходом сбоя для оболочки.
    pub fn invoke_detailed(
        &self,
        target: &ActionRef,
        source: InvocationSource,
        timeout: Duration,
        logger: Option<Logger>,
    ) -> Result<InvokeOutcome, Box<InvokeFailure>> {
        let mut host = DeniedHost;
        self.invoke_detailed_with_host(
            target,
            InvocationContext::new(source),
            timeout,
            logger,
            &mut host,
        )
    }

    /// Запускает модуль и обслуживает его обращения к возможностям хоста.
    pub fn invoke_with_host(
        &self,
        target: &ActionRef,
        source: InvocationSource,
        timeout: Duration,
        logger: Option<Logger>,
        host: &mut dyn HostHandler,
    ) -> Result<InvokeOutcome, CoreError> {
        self.invoke_detailed_with_host(
            target,
            InvocationContext::new(source),
            timeout,
            logger,
            host,
        )
        .map_err(|failure| (*failure).into_core())
    }

    pub fn invoke_detailed_with_host(
        &self,
        target: &ActionRef,
        context: InvocationContext,
        timeout: Duration,
        logger: Option<Logger>,
        host: &mut dyn HostHandler,
    ) -> Result<InvokeOutcome, Box<InvokeFailure>> {
        let mut plain = PlainSession { host };
        self.invoke_session(target, context, timeout, logger, &mut plain)
    }

    /// Основной маршрут с закреплением контекста вызова и освобождением
    /// ресурсов даже при отмене или аварии плагина.
    pub fn invoke_session(
        &self,
        target: &ActionRef,
        context: InvocationContext,
        timeout: Duration,
        logger: Option<Logger>,
        session: &mut dyn SessionHost,
    ) -> Result<InvokeOutcome, Box<InvokeFailure>> {
        self.invoke_session_with_params(target, context, Value::Null, timeout, logger, session)
    }

    /// Вариант вызова с дополнительными входными данными действия. Старые
    /// модули получают `params: null` и продолжают работать без изменений.
    pub fn invoke_session_with_params(
        &self,
        target: &ActionRef,
        context: InvocationContext,
        params: Value,
        timeout: Duration,
        logger: Option<Logger>,
        session: &mut dyn SessionHost,
    ) -> Result<InvokeOutcome, Box<InvokeFailure>> {
        let call_id = make_call_id();
        let started = Instant::now();
        let failure_logger = logger.clone();
        let action_name = self.module(&target.module_id).and_then(|entry| {
            entry
                .manifest
                .action(&target.action_id)
                .map(|a| a.name.clone())
        });
        let (entry, action) = match self.resolve(target) {
            Ok(value) => value,
            Err(error) => {
                let failure = InvokeFailure::new(
                    call_id,
                    target.module_id.clone(),
                    target.action_id.clone(),
                    action_name,
                    started.elapsed(),
                    error,
                );
                if let Some(logger) = failure_logger.as_ref() {
                    logger(&failure.log_line());
                }
                return Err(Box::new(failure));
            }
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.active.lock().expect("активные вызовы").insert(
            call_id.clone(),
            ActiveCall {
                module_id: entry.manifest.id.clone(),
                cancel: cancel.clone(),
            },
        );
        let call_session = CallSession::new(
            call_id.clone(),
            entry.manifest.id.clone(),
            action.id.clone(),
            context,
        );
        session.begin_call(&call_session);

        let result = self.run_entry(
            &entry,
            &action,
            &call_id,
            &call_session,
            params,
            timeout,
            logger,
            &cancel,
            session,
        );

        session.finish_call(&call_id);
        self.active
            .lock()
            .expect("активные вызовы")
            .remove(&call_id);

        match result {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                let failure = InvokeFailure::new(
                    call_id,
                    entry.manifest.id.clone(),
                    action.id.clone(),
                    Some(action.name.clone()),
                    started.elapsed(),
                    error,
                );
                if let Some(logger) = failure_logger.as_ref() {
                    logger(&failure.log_line());
                }
                Err(Box::new(failure))
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn run_entry(
        &self,
        entry: &ModuleEntry,
        action: &Action,
        call_id: &str,
        call_session: &CallSession,
        params: Value,
        timeout: Duration,
        logger: Option<Logger>,
        cancel: &Arc<AtomicBool>,
        session: &mut dyn SessionHost,
    ) -> Result<InvokeOutcome, CoreError> {
        let started = Instant::now();
        let outcome_logger = logger.clone();
        let logger_for_process =
            logger.unwrap_or_else(|| Arc::new(|line: &str| eprintln!("{line}")));
        let arguments: Vec<&str> = entry.arguments.iter().map(String::as_str).collect();
        let mut process =
            ModuleProcess::spawn_with_args(&entry.executable, &arguments, logger_for_process)?;
        let initialized = process
            .initialize_with_capabilities(&entry.manifest.id, &entry.manifest.capabilities)?;
        if initialized.module_id != entry.manifest.id {
            process.shutdown();
            return Err(CoreError::Protocol(format!(
                "модуль представился как {}, ожидался {}",
                initialized.module_id, entry.manifest.id
            )));
        }
        let pid = process.pid();
        let host_handler: &mut dyn HostHandler = session;
        let result = process.invoke_cancellable_with_host_params(
            call_id,
            &action.id,
            call_session.context.source,
            params,
            timeout,
            cancel.as_ref(),
            host_handler,
            &entry.manifest.capabilities,
        );
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
                call_session.context.source.as_str(),
                result.status,
                elapsed.as_millis(),
                pid
            ));
        }
        Ok(InvokeOutcome {
            call_id: call_id.to_owned(),
            module_id: entry.manifest.id.clone(),
            module_version: entry.manifest.version.clone(),
            action_id: action.id.clone(),
            action_name: action.name.clone(),
            status: result.status,
            message,
            data: result.data,
            elapsed,
            cold: true,
        })
    }

    fn point_status(
        &self,
        modules: &BTreeMap<String, ModuleEntry>,
        parent: &ModuleEntry,
        point: &ExtensionPoint,
    ) -> ExtensionPointStatus {
        let mut contributions = Vec::new();
        for module in modules.values() {
            for contribution in &module.manifest.provides {
                if contribution.module_id != parent.manifest.id || contribution.point_id != point.id
                {
                    continue;
                }
                let contract_ok = contribution.contract == point.contract;
                let interface_ok =
                    interface_matches(&contribution.interface_version, &point.contract_version);
                let compatible = contract_ok && interface_ok;
                let available = compatible && module.enabled;
                let reason = if !contract_ok {
                    Some("другой контракт".to_owned())
                } else if !interface_ok {
                    Some("несовместимая версия контракта".to_owned())
                } else if !module.enabled {
                    Some("пакет отключён".to_owned())
                } else {
                    None
                };
                contributions.push(ExtensionContributionStatus {
                    module_id: module.manifest.id.clone(),
                    name: contribution.name.clone(),
                    description: contribution.description.clone(),
                    enabled: module.enabled,
                    available,
                    reason,
                });
            }
        }
        contributions.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.module_id.cmp(&right.module_id))
        });
        let key = selection_key(&parent.manifest.id, &point.id);
        let saved = self
            .selections
            .lock()
            .expect("выбор расширений")
            .get(&key)
            .cloned();
        let mut selected = saved.filter(|module_id| {
            contributions
                .iter()
                .any(|item| item.available && &item.module_id == module_id)
        });
        if selected.is_none() {
            let available: Vec<_> = contributions.iter().filter(|item| item.available).collect();
            if available.len() == 1 {
                selected = Some(available[0].module_id.clone());
            }
        }
        let needs_selection = selected.is_none();
        ExtensionPointStatus {
            module_id: parent.manifest.id.clone(),
            point_id: point.id.clone(),
            name: point.name.clone(),
            contract: point.contract.clone(),
            contract_version: point.contract_version.clone(),
            required: point.required,
            selected,
            needs_selection,
            contributions,
        }
    }

    fn persist_selections(&self, selections: &BTreeMap<String, String>) -> Result<(), CoreError> {
        let Some(path) = self.selection_path.as_ref() else {
            return Ok(());
        };
        let document = ExtensionSelectionDocument {
            selected: selections.clone(),
        };
        let contents = serde_json::to_vec_pretty(&document)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, contents)?;
        if let Err(first) = fs::rename(&temporary, path) {
            let _ = fs::remove_file(path);
            fs::rename(&temporary, path).map_err(|_| {
                CoreError::Io(std::io::Error::other(format!(
                    "не удалось сохранить выбор расширений: {first}"
                )))
            })?;
        }
        Ok(())
    }

    fn persist_disabled(&self, disabled: &BTreeSet<String>) -> Result<(), CoreError> {
        let Some(path) = self.state_path.as_ref() else {
            return Ok(());
        };
        let document = DisabledDocument {
            disabled: disabled.iter().cloned().collect(),
        };
        let contents = serde_json::to_vec_pretty(&document)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, contents)?;
        if let Err(first) = fs::rename(&temporary, path) {
            let _ = fs::remove_file(path);
            fs::rename(&temporary, path).map_err(|_| {
                CoreError::Io(std::io::Error::other(format!(
                    "не удалось сохранить отключённые пакеты: {first}"
                )))
            })?;
        }
        Ok(())
    }
}

struct PlainSession<'a> {
    host: &'a mut dyn HostHandler,
}

impl HostHandler for PlainSession<'_> {
    fn handle(
        &mut self,
        context: &HostCallContext,
        call: &util_maker_sdk::HostCall,
    ) -> Result<Value, util_maker_sdk::RpcError> {
        self.host.handle(context, call)
    }
}

impl HostSession for PlainSession<'_> {
    fn begin_call(&mut self, _context: &CallSession) {}
    fn finish_call(&mut self, _call_id: &str) {}
}

fn selection_key(module_id: &str, point_id: &str) -> String {
    format!("{module_id}/{point_id}")
}

fn interface_matches(requirement: &str, version: &str) -> bool {
    let normalized = requirement
        .replace(',', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(", ");
    let Ok(requirement) = semver::VersionReq::parse(&normalized) else {
        return false;
    };
    let Ok(version) = Version::parse(version) else {
        return false;
    };
    requirement.matches(&version)
}

fn extension_error(code: i64, message: impl Into<String>, plugin: Option<&str>) -> RpcError {
    let error = RpcError::new(code, message);
    match plugin {
        Some(plugin) => error.with_data(serde_json::json!({ "pluginId": plugin })),
        None => error,
    }
}

fn map_extension_error(error: CoreError, plugin: &str) -> RpcError {
    match error {
        CoreError::Cancelled { .. } => extension_error(
            error_codes::CANCELLED,
            format!("вызов плагина {plugin} отменён"),
            Some(plugin),
        ),
        CoreError::Timeout { timeout, .. } => extension_error(
            error_codes::EXTENSION_FAILED,
            format!("плагин {plugin} не ответил за {} мс", timeout.as_millis()),
            Some(plugin),
        ),
        other => extension_error(
            error_codes::EXTENSION_FAILED,
            format!("плагин {plugin}: {other}"),
            Some(plugin),
        ),
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ExtensionSelectionDocument {
    #[serde(default)]
    selected: BTreeMap<String, String>,
}

fn load_selections(path: &Path) -> Result<BTreeMap<String, String>, CoreError> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(CoreError::Io(error)),
    };
    let document = serde_json::from_str::<ExtensionSelectionDocument>(&contents)?;
    Ok(document.selected)
}

fn validate_entry(entry: &ModuleEntry) -> Result<(), CoreError> {
    entry.manifest.validate()?;
    let host = Version::parse(HOST_API_VERSION)
        .map_err(|error| CoreError::IncompatibleApi(error.to_string()))?;
    if !entry.manifest.supports_host_api(&host)? {
        return Err(CoreError::IncompatibleApi(format!(
            "модуль {} требует hostApi {}, хост предлагает {}",
            entry.manifest.id, entry.manifest.host_api, host
        )));
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
struct DisabledDocument {
    #[serde(default)]
    disabled: Vec<String>,
}

fn load_disabled(path: &Path) -> Result<BTreeSet<String>, CoreError> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(CoreError::Io(error)),
    };
    let document = serde_json::from_str::<DisabledDocument>(&contents)?;
    Ok(document.disabled.into_iter().collect())
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
