#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Нативный хост прототипа Util Maker.
//!
//! В трее живёт один экземпляр приложения. Глобальное сочетание или пункт меню
//! запускают контрольный модуль отдельным процессом, согласуют API по JSON-RPC
//! и показывают результат в компактном окне. Настройки создаются по запросу и
//! уничтожаются при закрытии; без окон процесс остаётся в трее.

mod editor_placement;
mod extensions;
mod logging;
mod package_manager;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use package_manager::PackageManager;
use serde::{Deserialize, Serialize};
use tauri::{
    image::Image,
    menu::{IconMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder,
};
use util_maker_color_picker::{
    Arrow as PickerArrow, BlockReason, ClipboardError as PickerClipboardError, ClipboardSink,
    ColorFormat, CopyOutcome, FrameData, PickerPhase, PickerSession, Point as PickerPoint,
    SampleError,
};
use util_maker_core::diagnostics::{DiagnosticsLog, LogEntry};
use util_maker_core::hotkeys::{
    AssignmentRecord, Candidate, CheckResult, Confirmation, HotkeyPlatform, HotkeyRegistry,
    HotkeyStore, Owner, RegistrationError, StoreError,
};
use util_maker_core::ipc::ModuleProcess;
use util_maker_core::{
    ActionRef, CallSession, HostDispatcher, HostSession, InvocationContext, InvokeOutcome,
    ModuleEntry, Registry,
};
use util_maker_platform::{
    clipboard, screen_service, selection_service, FrozenFrame, HotkeyEvent, HotkeyService,
    NativeHost, NativeSelection, RegisterFailure, ScreenSource, SettingsStore,
};
use util_maker_sdk::{
    protocol::InvocationSource, Action, FormCommandRequest, FormCommandResult, FrameRegion,
    Manifest, MonitorGeometry, SettingsSchema,
};

const CONTROL_MODULE_ID: &str = "org.utilmaker.control";
const CONTROL_ACTION_ID: &str = "ping";
const CONTROL_SHORTCUT: &str = "Ctrl+Shift+U";
const SCREENSHOT_MODULE_ID: &str = "org.utilmaker.screenshot";
const SCREENSHOT_SAVE_DIRECTORY: &str = "saveDirectory";
const QR_MODULE_ID: &str = "org.utilmaker.qr-code";
const QR_READ_ACTION: &str = "read-qr-region";
const COLOR_PICKER_MODULE_ID: &str = "org.utilmaker.color-picker";
const COLOR_PICKER_ACTION_ID: &str = "pick-color";
const PICKER_WINDOW: &str = "picker";
/// Радиус окна лупы и увеличение: 15 физических пикселей без сглаживания.
const PICKER_LOUPE_RADIUS: u32 = 7;
const PICKER_LOUPE_ZOOM: u32 = 12;
const SETTINGS_WINDOW: &str = "settings";
const RESULT_WINDOW: &str = "result";
const INVOKE_TIMEOUT: Duration = Duration::from_secs(30);
const HOTKEY_ID: u32 = 1;

struct AppState {
    last_result: Mutex<Option<LastResult>>,
    catalog: CatalogState,
    tray_actions: Mutex<BTreeMap<String, ActionRef>>,
    metrics: Mutex<DiagnosticsLog>,
    packages: Mutex<PackageManager>,
    overlay: Mutex<Option<OverlayFrame>>,
    overlay_source: Mutex<Option<InvocationSource>>,
    /// Открытый выбор пикселя: снимок в памяти и состояние лупы.
    picker: Mutex<Option<PickerSession>>,
    settings: Arc<SettingsStore>,
}

struct OverlayFrame {
    origin_x: i32,
    origin_y: i32,
    width: u32,
    height: u32,
    monitors: Vec<MonitorGeometry>,
    data: Vec<u8>,
}

/// Каталог установленных модулей и реестр ядра.
struct CatalogState {
    root: PathBuf,
    registry: Arc<Registry>,
    errors: Mutex<Vec<String>>,
}

impl CatalogState {
    fn open(app: &AppHandle) -> Result<Self, String> {
        let root = module_catalog_directory(app)?;
        let data_directory = app
            .path()
            .app_local_data_dir()
            .map_err(|error| format!("каталог данных приложения: {error}"))?;
        std::fs::create_dir_all(&data_directory)
            .map_err(|error| format!("каталог {}: {error}", data_directory.display()))?;
        let registry = Arc::new(
            Registry::with_state_path(data_directory.join("packages.json"))
                .map_err(|error| format!("состояние пакетов: {error}"))?,
        );
        let state = Self {
            root,
            registry,
            errors: Mutex::new(Vec::new()),
        };
        state.refresh();
        Ok(state)
    }

    /// Перечитывает манифесты с диска. Ошибки отдельных модулей не скрывают
    /// остальные установленные пакеты.
    fn refresh(&self) -> Vec<String> {
        match discover_modules(&self.root, platform_key()) {
            Ok((entries, errors)) => {
                if let Err(error) = self.registry.replace_packages(entries) {
                    let mut all = errors;
                    all.push(format!("каталог модулей: {error}"));
                    *self.errors.lock().expect("ошибки каталога") = all.clone();
                    return all;
                }
                *self.errors.lock().expect("ошибки каталога") = errors.clone();
                errors
            }
            Err(error) => {
                let errors = vec![error];
                *self.errors.lock().expect("ошибки каталога") = errors.clone();
                errors
            }
        }
    }
}

/// Каталог рядом с исполняемым файлом либо установленные ресурсы.
fn module_catalog_directory(app: &AppHandle) -> Result<PathBuf, String> {
    if let Some(directory) = std::env::var_os("UTIL_MAKER_MODULES_DIR") {
        return Ok(PathBuf::from(directory));
    }
    let executable =
        std::env::current_exe().map_err(|error| format!("путь приложения: {error}"))?;
    let beside_exe = executable
        .parent()
        .ok_or_else(|| "у приложения нет каталога".to_owned())?
        .join("modules");
    if beside_exe.is_dir() {
        return Ok(beside_exe);
    }
    let resources = app
        .path()
        .resource_dir()
        .map_err(|error| format!("каталог ресурсов: {error}"))?;
    Ok(resources.join("modules"))
}

/// Читает активные манифесты и проверяет точки входа, не запуская процессы.
fn discover_modules(
    root: &Path,
    platform: &str,
) -> Result<(Vec<ModuleEntry>, Vec<String>), String> {
    let entries = std::fs::read_dir(root)
        .map_err(|error| format!("каталог модулей {}: {error}", root.display()))?;
    let mut modules = Vec::new();
    let mut errors = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                errors.push(format!("элемент каталога {}: {error}", root.display()));
                continue;
            }
        };
        let directory = entry.path();
        if !directory.is_dir() {
            continue;
        }
        let manifest_path = directory.join("manifest.json");
        if !manifest_path.is_file() {
            continue;
        }
        let manifest = match Manifest::load(&manifest_path) {
            Ok(manifest) => manifest,
            Err(error) => {
                errors.push(format!("{}: {error}", manifest_path.display()));
                continue;
            }
        };
        let entrypoint = match manifest.resolved_entrypoint(platform) {
            Ok(entrypoint) => entrypoint,
            Err(error) => {
                errors.push(format!(
                    "{}: точка входа {platform}: {error}",
                    manifest_path.display()
                ));
                continue;
            }
        };
        let executable = directory.join(entrypoint);
        if !executable.is_file() {
            errors.push(format!(
                "{}: не найден исполняемый файл {}",
                manifest_path.display(),
                executable.display()
            ));
            continue;
        }
        modules.push(ModuleEntry::new(manifest, executable, true));
    }
    modules.sort_by(|left, right| left.manifest.id.cmp(&right.manifest.id));
    Ok((modules, errors))
}

// --- Демонстрационный реестр шорткеев для редактора настроек ---

#[derive(Default)]
struct DemoStore {
    records: Vec<AssignmentRecord>,
    fail_saves: bool,
}

impl HotkeyStore for DemoStore {
    fn load(&self) -> Result<Vec<AssignmentRecord>, StoreError> {
        Ok(self.records.clone())
    }

    fn save(&mut self, records: &[AssignmentRecord]) -> Result<(), StoreError> {
        if self.fail_saves {
            return Err(StoreError("имитация отказа записи".to_owned()));
        }
        self.records = records.to_vec();
        Ok(())
    }
}

#[derive(Default)]
struct DemoPlatform {
    registered: BTreeSet<String>,
}

impl HotkeyPlatform for DemoPlatform {
    fn register(
        &mut self,
        hotkey: &util_maker_core::hotkeys::Hotkey,
    ) -> Result<(), RegistrationError> {
        self.registered.insert(hotkey.canonical());
        Ok(())
    }

    fn unregister(&mut self, hotkey: &util_maker_core::hotkeys::Hotkey) {
        self.registered.remove(&hotkey.canonical());
    }
}

struct HotkeyDemo {
    registry: Mutex<HotkeyRegistry<DemoStore, DemoPlatform>>,
}

impl HotkeyDemo {
    fn new() -> Self {
        let mut registry = HotkeyRegistry::open(DemoStore::default(), DemoPlatform::default())
            .expect("демонстрационный реестр шорткеев");
        for (combo, module, action) in [
            ("Ctrl+Shift+S", "org.utilmaker.screenshot", "capture-region"),
            ("Ctrl+Shift+2", "org.utilmaker.translator", "translate"),
        ] {
            if let Some((module_name, action_name)) = demo_target(module, action) {
                let candidate =
                    Candidate::new(combo, module, module_name, action, action_name, true);
                let _ = registry.assign(&candidate, None);
            }
        }
        Self {
            registry: Mutex::new(registry),
        }
    }
}

fn demo_target(module: &str, action: &str) -> Option<(&'static str, &'static str)> {
    match (module, action) {
        ("org.utilmaker.screenshot", "capture-region") => Some(("Скриншоты", "Снять область")),
        ("org.utilmaker.translator", "translate") => Some(("Переводчик", "Перевести текст")),
        _ => None,
    }
}

fn default_true() -> bool {
    true
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct HotkeyOwnerDto {
    module_id: String,
    module_name: String,
    action_id: String,
    action_name: String,
    enabled: bool,
    #[serde(default = "default_true")]
    available: bool,
    #[serde(default)]
    registration_error: Option<String>,
}

impl From<&Owner> for HotkeyOwnerDto {
    fn from(owner: &Owner) -> Self {
        Self {
            module_id: owner.module_id.clone(),
            module_name: owner.module_name.clone(),
            action_id: owner.action_id.clone(),
            action_name: owner.action_name.clone(),
            enabled: owner.enabled,
            available: owner.available,
            registration_error: owner.registration_error.clone(),
        }
    }
}

impl From<HotkeyOwnerDto> for Owner {
    fn from(owner: HotkeyOwnerDto) -> Self {
        Self {
            module_id: owner.module_id,
            module_name: owner.module_name,
            action_id: owner.action_id,
            action_name: owner.action_name,
            enabled: owner.enabled,
            available: owner.available,
            registration_error: owner.registration_error,
        }
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct HotkeyStateDto {
    combo: String,
    module_id: String,
    module_name: String,
    action_id: String,
    action_name: String,
    enabled: bool,
}

#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum HotkeyCheckDto {
    Available,
    SameAssignment,
    InternalConflict {
        previous: HotkeyOwnerDto,
        proposed: Box<HotkeyOwnerDto>,
    },
    SystemReserved {
        combo: String,
        function: String,
    },
    RegistrationRejected {
        combo: String,
        occupied: bool,
    },
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct HotkeyConfirmationDto {
    combo: String,
    expected_owner: HotkeyOwnerDto,
}

fn hotkey_state(state: &HotkeyDemo) -> Vec<HotkeyStateDto> {
    let registry = state.registry.lock().expect("реестр шорткеев");
    registry
        .records()
        .map(|record| HotkeyStateDto {
            combo: record.combo.clone(),
            module_id: record.module_id.clone(),
            module_name: record.module_name.clone(),
            action_id: record.action_id.clone(),
            action_name: record.action_name.clone(),
            enabled: record.enabled,
        })
        .collect()
}

fn candidate_for(combo: &str, module: &str, action: &str) -> Candidate {
    let (module_name, action_name) = demo_target(module, action).unwrap_or(("Модуль", "Действие"));
    Candidate::new(combo, module, module_name, action, action_name, true)
}

fn check_dto(result: CheckResult) -> HotkeyCheckDto {
    match result {
        CheckResult::Available => HotkeyCheckDto::Available,
        CheckResult::SameAssignment => HotkeyCheckDto::SameAssignment,
        CheckResult::InternalConflict { previous, proposed } => HotkeyCheckDto::InternalConflict {
            previous: HotkeyOwnerDto::from(&previous),
            proposed: Box::new(HotkeyOwnerDto::from(proposed.as_ref())),
        },
        CheckResult::SystemReserved { combo, function } => {
            HotkeyCheckDto::SystemReserved { combo, function }
        }
        CheckResult::RegistrationRejected { combo, occupied } => {
            HotkeyCheckDto::RegistrationRejected { combo, occupied }
        }
    }
}

#[derive(Clone)]
struct Registration {
    combo: String,
    target: ActionRef,
}

/// Регистрация прототипа: одно сочетание, пауза и повторная попытка.
struct ShortcutRuntime {
    service: Mutex<HotkeyService>,
    combo: Mutex<String>,
    paused: AtomicBool,
    registered: AtomicBool,
    status: Mutex<String>,
    registrations: Mutex<BTreeMap<u32, Registration>>,
    next_id: AtomicU32,
}

impl ShortcutRuntime {
    fn start(app: &AppHandle) -> Result<(Self, Receiver<HotkeyEvent>), String> {
        let mut service = HotkeyService::start()?;
        let events = service.take_events();
        let persisted = persisted_state(app);
        let combo = persisted
            .shortcut
            .unwrap_or_else(|| CONTROL_SHORTCUT.to_owned());
        let paused = persisted.paused;
        let (registered, status) = if paused {
            (false, "на паузе".to_owned())
        } else {
            register_shortcut(&service, &combo)
        };
        let registrations = BTreeMap::from([(
            HOTKEY_ID,
            Registration {
                combo: combo.clone(),
                target: control_action_ref(),
            },
        )]);
        Ok((
            Self {
                service: Mutex::new(service),
                combo: Mutex::new(combo),
                paused: AtomicBool::new(paused),
                registered: AtomicBool::new(registered),
                status: Mutex::new(status),
                registrations: Mutex::new(registrations),
                next_id: AtomicU32::new(2),
            },
            events,
        ))
    }
}

impl ShortcutRuntime {
    fn target_for(&self, id: u32) -> Option<ActionRef> {
        self.registrations
            .lock()
            .ok()
            .and_then(|registrations| registrations.get(&id).map(|entry| entry.target.clone()))
    }

    fn unregister_all(&self) {
        if let Ok(registrations) = self.registrations.lock() {
            let service = self.service.lock().expect("служба сочетаний");
            for id in registrations.keys() {
                service.unregister(*id);
            }
        }
    }

    /// Повторно регистрирует все сочетания. Возвращает `true`, только если
    /// зарегистрированы все, включая основное сочетание контрольного модуля.
    fn register_all(&self) -> bool {
        let Ok(registrations) = self.registrations.lock() else {
            return false;
        };
        let service = self.service.lock().expect("служба сочетаний");
        let mut all_ok = true;
        for (id, registration) in registrations.iter() {
            if let Err(error) = service.register(&registration.combo, *id) {
                logging::write(&format!(
                    "сочетание {} для {} / {} не зарегистрировано: {}",
                    registration.combo,
                    registration.target.module_id,
                    registration.target.action_id,
                    register_failure_message(error)
                ));
                all_ok = false;
            }
        }
        if !registrations.contains_key(&HOTKEY_ID) {
            let combo = self.combo.lock().expect("сочетание").clone();
            if let Err(error) = service.register(&combo, HOTKEY_ID) {
                logging::write(&format!(
                    "основное сочетание {combo} не зарегистрировано: {}",
                    register_failure_message(error)
                ));
                all_ok = false;
            }
        }
        all_ok
    }

    fn remove_module(&self, module_id: &str) {
        let ids: Vec<u32> = {
            let Ok(mut registrations) = self.registrations.lock() else {
                return;
            };
            let ids = registrations
                .iter()
                .filter(|(_, entry)| entry.target.module_id == module_id)
                .map(|(id, _)| *id)
                .collect();
            registrations.retain(|_, entry| entry.target.module_id != module_id);
            ids
        };
        let service = self.service.lock().expect("служба сочетаний");
        for id in ids {
            service.unregister(id);
        }
    }
}

fn shortcut_map_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_local_data_dir()
        .ok()
        .map(|directory| directory.join("shortcuts.json"))
}

fn persisted_shortcut_map(app: &AppHandle) -> BTreeMap<String, Vec<String>> {
    let Some(path) = shortcut_map_path(app) else {
        return BTreeMap::new();
    };
    let Some(bytes) = std::fs::read(path).ok() else {
        return BTreeMap::new();
    };
    if let Ok(map) = serde_json::from_slice::<BTreeMap<String, Vec<String>>>(&bytes) {
        return map;
    }
    if let Ok(old) = serde_json::from_slice::<BTreeMap<String, String>>(&bytes) {
        return old
            .into_iter()
            .map(|(key, combo)| (key, vec![combo]))
            .collect();
    }
    BTreeMap::new()
}

fn write_shortcut_map(app: &AppHandle, map: &BTreeMap<String, Vec<String>>) {
    let Some(path) = shortcut_map_path(app) else {
        return;
    };
    if let Ok(bytes) = serde_json::to_vec_pretty(map) {
        let _ = std::fs::write(path, bytes);
    }
}

fn save_action_shortcut(app: &AppHandle, module_id: &str, action_id: &str, combo: &str) {
    let mut map = persisted_shortcut_map(app);
    let entry = map.entry(format!("{module_id}/{action_id}")).or_default();
    if !entry.iter().any(|value| value == combo) {
        entry.push(combo.to_owned());
    }
    write_shortcut_map(app, &map);
}

fn remove_saved_shortcut(app: &AppHandle, module_id: &str, action_id: &str, combo: &str) {
    let mut map = persisted_shortcut_map(app);
    let key = format!("{module_id}/{action_id}");
    if let Some(entry) = map.get_mut(&key) {
        entry.retain(|value| value != combo);
        if entry.is_empty() {
            map.remove(&key);
        }
    }
    write_shortcut_map(app, &map);
}

/// Убирает из профиля сохранённые сочетания удалённого пакета. Без этого
/// назначения прежнего модуля возвращались после повторной установки.
fn drop_module_shortcuts(map: &mut BTreeMap<String, Vec<String>>, module_id: &str) {
    let prefix = format!("{module_id}/");
    map.retain(|key, _| !key.starts_with(&prefix));
}

fn remove_saved_shortcuts_for_module(app: &AppHandle, module_id: &str) {
    let mut map = persisted_shortcut_map(app);
    let before = map.len();
    drop_module_shortcuts(&mut map, module_id);
    if map.len() != before {
        write_shortcut_map(app, &map);
    }
}

fn sync_action_hotkeys(app: &AppHandle) {
    let Some(runtime) = app.try_state::<ShortcutRuntime>() else {
        return;
    };
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let actions = state.catalog.registry.actions();
    let persisted = persisted_shortcut_map(app);
    let existing: BTreeSet<(String, String, String)> = runtime
        .registrations
        .lock()
        .map(|registrations| {
            registrations
                .values()
                .map(|entry| {
                    (
                        entry.target.module_id.clone(),
                        entry.target.action_id.clone(),
                        entry.combo.clone(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    for action in actions {
        if action.module_id == CONTROL_MODULE_ID {
            continue;
        }
        let key = format!("{}/{}", action.module_id, action.action_id);
        let combos = persisted.get(&key).cloned().unwrap_or_else(|| {
            state
                .catalog
                .registry
                .module(&action.module_id)
                .and_then(|entry| {
                    entry
                        .manifest
                        .action(&action.action_id)
                        .map(|manifest_action| manifest_action.recommended_hotkeys.clone())
                })
                .unwrap_or_default()
        });
        for combo in combos {
            if existing.contains(&(
                action.module_id.clone(),
                action.action_id.clone(),
                combo.clone(),
            )) {
                continue;
            }
            let id = runtime.next_id.fetch_add(1, Ordering::SeqCst);
            if runtime.paused.load(Ordering::SeqCst) {
                if let Ok(mut registrations) = runtime.registrations.lock() {
                    registrations.insert(
                        id,
                        Registration {
                            combo: combo.clone(),
                            target: ActionRef::new(
                                action.module_id.clone(),
                                action.action_id.clone(),
                            ),
                        },
                    );
                }
                logging::write(&format!(
                    "сочетание {combo}: {} / {} (на паузе)",
                    action.module_id, action.action_id
                ));
                continue;
            }
            let registered = {
                let service = runtime.service.lock().expect("служба сочетаний");
                service.register(&combo, id).is_ok()
            };
            if registered {
                if let Ok(mut registrations) = runtime.registrations.lock() {
                    registrations.insert(
                        id,
                        Registration {
                            combo: combo.clone(),
                            target: ActionRef::new(
                                action.module_id.clone(),
                                action.action_id.clone(),
                            ),
                        },
                    );
                }
                logging::write(&format!(
                    "сочетание {combo}: {} / {}",
                    action.module_id, action.action_id
                ));
            } else {
                logging::write(&format!(
                    "сочетание {combo} для {} / {} не зарегистрировано",
                    action.module_id, action.action_id
                ));
            }
        }
    }
}

fn register_shortcut(service: &HotkeyService, combo: &str) -> (bool, String) {
    match service.register(combo, HOTKEY_ID) {
        Ok(()) => (true, "зарегистрировано".to_owned()),
        Err(RegisterFailure::SystemReserved { function }) => {
            (false, format!("недоступно: {function}"))
        }
        Err(RegisterFailure::Occupied) => (false, "занято другой программой".to_owned()),
        Err(RegisterFailure::Unavailable(error)) => (false, format!("ошибка: {error}")),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LastResult {
    message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
    module_id: String,
    action_id: String,
    action_name: String,
    source: String,
    core_elapsed_ms: u128,
    since_event_ms: u128,
    status: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusPayload {
    module_id: String,
    module_name: String,
    module_version: String,
    action_id: String,
    action_name: String,
    shortcut: String,
    shortcut_status: String,
    app_version: String,
    build_kind: String,
    module_path: String,
    log_path: String,
    config_path: String,
}

fn main() {
    logging::init();
    logging::write("запуск Util Maker (прототип)");
    logging::debug("инициализация приложения");
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            logging::write("повторный запуск: показываю настройки существующего экземпляра");
            open_settings(app);
        }))
        .invoke_handler(tauri::generate_handler![
            get_status,
            invoke_control,
            invoke_plugin_action,
            encode_qr_text,
            refresh_plugins,
            get_last_result,
            open_settings_command,
            close_settings,
            close_result,
            copy_text,
            get_overlay_frame,
            get_overlay_geometry,
            get_editor_image,
            save_editor_image,
            open_screenshot_editor,
            qr_read_region,
            retry_qr_read,
            copy_overlay_image,
            close_overlay,
            get_picker_geometry,
            get_picker_frame,
            picker_set_point,
            picker_arrow,
            picker_copy,
            picker_cancel,
            get_autostart,
            set_autostart,
            hotkey_state_command,
            check_hotkey,
            assign_hotkey,
            set_hotkey_save_failure,
            plugins,
            extension_points,
            select_extension,
            extension_packages,
            plugin_settings,
            save_plugin_settings,
            invoke_form_command,
            plugin_secret_present,
            set_plugin_secret,
            preview_package,
            install_package,
            rollback_package,
            remove_package,
            set_plugin_enabled,
            change_action_shortcut,
            add_action_shortcut,
            move_action_shortcut,
            remove_action_shortcut,
            reset_action_shortcut,
            change_control_shortcut,
            quit_app
        ])
        .setup(|app| {
            let metrics = DiagnosticsLog::open(&logging::log_directory(), 1_000_000)
                .map_err(|error| format!("журнал метрик: {error}"))?;
            let catalog = CatalogState::open(app.handle())?;
            let data_directory = app
                .path()
                .app_local_data_dir()
                .map_err(|error| format!("каталог данных: {error}"))?;
            let packages = PackageManager::open(&data_directory, catalog.root.clone())?;
            let settings_root = data_directory.join("module-settings");
            std::fs::create_dir_all(&settings_root)
                .map_err(|error| format!("каталог настроек: {error}"))?;
            let state = AppState {
                last_result: Mutex::new(None),
                catalog,
                tray_actions: Mutex::new(BTreeMap::new()),
                metrics: Mutex::new(metrics),
                packages: Mutex::new(packages),
                overlay: Mutex::new(None),
                overlay_source: Mutex::new(None),
                picker: Mutex::new(None),
                settings: Arc::new(SettingsStore::new(settings_root)),
            };
            app.manage(state);
            let (runtime, events) = ShortcutRuntime::start(app.handle())?;
            let status = runtime.status.lock().expect("статус сочетания").clone();
            app.manage(runtime);
            app.manage(HotkeyDemo::new());
            setup_tray(app)?;
            if let Err(error) = bootstrap_bundled_packages(app.handle()) {
                logging::write(&format!("пакетная поставка: {error}"));
            }
            if let Some(state) = app.try_state::<AppState>() {
                if let Err(error) =
                    extensions::migrate_translator_profile(&state.catalog.registry, &state.settings)
                {
                    logging::write(&format!("миграция плагинов переводчика: {error}"));
                }
            }
            set_shortcut_status(app.handle(), &status);
            sync_action_hotkeys(app.handle());
            if app
                .try_state::<AppState>()
                .and_then(|state| state.catalog.registry.module(CONTROL_MODULE_ID))
                .is_some_and(|entry| !entry.enabled)
            {
                if let Some(runtime) = app.try_state::<ShortcutRuntime>() {
                    runtime.unregister_all();
                    runtime.registered.store(false, Ordering::SeqCst);
                    runtime.paused.store(true, Ordering::SeqCst);
                }
                set_shortcut_status(app.handle(), "отключён");
                refresh_tray_menu(app.handle());
            }
            spawn_hotkey_listener(app.handle().clone(), events);
            spawn_session_watchdog(app.handle().clone());
            let open_settings_after_install = std::env::args().any(|arg| arg == "--open-settings");
            let first_run = mark_and_check_first_run(app.handle())?;
            if first_run || open_settings_after_install {
                logging::write(if first_run {
                    "первый запуск: открываю настройки"
                } else {
                    "запуск после установки: открываю настройки"
                });
                open_settings(app.handle());
            } else {
                logging::write("повторный запуск: работаю только в трее");
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("не удалось собрать приложение Util Maker");

    app.run(|_app_handle, event| {
        // Закрытие последнего окна не должно завершать резидентный хост.
        // Программный выход (меню трея, команда «Выход») приходит с кодом Some
        // и проходит без задержки.
        if let tauri::RunEvent::ExitRequested {
            code: None, api, ..
        } = event
        {
            logging::write("закрыто последнее окно: приложение остаётся в трее");
            api.prevent_exit();
        }
    });
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct BundledPublicKey {
    publisher: String,
    public_key: String,
}

/// Устанавливает подписанные пакеты, поставленные вместе с установщиком.
fn bootstrap_bundled_packages(app: &AppHandle) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let Some(directory) = executable.parent().map(|parent| parent.join("packages")) else {
        return Ok(());
    };
    if !directory.is_dir() {
        return Ok(());
    }
    let data_directory = app
        .path()
        .app_local_data_dir()
        .map_err(|error| error.to_string())?;
    let marker = data_directory.join("bundled-packages-v1");
    let key_path = directory.join("org.utilmaker-public.json");
    let key: BundledPublicKey = serde_json::from_slice(
        &std::fs::read(&key_path).map_err(|error| format!("ключ поставки: {error}"))?,
    )
    .map_err(|error| format!("ключ поставки: {error}"))?;
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "каталог модулей не готов".to_owned())?;
    let mut manager = state.packages.lock().map_err(|error| error.to_string())?;
    let mut latest: BTreeMap<String, (semver::Version, std::path::PathBuf)> = BTreeMap::new();
    for entry in std::fs::read_dir(&directory).map_err(|error| error.to_string())? {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("umpkg") {
            continue;
        }
        match manager.preview(&path, &key.publisher, &key.public_key) {
            Ok(preview) => {
                let version = semver::Version::parse(&preview.version)
                    .map_err(|error| format!("версия {}: {error}", preview.version))?;
                let replace = latest
                    .get(&preview.id)
                    .is_none_or(|(current, _)| version > *current);
                if replace {
                    latest.insert(preview.id, (version, path));
                }
            }
            Err(error) => logging::write(&format!("пакет поставки {}: {error}", path.display())),
        }
    }
    let mut installed = Vec::new();
    for (_, (_, path)) in latest {
        match manager.install(&path, &key.publisher, &key.public_key) {
            Ok(module) => installed.push(format!("{} {}", module.id, module.version)),
            Err(error) => logging::write(&format!("пакет поставки {}: {error}", path.display())),
        }
    }
    drop(manager);
    if !installed.is_empty() {
        state.catalog.refresh();
        let _ = refresh_plugins_inner(app);
        let _ = std::fs::write(
            marker,
            serde_json::to_vec_pretty(&installed).unwrap_or_default(),
        );
    }
    Ok(())
}

fn setup_tray(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let (icon_bytes, tooltip) = app
        .try_state::<ShortcutRuntime>()
        .map(|runtime| {
            tray_visual(
                runtime.paused.load(Ordering::SeqCst),
                runtime.registered.load(Ordering::SeqCst),
                &runtime.status.lock().expect("статус сочетания"),
            )
        })
        .unwrap_or_else(|| {
            (
                include_bytes!("../icons/tray-working.png").as_slice(),
                "Util Maker — состояние неизвестно".to_owned(),
            )
        });
    let icon = Image::from_bytes(icon_bytes)?;

    TrayIconBuilder::with_id("main")
        .icon(icon)
        .tooltip(tooltip)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "settings" => {
                logging::write("меню трея: настройки");
                open_settings(app);
            }
            "pause" => toggle_shortcut_pause(app),
            "retry" => retry_shortcut(app),
            "quit" => quit_application(app),
            id => {
                let target = app.try_state::<AppState>().and_then(|state| {
                    state
                        .tray_actions
                        .lock()
                        .ok()
                        .and_then(|actions| actions.get(id).cloned())
                });
                if let Some(target) = target {
                    logging::write(&format!(
                        "меню трея: {} / {}",
                        target.module_id, target.action_id
                    ));
                    invoke_and_show(app, target, InvocationSource::Tray);
                }
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                open_settings(tray.app_handle());
            }
        })
        .build(app)?;
    rebuild_tray_menu(app.handle()).map_err(std::io::Error::other)?;
    Ok(())
}

/// Пересобирает меню трея из активных манифестов. Сами процессы не
/// запускаются: источником служит снимок каталога в реестре.
fn embedded_menu_icon(bytes: &'static [u8]) -> Option<Image<'static>> {
    Image::from_bytes(bytes).ok()
}

/// Значок служебного пункта меню: шестерёнка настроек, пауза,
/// перерегистрация и выход. Логотип приложения остаётся только у самого
/// хоста и контрольного модуля.
fn service_menu_icon(id: &str) -> Option<Image<'static>> {
    let bytes: &'static [u8] = match id {
        "settings" => include_bytes!("../icons/menu-settings.png"),
        "pause" => include_bytes!("../icons/menu-pause.png"),
        "retry" => include_bytes!("../icons/menu-retry.png"),
        "quit" => include_bytes!("../icons/menu-quit.png"),
        _ => return None,
    };
    embedded_menu_icon(bytes)
}

/// Нейтральная заглушка микроприложения без иконки: пункт остаётся
/// доступным по тексту, но не получает логотип приложения.
fn fallback_menu_icon() -> Option<Image<'static>> {
    embedded_menu_icon(include_bytes!("../icons/menu-fallback.png"))
}

fn action_menu_icon(app: &AppHandle, module_id: &str) -> Option<Image<'static>> {
    let state = app.try_state::<AppState>()?;
    let entry = state
        .catalog
        .registry
        .modules()
        .into_iter()
        .find(|entry| entry.manifest.id == module_id)?;
    let icon = entry.manifest.icon.as_ref()?;
    let path = entry.executable.parent()?.join(icon);
    let bytes = std::fs::read(path).ok()?;
    Image::from_bytes(&bytes).ok()
}

fn rebuild_tray_menu(app: &AppHandle) -> Result<(), String> {
    let actions = app
        .try_state::<AppState>()
        .map(|state| state.catalog.registry.actions())
        .unwrap_or_default();
    let paused = shortcut_paused(app);
    let menu = Menu::new(app).map_err(|error| format!("меню трея: {error}"))?;
    let settings = IconMenuItem::with_id(
        app,
        "settings",
        "Настройки",
        true,
        service_menu_icon("settings"),
        None::<&str>,
    )
    .map_err(|error| format!("пункт настроек: {error}"))?;
    menu.append(&settings)
        .map_err(|error| format!("меню трея: {error}"))?;
    menu.append(
        &PredefinedMenuItem::separator(app)
            .map_err(|error| format!("разделитель меню: {error}"))?,
    )
    .map_err(|error| format!("меню трея: {error}"))?;

    let mut mapping = BTreeMap::new();
    if actions.is_empty() {
        let item = MenuItem::with_id(
            app,
            "no-actions",
            "Нет доступных действий",
            false,
            None::<&str>,
        )
        .map_err(|error| format!("пункт меню: {error}"))?;
        menu.append(&item)
            .map_err(|error| format!("меню трея: {error}"))?;
    } else {
        for (index, action) in actions.iter().enumerate() {
            mapping.insert(
                format!("action:{index}"),
                ActionRef::new(action.module_id.clone(), action.action_id.clone()),
            );
            let item = IconMenuItem::with_id(
                app,
                format!("action:{index}"),
                format!("{} — {}", action.module_name, action.action_name),
                action.enabled,
                action_menu_icon(app, &action.module_id).or_else(fallback_menu_icon),
                None::<&str>,
            )
            .map_err(|error| format!("пункт действия: {error}"))?;
            menu.append(&item)
                .map_err(|error| format!("меню трея: {error}"))?;
        }
    }

    menu.append(
        &PredefinedMenuItem::separator(app)
            .map_err(|error| format!("разделитель меню: {error}"))?,
    )
    .map_err(|error| format!("меню трея: {error}"))?;
    for (id, label) in [
        ("pause", pause_menu_label(paused)),
        ("retry", "Перерегистрировать сочетание"),
        ("quit", "Выход"),
    ] {
        let item = IconMenuItem::with_id(app, id, label, true, service_menu_icon(id), None::<&str>)
            .map_err(|error| format!("пункт меню {id}: {error}"))?;
        menu.append(&item)
            .map_err(|error| format!("меню трея: {error}"))?;
    }

    let tray = app
        .tray_by_id("main")
        .ok_or_else(|| "иконка трея не создана".to_owned())?;
    tray.set_menu(Some(menu))
        .map_err(|error| format!("обновление меню трея: {error}"))?;
    if let Some(state) = app.try_state::<AppState>() {
        *state.tray_actions.lock().expect("меню действий") = mapping;
    }
    let errors = app
        .try_state::<AppState>()
        .map(|state| {
            state
                .catalog
                .errors
                .lock()
                .expect("ошибки каталога")
                .clone()
        })
        .unwrap_or_default();
    for error in errors {
        logging::write(&format!("каталог модулей: {error}"));
    }
    Ok(())
}

fn open_settings(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(SETTINGS_WINDOW) {
        let _ = window.unminimize();
        center_on_active_monitor(&window, 700, 620);
        let _ = window.show();
        let _ = window.set_focus();
        #[cfg(windows)]
        if let Ok(hwnd) = window.hwnd() {
            util_maker_platform::focus_window(hwnd.0 as isize);
        }
        return;
    }
    match WebviewWindowBuilder::new(app, SETTINGS_WINDOW, WebviewUrl::App("index.html".into()))
        .title("Util Maker — настройки")
        .inner_size(700.0, 620.0)
        .min_inner_size(560.0, 460.0)
        .visible(false)
        .build()
    {
        Ok(window) => {
            center_on_active_monitor(&window, 700, 620);
            let _ = window.show();
            let _ = window.set_focus();
            logging::write("окно настроек открыто");
            #[cfg(not(windows))]
            let _ = &window;
            #[cfg(windows)]
            if let Ok(hwnd) = window.hwnd() {
                util_maker_platform::focus_window(hwnd.0 as isize);
            }
        }
        Err(error) => logging::write(&format!("не удалось открыть настройки: {error}")),
    }
}

fn center_on_active_monitor(window: &tauri::WebviewWindow, width: u32, height: u32) {
    let cursor = util_maker_platform::cursor_position().unwrap_or((0, 0));
    let geometry = screen_service().geometry().ok();
    let rect = geometry.as_ref().and_then(|geometry| {
        geometry
            .monitors
            .iter()
            .find(|monitor| {
                cursor.0 >= monitor.rect.x
                    && cursor.0 < monitor.rect.x + monitor.rect.width as i32
                    && cursor.1 >= monitor.rect.y
                    && cursor.1 < monitor.rect.y + monitor.rect.height as i32
            })
            .map(|monitor| monitor.rect)
            .or(Some(geometry.virtual_screen))
    });
    if let Some(rect) = rect {
        let x = rect.x + (rect.width as i32 - width as i32) / 2;
        let y = rect.y + (rect.height as i32 - height as i32) / 2;
        let _ = window.set_position(PhysicalPosition::new(x, y));
    }
}

fn activate_qr_result(app: &AppHandle, target: &ActionRef) -> bool {
    if target.module_id != "org.utilmaker.qr-code" || target.action_id != "show-qr" {
        return false;
    }
    let is_qr = app
        .try_state::<AppState>()
        .and_then(|state| {
            state.last_result.lock().ok().map(|slot| {
                slot.as_ref()
                    .is_some_and(|result| result.action_id == "show-qr")
            })
        })
        .unwrap_or(false);
    if !is_qr {
        return false;
    }
    let Some(window) = app.get_webview_window(RESULT_WINDOW) else {
        return false;
    };
    let _ = window.show();
    let _ = window.set_focus();
    true
}

/// Повторный вызов «Прочитать QR с экрана» при открытом успешном
/// предпросмотре возвращает к нему, а не начинает новый выбор.
fn activate_qr_read_result(app: &AppHandle) -> bool {
    let is_read = app
        .try_state::<AppState>()
        .and_then(|state| {
            state.last_result.lock().ok().map(|slot| {
                slot.as_ref().is_some_and(|result| {
                    result.action_id == QR_READ_ACTION && result.status == "ok"
                })
            })
        })
        .unwrap_or(false);
    if !is_read {
        return false;
    }
    let Some(window) = app.get_webview_window(RESULT_WINDOW) else {
        return false;
    };
    let _ = window.show();
    let _ = window.set_focus();
    true
}

fn show_result(app: &AppHandle, result: LastResult) {
    let qr = result.action_id == "show-qr";
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut slot) = state.last_result.lock() {
            *slot = Some(result);
        }
    }
    if let Some(window) = app.get_webview_window(RESULT_WINDOW) {
        if qr {
            let _ = window.set_title("Util Maker — QR-код");
            let _ = window.set_resizable(true);
            let _ = window.set_size(PhysicalSize::new(580, 760));
        }
        let _ = window.eval("window.__umRefresh && window.__umRefresh()");
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    let mut builder =
        WebviewWindowBuilder::new(app, RESULT_WINDOW, WebviewUrl::App("result.html".into()))
            .always_on_top(true);
    builder = if qr {
        builder
            .title("Util Maker — QR-код")
            .inner_size(580.0, 760.0)
            .min_inner_size(380.0, 480.0)
            .resizable(true)
    } else {
        builder
            .title("Util Maker — результат")
            .inner_size(520.0, 320.0)
            .min_inner_size(420.0, 260.0)
            .resizable(false)
            .visible(false)
    };
    match builder.build() {
        Ok(window) => {
            if qr {
                center_on_active_monitor(&window, 580, 760);
            } else {
                center_on_active_monitor(&window, 520, 320);
            }
            let _ = window.show();
            let _ = window.set_focus();
            logging::write("окно результата открыто");
        }
        Err(error) => logging::write(&format!("не удалось показать результат: {error}")),
    }
}

fn control_action_ref() -> ActionRef {
    ActionRef::new(CONTROL_MODULE_ID, CONTROL_ACTION_ID)
}

fn action_name_for(app: &AppHandle, target: &ActionRef) -> String {
    app.try_state::<AppState>()
        .and_then(|state| {
            state
                .catalog
                .registry
                .module(&target.module_id)
                .and_then(|entry| {
                    entry
                        .manifest
                        .action(&target.action_id)
                        .map(|action| action.name.clone())
                })
        })
        .unwrap_or_else(|| target.action_id.clone())
}

fn hide_and_close(app: &AppHandle, label: &str) {
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.hide();
    }
    let app = app.clone();
    let label = label.to_owned();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if let Some(window) = app.get_webview_window(&label) {
            let _ = window.close();
        }
    });
}

/// Формат из настройки «Формат цвета»; без значения — HEX из ТЗ.
fn color_picker_format(settings: &SettingsStore) -> ColorFormat {
    settings
        .values(COLOR_PICKER_MODULE_ID, None)
        .unwrap_or_default()
        .get("format")
        .and_then(serde_json::Value::as_str)
        .and_then(ColorFormat::from_setting)
        .unwrap_or_default()
}

/// Строка в буфер обмена из окна выбора пипетки.
struct PickerClipboard;

impl ClipboardSink for PickerClipboard {
    fn write_text(&mut self, text: &str) -> Result<(), PickerClipboardError> {
        clipboard::write_text(text).map_err(|error| PickerClipboardError::Other(error.to_string()))
    }
}

fn picker_phase_name(phase: PickerPhase) -> &'static str {
    match phase {
        PickerPhase::Idle => "idle",
        PickerPhase::Choosing => "choosing",
        PickerPhase::ClipboardBusy => "clipboard-busy",
        PickerPhase::ClipboardFailed => "clipboard-failed",
        PickerPhase::Completed => "completed",
        PickerPhase::Cancelled => "cancelled",
    }
}

fn picker_sample_reason(error: &SampleError) -> &'static str {
    match error {
        SampleError::OutsideMonitors { .. } => "outside-monitors",
        SampleError::OutsideFrame { .. } => "outside-frame",
        SampleError::OutOfBounds => "out-of-bounds",
    }
}

fn picker_sample_message(error: &SampleError) -> String {
    match error {
        SampleError::OutsideMonitors { .. } => "Точка вне мониторов".to_owned(),
        SampleError::OutsideFrame { .. } => "Точка вне кадра".to_owned(),
        SampleError::OutOfBounds => "Пиксель вне данных кадра".to_owned(),
    }
}

fn picker_block_reason(reason: BlockReason) -> &'static str {
    match reason {
        BlockReason::NoFrame => "no-frame",
        BlockReason::OutsideMonitors => "outside-monitors",
        BlockReason::OutsideFrame => "outside-frame",
        BlockReason::OutOfBounds => "out-of-bounds",
        BlockReason::GeometryChanged => "geometry-changed",
        BlockReason::GeometryUnavailable => "geometry-unavailable",
    }
}

fn picker_block_message(reason: BlockReason) -> &'static str {
    match reason {
        BlockReason::NoFrame => "Кадр уже освобождён — вызовите пипетку снова.",
        BlockReason::OutsideMonitors => "Точка не на мониторе — выберите пиксель экрана.",
        BlockReason::OutsideFrame => "Точка вне снимка — сделайте новый снимок.",
        BlockReason::OutOfBounds => "Пиксель вне данных кадра.",
        BlockReason::GeometryChanged => "Конфигурация дисплеев изменилась — сделайте новый снимок.",
        BlockReason::GeometryUnavailable => "Геометрия мониторов недоступна.",
    }
}

fn picker_loupe_view(session: &PickerSession) -> serde_json::Value {
    let Some(frame) = session.frame() else {
        return serde_json::Value::Null;
    };
    let Ok(loupe) = session.loupe(PICKER_LOUPE_RADIUS, PICKER_LOUPE_ZOOM) else {
        return serde_json::Value::Null;
    };
    let origin = frame.origin();
    let source = loupe.source();
    let crosshair = loupe.crosshair();
    serde_json::json!({
        "x": source.x - origin.x,
        "y": source.y - origin.y,
        "width": source.width,
        "height": source.height,
        "zoom": loupe.zoom(),
        "crosshairX": crosshair.x,
        "crosshairY": crosshair.y,
    })
}

/// Состояние выбора для окна: точка, показанная строка и окно лупы.
fn picker_view(session: &PickerSession) -> serde_json::Value {
    let point = session.point();
    let local = session.frame().map_or((0, 0), |frame| {
        let origin = frame.origin();
        (point.x - origin.x, point.y - origin.y)
    });
    let message = session
        .copy_error()
        .map(str::to_owned)
        .or_else(|| session.sample_error().map(picker_sample_message));
    serde_json::json!({
        "point": { "x": point.x, "y": point.y },
        "local": { "x": local.0, "y": local.1 },
        "value": session.value(),
        "blocked": session.sample_error().is_some(),
        "reason": session.sample_error().map(picker_sample_reason),
        "message": message,
        "format": session.format().setting_value(),
        "phase": picker_phase_name(session.phase()),
        "geometryChanged": session.geometry_changed(),
        "loupe": picker_loupe_view(session),
    })
}

/// Повторный вызов возвращает в открытый выбор, а не делает новый снимок.
fn show_picker_or_focus(app: &AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(PICKER_WINDOW) {
        let _ = window.show();
        let _ = window.set_focus();
        #[cfg(windows)]
        if let Ok(hwnd) = window.hwnd() {
            util_maker_platform::raise_window(hwnd.0 as isize);
        }
        return Ok(());
    }
    open_picker_overlay(app)
}

fn open_picker_overlay(app: &AppHandle) -> Result<(), String> {
    let screen = screen_service();
    let geometry = screen.geometry().map_err(|error| error.message)?;
    let rect = geometry.virtual_screen;
    let raw = screen
        .source()
        .capture(rect)
        .map_err(|error| error.to_string())?;
    let stride = usize::try_from(raw.stride).map_err(|_| "некорректный stride кадра".to_owned())?;
    let frame = FrameData::new(
        raw.pixels,
        rect.width,
        rect.height,
        stride,
        PickerPoint::new(rect.x, rect.y),
        geometry,
    )
    .map_err(|error| format!("кадр для выбора цвета: {error}"))?;
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "состояние недоступно".to_owned())?;
    let format = color_picker_format(&state.settings);
    let start = util_maker_platform::cursor_position()
        .map(|(x, y)| PickerPoint::new(x, y))
        .unwrap_or_else(|| {
            PickerPoint::new(
                rect.x + rect.width as i32 / 2,
                rect.y + rect.height as i32 / 2,
            )
        });
    {
        let mut picker = state.picker.lock().map_err(|error| error.to_string())?;
        *picker = Some(PickerSession::new(frame, format, start, 5_000));
    }
    let window =
        WebviewWindowBuilder::new(app, PICKER_WINDOW, WebviewUrl::App("picker.html".into()))
            .title("Выбрать цвет пикселя")
            .decorations(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .resizable(false)
            .shadow(false)
            .focused(true)
            .visible(false)
            .build()
            .map_err(|error| format!("окно выбора цвета: {error}"))?;
    window
        .set_position(PhysicalPosition::new(rect.x, rect.y))
        .map_err(|error| format!("положение окна выбора: {error}"))?;
    window
        .set_size(PhysicalSize::new(rect.width, rect.height))
        .map_err(|error| format!("размер окна выбора: {error}"))?;
    let _ = window.set_always_on_top(true);
    let _ = window.show();
    let _ = window.set_focus();
    #[cfg(windows)]
    if let Ok(hwnd) = window.hwnd() {
        util_maker_platform::raise_window(hwnd.0 as isize);
    }
    Ok(())
}

fn open_screenshot_overlay(app: &AppHandle) -> Result<(), String> {
    if let Some(state) = app.try_state::<AppState>() {
        let mut source = state
            .overlay_source
            .lock()
            .map_err(|error| error.to_string())?;
        *source = None;
    }
    open_screen_overlay(app, false)
}

fn open_qr_overlay(app: &AppHandle, source: InvocationSource) -> Result<(), String> {
    if let Some(state) = app.try_state::<AppState>() {
        let mut pending = state
            .overlay_source
            .lock()
            .map_err(|error| error.to_string())?;
        *pending = Some(source);
    }
    open_screen_overlay(app, true)
}

fn open_screen_overlay(app: &AppHandle, qr: bool) -> Result<(), String> {
    let screen = screen_service();
    let geometry = screen.geometry().map_err(|error| error.message)?;
    let rect = geometry.virtual_screen;
    let raw = screen
        .source()
        .capture(rect)
        .map_err(|error| error.to_string())?;
    let mut data = Vec::with_capacity(rect.width as usize * rect.height as usize * 4);
    for row in 0..rect.height as usize {
        let start = row * raw.stride as usize;
        data.extend_from_slice(&raw.pixels[start..start + rect.width as usize * 4]);
    }
    if let Some(state) = app.try_state::<AppState>() {
        *state.overlay.lock().map_err(|error| error.to_string())? = Some(OverlayFrame {
            origin_x: rect.x,
            origin_y: rect.y,
            width: rect.width,
            height: rect.height,
            monitors: geometry.monitors.clone(),
            data,
        });
    }
    if let Some(window) = app.get_webview_window("overlay") {
        let _ = window.close();
    }
    let title = if qr {
        "Прочитать QR с экрана"
    } else {
        "Снять область"
    };
    let page = if qr {
        "overlay.html?mode=qr"
    } else {
        "overlay.html"
    };
    let window = WebviewWindowBuilder::new(app, "overlay", WebviewUrl::App(page.into()))
        .title(title)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .transparent(true)
        .shadow(false)
        .focused(true)
        .visible(false)
        .build()
        .map_err(|error| format!("overlay: {error}"))?;
    window
        .set_position(PhysicalPosition::new(rect.x, rect.y))
        .map_err(|error| format!("overlay position: {error}"))?;
    window
        .set_size(PhysicalSize::new(rect.width, rect.height))
        .map_err(|error| format!("overlay size: {error}"))?;
    let _ = window.set_always_on_top(true);
    let _ = window.show();
    let _ = window.set_focus();
    #[cfg(windows)]
    if let Ok(hwnd) = window.hwnd() {
        util_maker_platform::raise_window(hwnd.0 as isize);
    }
    Ok(())
}

#[tauri::command]
fn open_screenshot_editor(
    app: AppHandle,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Result<(), String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "кадр не захвачен".to_owned())?;
    let (origin_x, origin_y, frame_width, frame_height, monitors) = {
        let overlay = state.overlay.lock().map_err(|error| error.to_string())?;
        let frame = overlay
            .as_ref()
            .ok_or_else(|| "кадр не захвачен".to_owned())?;
        (
            frame.origin_x,
            frame.origin_y,
            frame.width,
            frame.height,
            frame.monitors.clone(),
        )
    };
    if width < 3 || height < 3 || x < 0 || y < 0 {
        return Err("слишком маленькая область".to_owned());
    }
    if x as u32 + width > frame_width || y as u32 + height > frame_height {
        return Err("область выходит за пределы экрана".to_owned());
    }
    let app_for_editor = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(120));
        if let Some(window) = app_for_editor.get_webview_window("overlay") {
            let _ = window.hide();
        }
        std::thread::sleep(std::time::Duration::from_millis(120));
        if let Some(window) = app_for_editor.get_webview_window("overlay") {
            let _ = window.close();
        }
        if let Some(window) = app_for_editor.get_webview_window("editor") {
            let _ = window.close();
        }
        if let Err(error) = show_editor_window(
            &app_for_editor,
            x,
            y,
            width,
            height,
            origin_x,
            origin_y,
            frame_width,
            frame_height,
            &monitors,
        ) {
            logging::write(&format!("редактор снимка: {error}"));
        }
    });
    Ok(())
}

/// Завершает выбор области для «Прочитать QR с экрана». Кадр, который
/// пользователь видел в окне выбора, передаётся плагину как `frame.capture`:
/// координаты и содержимое не сдвигаются, даже если экран успел измениться.
#[tauri::command]
fn qr_read_region(app: AppHandle, x: i32, y: i32, width: u32, height: u32) -> Result<(), String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "кадр не захвачен".to_owned())?;
    let (origin_x, origin_y, frame_width, frame_height) = {
        let overlay = state.overlay.lock().map_err(|error| error.to_string())?;
        let frame = overlay
            .as_ref()
            .ok_or_else(|| "кадр не захвачен".to_owned())?;
        (frame.origin_x, frame.origin_y, frame.width, frame.height)
    };
    if width < 3 || height < 3 || x < 0 || y < 0 {
        return Err("слишком маленькая область".to_owned());
    }
    if x as u32 + width > frame_width || y as u32 + height > frame_height {
        return Err("область выходит за пределы экрана".to_owned());
    }
    logging::write(&format!("QR: выбрана область x={x} y={y} {width}x{height}"));
    let frame = state
        .overlay
        .lock()
        .map_err(|error| error.to_string())?
        .take()
        .ok_or_else(|| "кадр не захвачен".to_owned())?;
    let source = state
        .overlay_source
        .lock()
        .map_err(|error| error.to_string())?
        .take()
        .unwrap_or(InvocationSource::Hotkey);
    hide_and_close(&app, "overlay");

    let selection = util_maker_sdk::FrameRegion {
        x: origin_x.saturating_add(x),
        y: origin_y.saturating_add(y),
        width,
        height,
    };
    let geometry = util_maker_sdk::ScreenGeometry {
        virtual_screen: util_maker_sdk::FrameRegion {
            x: origin_x,
            y: origin_y,
            width: frame.width,
            height: frame.height,
        },
        monitors: frame.monitors.clone(),
    };
    let frozen = FrozenFrame {
        geometry,
        origin: util_maker_sdk::FrameOrigin {
            x: origin_x,
            y: origin_y,
        },
        pixels: frame.data,
        stride: frame.width.saturating_mul(4),
    };
    let target = ActionRef::new(QR_MODULE_ID, QR_READ_ACTION);
    let params = serde_json::json!({
        "selection": {
            "x": selection.x,
            "y": selection.y,
            "width": selection.width,
            "height": selection.height,
        }
    });
    // Окно выбора прячем уже после ответа команды: закрытие изнутри
    // обработчика, пока WebView ждёт invoke, подвешивает приложение.
    let app_for_close = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(120));
        hide_and_close(&app_for_close, "overlay");
    });
    let app_for_call = app.clone();
    std::thread::spawn(move || {
        qr_read_and_show(app_for_call, target, source, params, frozen);
    });
    Ok(())
}

/// Выполняет чтение QR вне основного потока и показывает результат, когда
/// плагин закончил. Вызов из основного потока блокировал бы WebView2 и
/// закрытие окна выбора.
fn qr_read_and_show(
    app: AppHandle,
    target: ActionRef,
    source: InvocationSource,
    params: serde_json::Value,
    frozen: FrozenFrame,
) {
    let started = Instant::now();
    let selection = Arc::new(Mutex::new(selection_service()));
    let context = InvocationContext::new(source);
    let result = match invoke_action_params_with_frozen(
        &app,
        &target,
        source,
        params,
        selection,
        context,
        Some(frozen),
    ) {
        Ok(outcome) => {
            let since_event = started.elapsed();
            logging::write(&format!(
                "action->result source={} module={} action={} core_elapsed_ms={} since_event_ms={}",
                source.as_str(),
                outcome.module_id,
                outcome.action_id,
                outcome.elapsed.as_millis(),
                since_event.as_millis()
            ));
            LastResult {
                message: outcome.message.clone(),
                data: outcome.data.clone(),
                module_id: outcome.module_id.clone(),
                action_id: outcome.action_id.clone(),
                action_name: outcome.action_name.clone(),
                source: source.as_str().to_owned(),
                core_elapsed_ms: outcome.elapsed.as_millis(),
                since_event_ms: since_event.as_millis(),
                status: outcome_status(&outcome).to_owned(),
            }
        }
        Err(error) => {
            logging::write(&format!(
                "ошибка чтения QR module={} action={} source={}: {error}",
                target.module_id,
                target.action_id,
                source.as_str()
            ));
            LastResult {
                message: format!("Не удалось прочитать QR: {error}"),
                data: None,
                module_id: target.module_id.clone(),
                action_id: target.action_id.clone(),
                action_name: "Прочитать QR с экрана".to_owned(),
                source: source.as_str().to_owned(),
                core_elapsed_ms: 0,
                since_event_ms: started.elapsed().as_millis(),
                status: "error".to_owned(),
            }
        }
    };
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || show_result(&handle, result));
}

/// Открывает окно выбора заново после неудачного чтения QR.
#[tauri::command]
fn retry_qr_read(app: AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(RESULT_WINDOW) {
        let _ = window.close();
    }
    open_qr_overlay(&app, InvocationSource::Hotkey)
}

#[allow(clippy::too_many_arguments)]
fn show_editor_window(
    app: &AppHandle,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    origin_x: i32,
    origin_y: i32,
    frame_width: u32,
    frame_height: u32,
    monitors: &[MonitorGeometry],
) -> Result<(), String> {
    let selection = FrameRegion {
        x: origin_x.saturating_add(x),
        y: origin_y.saturating_add(y),
        width,
        height,
    };
    let frame = FrameRegion {
        x: origin_x,
        y: origin_y,
        width: frame_width,
        height: frame_height,
    };
    let placement = editor_placement::place_editor(selection, monitors, frame);
    let window = WebviewWindowBuilder::new(
        app,
        "editor",
        WebviewUrl::App(format!("editor.html?x={x}&y={y}&w={width}&h={height}").into()),
    )
    .title("Редактировать снимок")
    .decorations(true)
    .always_on_top(true)
    .resizable(false)
    .visible(false)
    .build()
    .map_err(|error| format!("редактор: {error}"))?;
    window
        .set_position(PhysicalPosition::new(placement.x, placement.y))
        .map_err(|error| format!("редактор position: {error}"))?;
    window
        .set_size(PhysicalSize::new(placement.width, placement.height))
        .map_err(|error| format!("редактор size: {error}"))?;
    let _ = window.set_always_on_top(true);
    let _ = window.show();
    let _ = window.set_focus();
    Ok(())
}

#[tauri::command]
fn get_editor_image(
    app: AppHandle,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Result<String, String> {
    use base64::Engine as _;
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "кадр не захвачен".to_owned())?;
    let overlay = state.overlay.lock().map_err(|error| error.to_string())?;
    let frame = overlay
        .as_ref()
        .ok_or_else(|| "кадр не захвачен".to_owned())?;
    if width == 0 || height == 0 || x < 0 || y < 0 {
        return Err("некорректная область".to_owned());
    }
    if x as u32 + width > frame.width || y as u32 + height > frame.height {
        return Err("область выходит за пределы кадра".to_owned());
    }
    let stride = frame.width as usize * 4;
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for row in 0..height as usize {
        let start = (y as usize + row) * stride + x as usize * 4;
        let line = &frame.data[start..start + width as usize * 4];
        for pixel in line.as_chunks::<4>().0 {
            rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
        }
    }
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|error| error.to_string())?;
        writer
            .write_image_data(&rgba)
            .map_err(|error| error.to_string())?;
    }
    Ok(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    ))
}

fn default_screenshot_directory() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Pictures")
        .join("Util Maker")
}

fn resolve_screenshot_directory(
    configured: Option<&str>,
    fallback: PathBuf,
) -> Result<PathBuf, String> {
    let configured = configured.unwrap_or_default().trim();
    if configured.is_empty() {
        return Ok(fallback);
    }
    let path = PathBuf::from(configured);
    if !path.is_absolute() {
        return Err(format!(
            "Укажите полный путь к папке сохранения, например D:\\Скриншоты: {configured}"
        ));
    }
    if path.exists() && !path.is_dir() {
        return Err(format!(
            "Путь сохранения не является папкой: {}",
            path.display()
        ));
    }
    Ok(path)
}

fn screenshot_save_directory(app: &AppHandle) -> Result<PathBuf, String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "настройки сохранения недоступны".to_owned())?;
    let values = state
        .settings
        .values(SCREENSHOT_MODULE_ID, None)
        .map_err(|error| format!("настройки «Скриншотов»: {error}"))?;
    resolve_screenshot_directory(
        values
            .get(SCREENSHOT_SAVE_DIRECTORY)
            .and_then(serde_json::Value::as_str),
        default_screenshot_directory(),
    )
}

fn write_screenshot_png(
    directory: &Path,
    width: u32,
    height: u32,
    rgba: &str,
) -> Result<PathBuf, String> {
    if width == 0 || height == 0 || rgba.len() != width as usize * height as usize * 4 * 2 {
        return Err("некорректный размер изображения".to_owned());
    }
    let mut bytes = Vec::with_capacity(width as usize * height as usize * 4);
    for index in 0..width as usize * height as usize * 4 {
        let start = index * 2;
        bytes.push(
            u8::from_str_radix(&rgba[start..start + 2], 16).map_err(|error| error.to_string())?,
        );
    }
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|error| error.to_string())?;
        writer
            .write_image_data(&bytes)
            .map_err(|error| error.to_string())?;
    }
    std::fs::create_dir_all(directory).map_err(|error| {
        format!(
            "не удалось создать папку сохранения {}: {error}",
            directory.display()
        )
    })?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis())
        .unwrap_or_default();
    let path = directory.join(format!("Screenshot-{stamp}.png"));
    std::fs::write(&path, png)
        .map_err(|error| format!("не удалось сохранить {}: {error}", path.display()))?;
    Ok(path)
}

#[tauri::command]
fn save_editor_image(
    app: AppHandle,
    width: u32,
    height: u32,
    rgba: String,
) -> Result<String, String> {
    let directory = screenshot_save_directory(&app)?;
    write_screenshot_png(&directory, width, height, &rgba).map(|path| path.display().to_string())
}

#[tauri::command]
fn get_overlay_geometry(app: AppHandle) -> Result<serde_json::Value, String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "кадр не захвачен".to_owned())?;
    let overlay = state.overlay.lock().map_err(|error| error.to_string())?;
    let frame = overlay
        .as_ref()
        .ok_or_else(|| "кадр не захвачен".to_owned())?;
    Ok(serde_json::json!({
        "x": frame.origin_x,
        "y": frame.origin_y,
        "width": frame.width,
        "height": frame.height,
    }))
}

/// Захватывает внешнее окно до открытия окон приложения. Тот же сервис
/// передаётся в `HostDispatcher`, иначе ссылка на источник не будет найдена.
fn capture_selection_source() -> (
    Arc<Mutex<NativeSelection>>,
    Option<util_maker_core::SourceRef>,
) {
    let selection = Arc::new(Mutex::new(selection_service()));
    let mut source_ref = None;
    match selection.lock() {
        Ok(mut service) => match service.capture_foreground() {
            Ok(captured) => {
                logging::write(&format!(
                    "источник вызова: pid={} окно={}x{}",
                    captured.process_id, captured.window.width, captured.window.height
                ));
                if captured.process_id != std::process::id() {
                    source_ref = Some(captured.as_ref());
                } else {
                    logging::write("источник вызова: собственное окно приложения");
                }
            }
            Err(error) => logging::write(&format!("источник вызова: ошибка {error:?}")),
        },
        Err(_) => logging::write("источник вызова: сервис выделения занят"),
    }
    (selection, source_ref)
}

fn action_needs_external_selection(target: &ActionRef) -> bool {
    (target.module_id == QR_MODULE_ID && target.action_id == "show-qr")
        || target.action_id == "translate"
}

fn invoke_action(
    app: &AppHandle,
    target: &ActionRef,
    source: InvocationSource,
    selection: Arc<Mutex<NativeSelection>>,
    context: InvocationContext,
) -> Result<InvokeOutcome, String> {
    invoke_action_params(
        app,
        target,
        source,
        serde_json::Value::Null,
        selection,
        context,
    )
}

fn invoke_action_params(
    app: &AppHandle,
    target: &ActionRef,
    source: InvocationSource,
    params: serde_json::Value,
    selection: Arc<Mutex<NativeSelection>>,
    context: InvocationContext,
) -> Result<InvokeOutcome, String> {
    invoke_action_params_with_frozen(app, target, source, params, selection, context, None)
}

/// Выполняет действие с заранее снятым кадром: `frame.capture` процесса
/// вернёт выбранный прямоугольник снимка, а не живого экрана.
fn invoke_action_params_with_frozen(
    app: &AppHandle,
    target: &ActionRef,
    source: InvocationSource,
    params: serde_json::Value,
    selection: Arc<Mutex<NativeSelection>>,
    context: InvocationContext,
    frozen: Option<FrozenFrame>,
) -> Result<InvokeOutcome, String> {
    logging::debug(&format!(
        "вызов действия module={} action={} source={}",
        target.module_id,
        target.action_id,
        source.as_str()
    ));
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "каталог модулей не готов".to_owned())?;
    // Диспетчер живёт ровно один вызов: кадры и буферы освобождаются вместе
    // с ним, а параллельные действия не блокируют друг друга на время IPC.
    let router = extensions::ExtensionRouterImpl::new(
        state.catalog.registry.clone(),
        state.settings.clone(),
        selection.clone(),
    );
    let native = NativeHost::new(selection, state.settings.clone());
    let native = match frozen {
        Some(frame) => native.with_frozen_frame(frame),
        None => native,
    };
    let mut host = HostDispatcher::new(native).with_extensions(Box::new(router));
    state
        .catalog
        .registry
        .invoke_session_with_params(
            target,
            context,
            params,
            INVOKE_TIMEOUT,
            Some(logging::logger()),
            &mut host,
        )
        .map_err(|failure| failure.message)
}

fn outcome_status(outcome: &InvokeOutcome) -> &'static str {
    let kind = outcome
        .data
        .as_ref()
        .and_then(|value| value.get("outcome"))
        .and_then(|value| value.as_str());
    match kind {
        Some(
            "blocked" | "missing-key" | "awaiting-selection" | "manual-input" | "invalid-input",
        ) => "warning",
        _ => "ok",
    }
}

fn invoke_and_show(app: &AppHandle, target: ActionRef, source: InvocationSource) {
    invoke_and_show_params(app, target, source, serde_json::Value::Null, None, true);
}

fn invoke_and_show_params(
    app: &AppHandle,
    target: ActionRef,
    source: InvocationSource,
    params: serde_json::Value,
    frozen: Option<FrozenFrame>,
    allow_overlay: bool,
) {
    if activate_qr_result(app, &target) {
        return;
    }
    if source == InvocationSource::Tray && action_needs_external_selection(&target) {
        // Меню трея закрывается с задержкой: без паузы foreground ещё наше
        // меню и выделение внешней программы не читается.
        std::thread::sleep(Duration::from_millis(150));
    }
    // Для готового кадра источник вызова не нужен: внешнее окно уже потеряло
    // фокус, и `source_lost` заблокировал бы `frame.capture`.
    let (selection, context) = if frozen.is_some() {
        (
            Arc::new(Mutex::new(selection_service())),
            InvocationContext::new(source),
        )
    } else {
        let (selection, source_ref) = capture_selection_source();
        let context = match source {
            InvocationSource::Hotkey => InvocationContext::hotkey(source_ref),
            InvocationSource::Tray => InvocationContext::tray(source_ref),
        };
        (selection, context)
    };
    if allow_overlay
        && target.module_id == COLOR_PICKER_MODULE_ID
        && target.action_id == COLOR_PICKER_ACTION_ID
    {
        if let Err(error) = show_picker_or_focus(app) {
            show_result(
                app,
                LastResult {
                    message: format!("Не удалось открыть выбор цвета: {error}"),
                    data: None,
                    module_id: target.module_id,
                    action_id: target.action_id,
                    action_name: "Скопировать цвет пикселя".to_owned(),
                    source: source.as_str().to_owned(),
                    core_elapsed_ms: 0,
                    since_event_ms: 0,
                    status: "error".to_owned(),
                },
            );
        }
        return;
    }
    if allow_overlay
        && target.module_id == SCREENSHOT_MODULE_ID
        && target.action_id == "capture-region"
    {
        if let Err(error) = open_screenshot_overlay(app) {
            show_result(
                app,
                LastResult {
                    message: format!("Не удалось открыть редактор: {error}"),
                    data: None,
                    module_id: target.module_id,
                    action_id: target.action_id,
                    action_name: "Снять область".to_owned(),
                    source: source.as_str().to_owned(),
                    core_elapsed_ms: 0,
                    since_event_ms: 0,
                    status: "error".to_owned(),
                },
            );
        }
        return;
    }
    if allow_overlay && target.module_id == QR_MODULE_ID && target.action_id == QR_READ_ACTION {
        if activate_qr_read_result(app) {
            return;
        }
        if let Err(error) = open_qr_overlay(app, source) {
            show_result(
                app,
                LastResult {
                    message: format!("Не удалось открыть выбор области: {error}"),
                    data: None,
                    module_id: target.module_id,
                    action_id: target.action_id,
                    action_name: "Прочитать QR с экрана".to_owned(),
                    source: source.as_str().to_owned(),
                    core_elapsed_ms: 0,
                    since_event_ms: 0,
                    status: "error".to_owned(),
                },
            );
        }
        return;
    }
    let started = Instant::now();
    if target.action_id == "translate" {
        show_result(
            app,
            LastResult {
                message: "Перевод…".to_owned(),
                data: None,
                module_id: target.module_id.clone(),
                action_id: target.action_id.clone(),
                action_name: action_name_for(app, &target),
                source: source.as_str().to_owned(),
                core_elapsed_ms: 0,
                since_event_ms: 0,
                status: "progress".to_owned(),
            },
        );
    }
    match invoke_action_params_with_frozen(app, &target, source, params, selection, context, frozen)
    {
        Ok(outcome) => {
            let since_event = started.elapsed();
            logging::write(&format!(
                "action->result source={} module={} action={} core_elapsed_ms={} since_event_ms={}",
                source.as_str(),
                outcome.module_id,
                outcome.action_id,
                outcome.elapsed.as_millis(),
                since_event.as_millis()
            ));
            show_result(
                app,
                LastResult {
                    message: outcome.message.clone(),
                    data: outcome.data.clone(),
                    module_id: outcome.module_id.clone(),
                    action_id: outcome.action_id.clone(),
                    action_name: outcome.action_name.clone(),
                    source: source.as_str().to_owned(),
                    core_elapsed_ms: outcome.elapsed.as_millis(),
                    since_event_ms: since_event.as_millis(),
                    status: outcome_status(&outcome).to_owned(),
                },
            );
            let first_frame = started.elapsed().as_millis() as u64;
            if let Some(state) = app.try_state::<AppState>() {
                if let Ok(mut metrics) = state.metrics.lock() {
                    let entry = LogEntry::invocation(
                        env!("CARGO_PKG_VERSION"),
                        outcome.module_id.clone(),
                        outcome.action_id.clone(),
                        outcome.elapsed.as_millis() as u64,
                        "ok",
                        None,
                    )
                    .with_first_frame(first_frame, outcome.cold);
                    let _ = metrics.record(&entry);
                }
            }
        }
        Err(error) => {
            logging::write(&format!(
                "ошибка вызова module={} action={} source={}: {error}",
                target.module_id,
                target.action_id,
                source.as_str()
            ));
            show_result(
                app,
                LastResult {
                    message: format!("Не удалось выполнить действие: {error}"),
                    data: None,
                    module_id: target.module_id.clone(),
                    action_id: target.action_id.clone(),
                    action_name: action_name_for(app, &target),
                    source: source.as_str().to_owned(),
                    core_elapsed_ms: 0,
                    since_event_ms: started.elapsed().as_millis(),
                    status: "error".to_owned(),
                },
            );
            if let Some(state) = app.try_state::<AppState>() {
                if let Ok(mut metrics) = state.metrics.lock() {
                    let entry = LogEntry::invocation(
                        env!("CARGO_PKG_VERSION"),
                        target.module_id.clone(),
                        target.action_id.clone(),
                        started.elapsed().as_millis() as u64,
                        "error",
                        Some("invoke_failed"),
                    )
                    .with_message(&error);
                    let _ = metrics.record(&entry);
                }
            }
        }
    }
}

/// Слушает WM_HOTKEY из платформенного адаптера и вызывает действие.
fn spawn_hotkey_listener(app: AppHandle, events: Receiver<HotkeyEvent>) {
    std::thread::spawn(move || loop {
        match events.recv_timeout(Duration::from_millis(250)) {
            Ok(event) if event.pressed => {
                logging::debug(&format!("WM_HOTKEY id={}", event.id));
                let paused = app
                    .try_state::<ShortcutRuntime>()
                    .is_some_and(|runtime| runtime.paused.load(Ordering::SeqCst));
                if paused {
                    continue;
                }
                let target = app
                    .try_state::<ShortcutRuntime>()
                    .and_then(|runtime| runtime.target_for(event.id))
                    .unwrap_or_else(control_action_ref);
                let handle = app.clone();
                let _ = app.run_on_main_thread(move || {
                    invoke_and_show(&handle, target, InvocationSource::Hotkey);
                });
            }
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    });
}

fn set_shortcut_status(app: &AppHandle, status: &str) {
    if let Some(runtime) = app.try_state::<ShortcutRuntime>() {
        if let Ok(mut slot) = runtime.status.lock() {
            *slot = status.to_owned();
        }
    }
    logging::write(&format!("сочетание: {status}"));
    apply_tray_state(app);
}

fn shortcut_paused(app: &AppHandle) -> bool {
    app.try_state::<ShortcutRuntime>()
        .is_some_and(|runtime| runtime.paused.load(Ordering::SeqCst))
}

fn pause_menu_label(paused: bool) -> &'static str {
    if paused {
        "Возобновить сочетание"
    } else {
        "Пауза сочетания"
    }
}

/// Иконка трея показывает только два состояния: работа и пауза. Причину
/// недоступного сочетания несёт подсказка, а не третий цвет иконки.
fn tray_visual(paused: bool, registered: bool, status: &str) -> (&'static [u8], String) {
    if paused {
        (
            include_bytes!("../icons/tray-paused.png").as_slice(),
            "Util Maker — на паузе".to_owned(),
        )
    } else if registered {
        (
            include_bytes!("../icons/tray-working.png").as_slice(),
            "Util Maker — работает".to_owned(),
        )
    } else {
        (
            include_bytes!("../icons/tray-working.png").as_slice(),
            format!("Util Maker — работает, сочетание недоступно: {status}"),
        )
    }
}

fn apply_tray_state(app: &AppHandle) {
    let (icon, tooltip) = if let Some(runtime) = app.try_state::<ShortcutRuntime>() {
        let status = runtime
            .status
            .lock()
            .ok()
            .map(|value| value.clone())
            .unwrap_or_default();
        tray_visual(
            runtime.paused.load(Ordering::SeqCst),
            runtime.registered.load(Ordering::SeqCst),
            &status,
        )
    } else {
        (
            include_bytes!("../icons/tray-working.png").as_slice(),
            "Util Maker — состояние неизвестно".to_owned(),
        )
    };
    if let Some(tray) = app.tray_by_id("main") {
        if let Ok(image) = Image::from_bytes(icon) {
            let _ = tray.set_icon(Some(image));
        }
        let _ = tray.set_tooltip(Some(tooltip));
    }
}

fn refresh_tray_menu(app: &AppHandle) {
    if let Err(error) = rebuild_tray_menu(app) {
        logging::write(&format!("обновление меню трея: {error}"));
    }
}

fn toggle_shortcut_pause(app: &AppHandle) {
    logging::debug("меню: переключение паузы сочетания");
    let Some(runtime) = app.try_state::<ShortcutRuntime>() else {
        return;
    };
    if runtime.paused.load(Ordering::SeqCst) {
        let registered = runtime.register_all();
        runtime.registered.store(registered, Ordering::SeqCst);
        if registered {
            runtime.paused.store(false, Ordering::SeqCst);
            save_paused(app, false);
            set_shortcut_status(app, "зарегистрировано");
        } else {
            save_paused(app, true);
            set_shortcut_status(app, "занято другой программой");
        }
    } else {
        // Сначала закрываем обработку событий, затем освобождаем регистрации:
        // иначе уже поставленное в очередь WM_HOTKEY успеет вызвать действие.
        runtime.paused.store(true, Ordering::SeqCst);
        runtime.unregister_all();
        runtime.registered.store(false, Ordering::SeqCst);
        save_paused(app, true);
        set_shortcut_status(app, "на паузе");
    }
    refresh_tray_menu(app);
}

fn retry_shortcut(app: &AppHandle) {
    logging::debug("меню: повторная регистрация сочетания");
    let Some(runtime) = app.try_state::<ShortcutRuntime>() else {
        return;
    };
    if runtime.paused.load(Ordering::SeqCst) {
        set_shortcut_status(app, "на паузе");
        refresh_tray_menu(app);
        return;
    }
    if runtime.registered.load(Ordering::SeqCst) {
        set_shortcut_status(app, "уже зарегистрировано");
        refresh_tray_menu(app);
        return;
    }
    runtime.unregister_all();
    let registered = runtime.register_all();
    runtime.registered.store(registered, Ordering::SeqCst);
    if registered {
        runtime.paused.store(false, Ordering::SeqCst);
        save_paused(app, false);
        set_shortcut_status(app, "зарегистрировано");
    } else {
        set_shortcut_status(app, "занято другой программой");
    }
    refresh_tray_menu(app);
}

/// Ключ платформы в манифесте пакета.
fn platform_key() -> &'static str {
    if cfg!(windows) {
        "windows-x64"
    } else {
        "linux-x64"
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginActionDto {
    id: String,
    name: String,
    description: String,
    hotkey: Option<String>,
    hotkeys: Vec<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginDto {
    id: String,
    name: String,
    version: String,
    publisher: String,
    icon: Option<String>,
    installed: bool,
    enabled: bool,
    status: String,
    actions: Vec<PluginActionDto>,
    settings: Option<SettingsSchema>,
    extension_only: bool,
}

/// Строит страницу настроек и трей из одного снимка активных манифестов.
fn collect_plugins(app: &AppHandle) -> Vec<PluginDto> {
    let Some(state) = app.try_state::<AppState>() else {
        return Vec::new();
    };
    state
        .catalog
        .registry
        .modules()
        .into_iter()
        .map(|entry| {
            let enabled = entry.enabled;
            let icon = entry.manifest.icon.as_ref().and_then(|icon| {
                entry
                    .executable
                    .parent()
                    .and_then(|directory| icon_data_url(&directory.join(icon)))
            });
            PluginDto {
                id: entry.manifest.id.clone(),
                name: entry.manifest.name.clone(),
                version: entry.manifest.version.clone(),
                publisher: entry.manifest.publisher.clone(),
                icon,
                installed: true,
                enabled,
                status: if enabled {
                    "включён"
                } else {
                    "отключён"
                }
                .to_owned(),
                settings: entry.manifest.settings.clone(),
                extension_only: entry.manifest.actions.is_empty()
                    && !entry.manifest.provides.is_empty(),
                actions: entry
                    .manifest
                    .actions
                    .iter()
                    .map(|action| PluginActionDto {
                        id: action.id.clone(),
                        name: action.name.clone(),
                        description: action.description().to_owned(),
                        hotkey: action_hotkeys(app, &entry.manifest, action)
                            .first()
                            .cloned(),
                        hotkeys: action_hotkeys(app, &entry.manifest, action),
                    })
                    .collect(),
            }
        })
        .collect()
}

fn action_hotkeys(app: &AppHandle, manifest: &Manifest, action: &Action) -> Vec<String> {
    if let Some(runtime) = app.try_state::<ShortcutRuntime>() {
        if let Ok(registrations) = runtime.registrations.lock() {
            let mut combos: Vec<String> = registrations
                .values()
                .filter(|registration| {
                    registration.target.module_id == manifest.id
                        && registration.target.action_id == action.id
                })
                .map(|registration| registration.combo.clone())
                .collect();
            combos.sort();
            combos.dedup();
            if !combos.is_empty() {
                return combos;
            }
        }
    }
    action.recommended_hotkeys.clone()
}

fn refresh_plugins_inner(app: &AppHandle) -> Vec<PluginDto> {
    if let Some(state) = app.try_state::<AppState>() {
        state.catalog.refresh();
    }
    if let Err(error) = rebuild_tray_menu(app) {
        logging::write(&format!("меню трея: {error}"));
    }
    sync_action_hotkeys(app);
    collect_plugins(app)
}

#[tauri::command]
fn preview_package(
    app: AppHandle,
    package_path: String,
    publisher: String,
    key_hex: String,
) -> Result<util_maker_packages::PackagePreview, String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "менеджер пакетов не готов".to_owned())?;
    let mut manager = state.packages.lock().map_err(|error| error.to_string())?;
    manager
        .preview(Path::new(&package_path), &publisher, &key_hex)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn install_package(
    app: AppHandle,
    package_path: String,
    publisher: String,
    key_hex: String,
) -> Result<util_maker_packages::InstalledPackage, String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "менеджер пакетов не готов".to_owned())?;
    let mut manager = state.packages.lock().map_err(|error| error.to_string())?;
    let installed = manager
        .install(Path::new(&package_path), &publisher, &key_hex)
        .map_err(|error| error.to_string())?;
    drop(manager);
    let _ = refresh_plugins_inner(&app);
    Ok(installed)
}

#[tauri::command]
fn rollback_package(app: AppHandle, plugin_id: String) -> Result<String, String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "менеджер пакетов не готов".to_owned())?;
    let mut manager = state.packages.lock().map_err(|error| error.to_string())?;
    let installed = manager
        .rollback(&plugin_id)
        .map_err(|error| error.to_string())?;
    drop(manager);
    let _ = refresh_plugins_inner(&app);
    Ok(installed.version)
}

#[tauri::command]
fn remove_package(app: AppHandle, plugin_id: String) -> Result<bool, String> {
    if plugin_id == CONTROL_MODULE_ID {
        return Err("контрольный модуль входит в ядро и не удаляется".to_owned());
    }
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "менеджер пакетов не готов".to_owned())?;
    let mut manager = state.packages.lock().map_err(|error| error.to_string())?;
    let removed = manager
        .remove(&plugin_id)
        .map_err(|error| error.to_string())?;
    drop(manager);
    if removed {
        if let Ok(state) = app.try_state::<AppState>().ok_or(()) {
            let _ = state.settings.remove_module(&plugin_id);
        }
        if let Some(runtime) = app.try_state::<ShortcutRuntime>() {
            runtime.remove_module(&plugin_id);
        }
        remove_saved_shortcuts_for_module(&app, &plugin_id);
    }
    if removed && plugin_id == CONTROL_MODULE_ID {
        if let Some(runtime) = app.try_state::<ShortcutRuntime>() {
            runtime
                .service
                .lock()
                .expect("служба сочетаний")
                .unregister(HOTKEY_ID);
            runtime.registered.store(false, Ordering::SeqCst);
            runtime.paused.store(true, Ordering::SeqCst);
        }
        set_shortcut_status(&app, "пакет удалён");
    }
    let _ = refresh_plugins_inner(&app);
    Ok(removed)
}

#[tauri::command]
fn plugin_settings(app: AppHandle, plugin_id: String) -> Result<serde_json::Value, String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "настройки недоступны".to_owned())?;
    state
        .settings
        .read(&plugin_id, None)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn save_plugin_settings(
    app: AppHandle,
    plugin_id: String,
    values: serde_json::Value,
) -> Result<(), String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "настройки недоступны".to_owned())?;
    let active = values
        .get("activeProfile")
        .and_then(serde_json::Value::as_str);
    let map = values
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .collect();
    state
        .settings
        .save(&plugin_id, active, None, &map)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn invoke_form_command(
    app: AppHandle,
    plugin_id: String,
    command_id: String,
    profile: Option<String>,
    values: serde_json::Map<String, serde_json::Value>,
) -> Result<FormCommandResult, String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "каталог модулей не готов".to_owned())?;
    let entry = state
        .catalog
        .registry
        .module(&plugin_id)
        .ok_or_else(|| "плагин не установлен".to_owned())?;
    let mut process = ModuleProcess::spawn(&entry.executable)
        .map_err(|error| format!("запуск плагина: {error}"))?;
    process
        .initialize_with_capabilities(&plugin_id, &entry.manifest.capabilities)
        .map_err(|error| format!("рукопожатие: {error}"))?;
    let request = FormCommandRequest {
        command_id,
        profile,
        values: values.into_iter().collect(),
        secrets: Default::default(),
    };
    let call_id = format!("command:{}", request.command_id);
    let selection = Arc::new(Mutex::new(selection_service()));
    let router = extensions::ExtensionRouterImpl::new(
        state.catalog.registry.clone(),
        state.settings.clone(),
        selection.clone(),
    );
    let mut host = HostDispatcher::new(NativeHost::new(selection, state.settings.clone()))
        .with_extensions(Box::new(router));
    host.begin_call(&CallSession::new(
        call_id.clone(),
        plugin_id.clone(),
        String::new(),
        InvocationContext::new(InvocationSource::Tray),
    ));
    let result = process
        .form_command_with_host(
            &request,
            Duration::from_secs(10),
            &mut host,
            &entry.manifest.capabilities,
        )
        .map_err(|error| error.to_string());
    host.finish_call(&call_id);
    process.shutdown();
    result
}

#[tauri::command]
fn plugin_secret_present(
    app: AppHandle,
    plugin_id: String,
    _profile: String,
    name: String,
) -> Result<bool, String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "настройки недоступны".to_owned())?;
    state
        .settings
        .secret_present(&plugin_id, &name)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn set_plugin_secret(
    app: AppHandle,
    plugin_id: String,
    _profile: String,
    name: String,
    value: String,
) -> Result<(), String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "настройки недоступны".to_owned())?;
    state
        .settings
        .write_secret(&plugin_id, &name, &value)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn extension_points(
    app: AppHandle,
    module_id: String,
) -> Result<Vec<util_maker_core::ExtensionPointStatus>, String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "каталог модулей не готов".to_owned())?;
    Ok(state.catalog.registry.extension_points(&module_id))
}

#[tauri::command]
fn select_extension(
    app: AppHandle,
    module_id: String,
    point_id: String,
    plugin_id: String,
) -> Result<(), String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "каталог модулей не готов".to_owned())?;
    state
        .catalog
        .registry
        .select_extension(&module_id, &point_id, &plugin_id)
        .map_err(|error| error.to_string())?;
    logging::write(&format!(
        "плагин расширения: {module_id}/{point_id} -> {plugin_id}"
    ));
    Ok(())
}

#[tauri::command]
fn extension_packages(app: AppHandle) -> Vec<extensions::ExtensionPackageDto> {
    let Some(state) = app.try_state::<AppState>() else {
        return Vec::new();
    };
    extensions::extension_packages(&state.catalog.registry)
}

#[tauri::command]
fn plugins(app: AppHandle) -> Vec<PluginDto> {
    refresh_plugins_inner(&app)
}

#[tauri::command]
fn refresh_plugins(app: AppHandle) -> Vec<PluginDto> {
    refresh_plugins_inner(&app)
}

#[tauri::command]
fn set_plugin_enabled(app: AppHandle, plugin_id: String, enabled: bool) -> Result<(), String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "каталог модулей не готов".to_owned())?;
    let changed = state
        .catalog
        .registry
        .set_enabled(&plugin_id, enabled)
        .map_err(|error| error.to_string())?;
    if changed {
        logging::write(&format!(
            "пакет {plugin_id}: {}",
            if enabled {
                "включён"
            } else {
                "отключён"
            }
        ));
        if !enabled && plugin_id != CONTROL_MODULE_ID {
            if let Some(runtime) = app.try_state::<ShortcutRuntime>() {
                runtime.remove_module(&plugin_id);
            }
        }
        if plugin_id == CONTROL_MODULE_ID {
            if let Some(runtime) = app.try_state::<ShortcutRuntime>() {
                if enabled {
                    let registered = runtime.register_all();
                    runtime.registered.store(registered, Ordering::SeqCst);
                    if !registered {
                        runtime.unregister_all();
                        runtime.registered.store(false, Ordering::SeqCst);
                        runtime.paused.store(true, Ordering::SeqCst);
                        save_paused(&app, true);
                        let _ = state.catalog.registry.set_enabled(&plugin_id, false);
                        set_shortcut_status(&app, "занято другой программой");
                        refresh_tray_menu(&app);
                        return Err(register_failure_message(RegisterFailure::Occupied));
                    }
                    runtime.paused.store(false, Ordering::SeqCst);
                    save_paused(&app, false);
                    set_shortcut_status(&app, "зарегистрировано");
                } else {
                    runtime.unregister_all();
                    runtime.registered.store(false, Ordering::SeqCst);
                    runtime.paused.store(true, Ordering::SeqCst);
                    save_paused(&app, true);
                    set_shortcut_status(&app, "отключён");
                }
            }
        }
        sync_action_hotkeys(&app);
        rebuild_tray_menu(&app)?;
    }
    Ok(())
}

/// Проверяет сигнатуру файла, чтобы повреждённая иконка не превращалась в
/// «битую картинку»: в этом случае интерфейс показывает нейтральную заглушку.
fn has_image_signature(bytes: &[u8]) -> bool {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(b"\xff\xd8\xff")
        || (bytes.len() > 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP")
    {
        return true;
    }
    std::str::from_utf8(bytes).is_ok_and(|text| text.contains("<svg"))
}

/// Небольшой PNG-значок из манифеста передаётся в WebView как data URL.
fn icon_data_url(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > 1024 * 1024 {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if !has_image_signature(&bytes) {
        return None;
    }
    let mime = match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "svg" => "image/svg+xml",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        _ => "image/png",
    };
    Some(format!("data:{mime};base64,{}", base64(&bytes)))
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        output.push(ALPHABET[(first >> 2) as usize] as char);
        output.push(ALPHABET[(((first & 0b11) << 4) | (second >> 4)) as usize] as char);
        if chunk.len() > 1 {
            output.push(ALPHABET[(((second & 0b1111) << 2) | (third >> 6)) as usize] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(ALPHABET[(third & 0b11_1111) as usize] as char);
        } else {
            output.push('=');
        }
    }
    output
}

#[tauri::command]
fn change_action_shortcut(
    app: AppHandle,
    module_id: String,
    action_id: String,
    combo: String,
) -> Result<String, String> {
    let normalized = util_maker_core::hotkeys::Hotkey::parse(&combo)
        .map_err(|error| error.to_string())?
        .canonical();
    let Some(runtime) = app.try_state::<ShortcutRuntime>() else {
        return Err("служба сочетаний недоступна".to_owned());
    };
    let target = ActionRef::new(module_id.clone(), action_id.clone());
    let (id, old_combo) = {
        let registrations = runtime
            .registrations
            .lock()
            .map_err(|error| error.to_string())?;
        registrations
            .iter()
            .find(|(_, registration)| {
                registration.target.module_id == module_id
                    && registration.target.action_id == action_id
            })
            .map(|(id, registration)| (*id, Some(registration.combo.clone())))
            .unwrap_or_else(|| (runtime.next_id.fetch_add(1, Ordering::SeqCst), None))
    };
    if old_combo.as_deref() == Some(normalized.as_str()) {
        return Ok(normalized);
    }
    if let Ok(registrations) = runtime.registrations.lock() {
        if let Some(owner) = registrations.values().find(|registration| {
            registration.combo == normalized
                && !(registration.target.module_id == module_id
                    && registration.target.action_id == action_id)
        }) {
            return Err(format!(
                "сочетание уже назначено действию {} / {}",
                owner.target.module_id, owner.target.action_id
            ));
        }
    }
    let service = runtime.service.lock().expect("служба сочетаний");
    if old_combo.is_some() {
        service.unregister(id);
    }
    match service.register(&normalized, id) {
        Ok(()) => {
            drop(service);
            if let Ok(mut registrations) = runtime.registrations.lock() {
                registrations.insert(
                    id,
                    Registration {
                        combo: normalized.clone(),
                        target,
                    },
                );
            }
            save_action_shortcut(&app, &module_id, &action_id, &normalized);
            if module_id == CONTROL_MODULE_ID {
                *runtime.combo.lock().expect("сочетание") = normalized.clone();
                runtime.registered.store(true, Ordering::SeqCst);
                runtime.paused.store(false, Ordering::SeqCst);
                save_paused(&app, false);
                set_shortcut_status(&app, "зарегистрировано");
                refresh_tray_menu(&app);
                save_shortcut(&app, &normalized);
            }
            Ok(normalized)
        }
        Err(error) => {
            if let Some(old) = old_combo {
                let _ = service.register(&old, id);
            }
            Err(register_failure_message(error))
        }
    }
}

#[tauri::command]
fn add_action_shortcut(
    app: AppHandle,
    module_id: String,
    action_id: String,
    combo: String,
) -> Result<String, String> {
    let normalized = util_maker_core::hotkeys::Hotkey::parse(&combo)
        .map_err(|error| error.to_string())?
        .canonical();
    let Some(runtime) = app.try_state::<ShortcutRuntime>() else {
        return Err("служба сочетаний недоступна".to_owned());
    };
    if let Ok(registrations) = runtime.registrations.lock() {
        if let Some(owner) = registrations
            .values()
            .find(|registration| registration.combo == normalized)
        {
            return Err(format!(
                "сочетание уже назначено действию {} / {}",
                owner.target.module_id, owner.target.action_id
            ));
        }
    }
    let id = runtime.next_id.fetch_add(1, Ordering::SeqCst);
    {
        let service = runtime.service.lock().expect("служба сочетаний");
        service
            .register(&normalized, id)
            .map_err(register_failure_message)?;
    }
    if let Ok(mut registrations) = runtime.registrations.lock() {
        registrations.insert(
            id,
            Registration {
                combo: normalized.clone(),
                target: ActionRef::new(module_id.clone(), action_id.clone()),
            },
        );
    }
    save_action_shortcut(&app, &module_id, &action_id, &normalized);
    Ok(normalized)
}

#[tauri::command]
fn move_action_shortcut(
    app: AppHandle,
    module_id: String,
    action_id: String,
    combo: String,
) -> Result<String, String> {
    let normalized = util_maker_core::hotkeys::Hotkey::parse(&combo)
        .map_err(|error| error.to_string())?
        .canonical();
    let Some(runtime) = app.try_state::<ShortcutRuntime>() else {
        return Err("служба сочетаний недоступна".to_owned());
    };
    let (owner_id, owner) = {
        let mut registrations = runtime
            .registrations
            .lock()
            .map_err(|error| error.to_string())?;
        let owner = registrations
            .iter()
            .find(|(_, registration)| registration.combo == normalized)
            .map(|(id, registration)| (*id, registration.clone()));
        if let Some((id, _)) = &owner {
            registrations.remove(id);
        }
        owner.ok_or_else(|| "сочетание свободно".to_owned())?
    };
    runtime
        .service
        .lock()
        .expect("служба сочетаний")
        .unregister(owner_id);
    remove_saved_shortcut(
        &app,
        &owner.target.module_id,
        &owner.target.action_id,
        &normalized,
    );
    let id = runtime.next_id.fetch_add(1, Ordering::SeqCst);
    {
        let service = runtime.service.lock().expect("служба сочетаний");
        service
            .register(&normalized, id)
            .map_err(register_failure_message)?;
    }
    if let Ok(mut registrations) = runtime.registrations.lock() {
        registrations.insert(
            id,
            Registration {
                combo: normalized.clone(),
                target: ActionRef::new(module_id.clone(), action_id.clone()),
            },
        );
    }
    save_action_shortcut(&app, &module_id, &action_id, &normalized);
    Ok(normalized)
}

#[tauri::command]
fn remove_action_shortcut(
    app: AppHandle,
    module_id: String,
    action_id: String,
    combo: String,
) -> Result<(), String> {
    let Some(runtime) = app.try_state::<ShortcutRuntime>() else {
        return Err("служба сочетаний недоступна".to_owned());
    };
    let normalized = util_maker_core::hotkeys::Hotkey::parse(&combo)
        .map_err(|error| error.to_string())?
        .canonical();
    let id = {
        let mut registrations = runtime
            .registrations
            .lock()
            .map_err(|error| error.to_string())?;
        let id = registrations
            .iter()
            .find(|(_, registration)| {
                registration.target.module_id == module_id
                    && registration.target.action_id == action_id
                    && registration.combo == normalized
            })
            .map(|(id, _)| *id);
        if let Some(id) = id {
            registrations.remove(&id);
        }
        id
    };
    if let Some(id) = id {
        runtime
            .service
            .lock()
            .expect("служба сочетаний")
            .unregister(id);
    }
    remove_saved_shortcut(&app, &module_id, &action_id, &normalized);
    Ok(())
}

#[tauri::command]
fn reset_action_shortcut(
    app: AppHandle,
    module_id: String,
    action_id: String,
) -> Result<(), String> {
    let Some(runtime) = app.try_state::<ShortcutRuntime>() else {
        return Err("служба сочетаний недоступна".to_owned());
    };
    let ids: Vec<u32> = {
        let mut registrations = runtime
            .registrations
            .lock()
            .map_err(|error| error.to_string())?;
        let ids = registrations
            .iter()
            .filter(|(_, registration)| {
                registration.target.module_id == module_id
                    && registration.target.action_id == action_id
            })
            .map(|(id, _)| *id)
            .collect();
        for id in &ids {
            registrations.remove(id);
        }
        ids
    };
    {
        let service = runtime.service.lock().expect("служба сочетаний");
        for id in ids {
            service.unregister(id);
        }
    }
    let mut map = persisted_shortcut_map(&app);
    map.remove(&format!("{module_id}/{action_id}"));
    write_shortcut_map(&app, &map);
    sync_action_hotkeys(&app);
    Ok(())
}

#[tauri::command]
fn change_control_shortcut(
    app: AppHandle,
    state: tauri::State<ShortcutRuntime>,
    combo: String,
) -> Result<String, String> {
    let normalized = util_maker_core::hotkeys::Hotkey::parse(&combo)
        .map_err(|error| error.to_string())?
        .canonical();
    let old = state.combo.lock().expect("сочетание").clone();
    if normalized == old {
        return Ok(old);
    }
    let service = state.service.lock().expect("служба сочетаний");
    service.unregister(HOTKEY_ID);
    match service.register(&normalized, HOTKEY_ID) {
        Ok(()) => {
            *state.combo.lock().expect("сочетание") = normalized.clone();
            if let Ok(mut registrations) = state.registrations.lock() {
                if let Some(registration) = registrations.get_mut(&HOTKEY_ID) {
                    registration.combo = normalized.clone();
                }
            }
            state.registered.store(true, Ordering::SeqCst);
            state.paused.store(false, Ordering::SeqCst);
            drop(service);
            save_paused(&app, false);
            set_shortcut_status(&app, "зарегистрировано");
            refresh_tray_menu(&app);
            save_shortcut(&app, &normalized);
            save_action_shortcut(&app, CONTROL_MODULE_ID, CONTROL_ACTION_ID, &normalized);
            Ok(normalized)
        }
        Err(error) => {
            let _ = service.register(&old, HOTKEY_ID);
            drop(service);
            set_shortcut_status(&app, "зарегистрировано");
            Err(register_failure_message(error))
        }
    }
}

fn register_failure_message(error: RegisterFailure) -> String {
    match error {
        RegisterFailure::SystemReserved { function } => {
            format!("сочетание зарезервировано Windows: {function}")
        }
        RegisterFailure::Occupied => "сочетание занято другой программой".to_owned(),
        RegisterFailure::Unavailable(message) => {
            format!("сочетание недоступно для глобального назначения: {message}")
        }
    }
}

#[tauri::command]
fn get_status(app: AppHandle) -> Result<StatusPayload, String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "каталог модулей не готов".to_owned())?;
    let entry = state
        .catalog
        .registry
        .module(CONTROL_MODULE_ID)
        .ok_or_else(|| format!("пакет {CONTROL_MODULE_ID} не установлен"))?;
    let action = entry
        .manifest
        .action(CONTROL_ACTION_ID)
        .ok_or_else(|| format!("в манифесте нет действия {CONTROL_ACTION_ID}"))?;
    let config_path = state_path(&app)
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    Ok(StatusPayload {
        module_id: entry.manifest.id.clone(),
        module_name: entry.manifest.name.clone(),
        module_version: entry.manifest.version.clone(),
        action_id: action.id.clone(),
        action_name: action.name.clone(),
        shortcut: app
            .try_state::<ShortcutRuntime>()
            .and_then(|runtime| runtime.combo.lock().ok().map(|combo| combo.clone()))
            .unwrap_or_else(|| CONTROL_SHORTCUT.to_owned()),
        shortcut_status: app
            .try_state::<ShortcutRuntime>()
            .and_then(|runtime| runtime.status.lock().ok().map(|status| status.clone()))
            .unwrap_or_else(|| "неизвестно".to_owned()),
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        build_kind: logging::build_kind().to_owned(),
        module_path: entry
            .executable
            .parent()
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        log_path: logging::log_path(),
        config_path,
    })
}

async fn invoke_target(
    app: &AppHandle,
    target: ActionRef,
    source: Option<String>,
) -> Result<LastResult, String> {
    if target.module_id == COLOR_PICKER_MODULE_ID && target.action_id == COLOR_PICKER_ACTION_ID {
        show_picker_or_focus(app)?;
        return Ok(LastResult {
            message: "Выберите пиксель в открывшемся окне.".to_owned(),
            data: None,
            module_id: target.module_id,
            action_id: target.action_id,
            action_name: "Скопировать цвет пикселя".to_owned(),
            source: source.unwrap_or_else(|| "settings".to_owned()),
            core_elapsed_ms: 0,
            since_event_ms: 0,
            status: "progress".to_owned(),
        });
    }
    if target.module_id == "org.utilmaker.screenshot" && target.action_id == "capture-region" {
        open_screenshot_overlay(app)?;
        return Ok(LastResult {
            message: "Выделите область в открывшемся редакторе.".to_owned(),
            data: None,
            module_id: target.module_id,
            action_id: target.action_id,
            action_name: "Снять область".to_owned(),
            source: source.unwrap_or_else(|| "settings".to_owned()),
            core_elapsed_ms: 0,
            since_event_ms: 0,
            status: "progress".to_owned(),
        });
    }
    if target.module_id == QR_MODULE_ID && target.action_id == QR_READ_ACTION {
        let parsed = match source.as_deref() {
            Some("hotkey") => InvocationSource::Hotkey,
            _ => InvocationSource::Tray,
        };
        open_qr_overlay(app, parsed)?;
        return Ok(LastResult {
            message: "Выделите QR-код на экране.".to_owned(),
            data: None,
            module_id: target.module_id,
            action_id: target.action_id,
            action_name: "Прочитать QR с экрана".to_owned(),
            source: source.unwrap_or_else(|| "settings".to_owned()),
            core_elapsed_ms: 0,
            since_event_ms: 0,
            status: "progress".to_owned(),
        });
    }
    let parsed = match source.as_deref() {
        Some("hotkey") => InvocationSource::Hotkey,
        _ => InvocationSource::Tray,
    };
    let handle = app.clone();
    let call_target = target.clone();
    let needs_selection = action_needs_external_selection(&target);
    if needs_selection {
        // Кнопка в настройках закрывает окно, чтобы фокус вернулся в программу,
        // где пользователь выделил текст: иначе выделение недоступно.
        if let Some(window) = app.get_webview_window(SETTINGS_WINDOW) {
            let _ = window.hide();
        }
    }
    let started = Instant::now();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        if needs_selection {
            std::thread::sleep(Duration::from_millis(150));
        }
        let (selection, source_ref) = capture_selection_source();
        let context = match parsed {
            InvocationSource::Hotkey => InvocationContext::hotkey(source_ref),
            InvocationSource::Tray => InvocationContext::tray(source_ref),
        };
        invoke_action(&handle, &call_target, parsed, selection, context)
    })
    .await
    .map_err(|error| format!("задача вызова прервана: {error}"))??;
    let last = LastResult {
        message: outcome.message.clone(),
        data: outcome.data.clone(),
        module_id: outcome.module_id.clone(),
        action_id: outcome.action_id.clone(),
        action_name: outcome.action_name.clone(),
        source: source.unwrap_or_else(|| "settings".to_owned()),
        core_elapsed_ms: outcome.elapsed.as_millis(),
        since_event_ms: started.elapsed().as_millis(),
        status: outcome_status(&outcome).to_owned(),
    };
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut slot) = state.last_result.lock() {
            *slot = Some(last.clone());
        }
    }
    Ok(last)
}

#[tauri::command]
async fn invoke_control(app: AppHandle, source: Option<String>) -> Result<LastResult, String> {
    invoke_target(&app, control_action_ref(), source).await
}

#[tauri::command]
async fn invoke_plugin_action(
    app: AppHandle,
    module_id: String,
    action_id: String,
    source: Option<String>,
) -> Result<LastResult, String> {
    invoke_target(&app, ActionRef::new(module_id, action_id), source).await
}

#[tauri::command]
fn encode_qr_text(
    app: AppHandle,
    text: String,
    width: u32,
    height: u32,
) -> Result<serde_json::Value, String> {
    let target = ActionRef::new("org.utilmaker.qr-code", "show-qr");
    let selection = Arc::new(Mutex::new(selection_service()));
    let context = InvocationContext::new(InvocationSource::Hotkey);
    let outcome = invoke_action_params(
        &app,
        &target,
        InvocationSource::Hotkey,
        serde_json::json!({
            "manualText": text,
            "windowWidth": width,
            "windowHeight": height,
        }),
        selection,
        context,
    )?;
    Ok(outcome.data.unwrap_or(serde_json::Value::Null))
}

#[tauri::command]
fn get_autostart() -> Result<bool, String> {
    let executable =
        std::env::current_exe().map_err(|error| format!("путь приложения: {error}"))?;
    util_maker_platform::autostart::is_enabled(&executable)
}

#[tauri::command]
fn set_autostart(enabled: bool) -> Result<(), String> {
    let executable =
        std::env::current_exe().map_err(|error| format!("путь приложения: {error}"))?;
    util_maker_platform::autostart::set_enabled(enabled, &executable)?;
    logging::write(&format!(
        "автозапуск при входе в Windows: {}",
        if enabled {
            "включён"
        } else {
            "выключен"
        }
    ));
    Ok(())
}

#[tauri::command]
fn get_last_result(app: AppHandle) -> Option<LastResult> {
    app.try_state::<AppState>()
        .and_then(|state| state.last_result.lock().ok().and_then(|slot| slot.clone()))
}

#[tauri::command]
fn open_settings_command(app: AppHandle) {
    open_settings(&app);
}

#[tauri::command]
fn close_settings(app: AppHandle) {
    if let Some(window) = app.get_webview_window(SETTINGS_WINDOW) {
        let _ = window.close();
    }
}

/// Переставляет каналы BGRA в RGBA для PNG: захват отдаёт BGRA.
fn bgra_to_rgba(data: &[u8]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(data.len());
    for pixel in data.as_chunks::<4>().0 {
        rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
    }
    rgba
}

/// Кадр отдаётся PNG data URL — тем же способом, что и картинка редактора.
/// Сырой IPC-ответ на WebView2 не отрисовался, поэтому окно выбора получало
/// пустую подложку и показывало серую заглушку вместо экрана.
#[tauri::command]
fn get_overlay_frame(app: AppHandle) -> Result<String, String> {
    use base64::Engine as _;
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "состояние недоступно".to_owned())?;
    let overlay = state.overlay.lock().map_err(|error| error.to_string())?;
    let frame = overlay
        .as_ref()
        .ok_or_else(|| "кадр не захвачен".to_owned())?;
    let rgba = bgra_to_rgba(&frame.data);
    let mut png_bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, frame.width, frame.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().map_err(|error| error.to_string())?;
        writer
            .write_image_data(&rgba)
            .map_err(|error| error.to_string())?;
    }
    Ok(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png_bytes)
    ))
}

#[tauri::command]
fn copy_overlay_image(width: u32, height: u32, rgba: String) -> Result<(), String> {
    let bytes = (0..rgba.len() / 2)
        .map(|index| u8::from_str_radix(&rgba[index * 2..index * 2 + 2], 16))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    clipboard::write_image(width, height, &bytes).map_err(|error| error.to_string())
}

#[tauri::command]
fn close_overlay(app: AppHandle) {
    for label in ["overlay", "editor"] {
        hide_and_close(&app, label);
    }
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut overlay) = state.overlay.lock() {
            *overlay = None;
        }
        if let Ok(mut source) = state.overlay_source.lock() {
            *source = None;
        }
    }
}

/// Геометрия кадра и текущее состояние выбора для окна пипетки.
#[tauri::command]
fn get_picker_geometry(app: AppHandle) -> Result<serde_json::Value, String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "состояние недоступно".to_owned())?;
    let picker = state.picker.lock().map_err(|error| error.to_string())?;
    let session = picker
        .as_ref()
        .ok_or_else(|| "кадр не захвачен".to_owned())?;
    let frame = session
        .frame()
        .ok_or_else(|| "кадр не захвачен".to_owned())?;
    Ok(serde_json::json!({
        "origin": { "x": frame.origin().x, "y": frame.origin().y },
        "width": frame.width(),
        "height": frame.height(),
        "monitors": frame.geometry().monitors,
        "format": session.format().setting_value(),
        "view": picker_view(session),
    }))
}

/// Кадр отдаётся PNG data URL: сырой IPC-ответ WebView2 не отрисовывает.
#[tauri::command]
fn get_picker_frame(app: AppHandle) -> Result<String, String> {
    use base64::Engine as _;
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "состояние недоступно".to_owned())?;
    let picker = state.picker.lock().map_err(|error| error.to_string())?;
    let session = picker
        .as_ref()
        .ok_or_else(|| "кадр не захвачен".to_owned())?;
    let frame = session
        .frame()
        .ok_or_else(|| "кадр не захвачен".to_owned())?;
    let rgba = frame.rgba_rows();
    let mut png_bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, frame.width(), frame.height());
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().map_err(|error| error.to_string())?;
        writer
            .write_image_data(&rgba)
            .map_err(|error| error.to_string())?;
    }
    Ok(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png_bytes)
    ))
}

fn update_picker<F>(app: &AppHandle, update: F) -> Result<serde_json::Value, String>
where
    F: FnOnce(&mut PickerSession) -> Result<(), String>,
{
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "состояние недоступно".to_owned())?;
    let mut picker = state.picker.lock().map_err(|error| error.to_string())?;
    let session = picker
        .as_mut()
        .ok_or_else(|| "кадр не захвачен".to_owned())?;
    update(session)?;
    Ok(picker_view(session))
}

/// Точка задана в пикселях кадра: окно переводит клик в его координаты.
#[tauri::command]
fn picker_set_point(app: AppHandle, x: i32, y: i32) -> Result<serde_json::Value, String> {
    update_picker(&app, |session| {
        let origin = session
            .frame()
            .ok_or_else(|| "кадр не захвачен".to_owned())?
            .origin();
        session.set_point(PickerPoint::new(
            origin.x.saturating_add(x),
            origin.y.saturating_add(y),
        ));
        Ok(())
    })
}

#[tauri::command]
fn picker_arrow(app: AppHandle, arrow: String) -> Result<serde_json::Value, String> {
    let arrow = match arrow.as_str() {
        "left" => PickerArrow::Left,
        "right" => PickerArrow::Right,
        "up" => PickerArrow::Up,
        "down" => PickerArrow::Down,
        other => return Err(format!("неизвестное направление: {other}")),
    };
    update_picker(&app, move |session| {
        session.move_arrow(arrow).map_err(|error| error.to_string())
    })
}

/// Копирует ровно показанную строку. При отказе кадр и точка сохраняются.
#[tauri::command]
fn picker_copy(app: AppHandle) -> Result<serde_json::Value, String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "состояние недоступно".to_owned())?;
    let mut picker = state.picker.lock().map_err(|error| error.to_string())?;
    let session = picker
        .as_mut()
        .ok_or_else(|| "кадр не захвачен".to_owned())?;
    // Смена конфигурации дисплеев блокирует копирование недостоверного пикселя.
    if let Ok(geometry) = screen_service().geometry() {
        session.refresh_geometry(&geometry);
    }
    let mut sink = PickerClipboard;
    let outcome = session.copy(&mut sink);
    let view = picker_view(session);
    let copied = matches!(outcome, CopyOutcome::Copied { .. });
    let result = match &outcome {
        CopyOutcome::Copied { text } => {
            logging::write(&format!("пипетка: скопировано {text}"));
            serde_json::json!({ "status": "copied", "text": text, "view": view })
        }
        CopyOutcome::ClipboardBusy { text } => serde_json::json!({
            "status": "busy",
            "text": text,
            "message": "Буфер обмена занят. Повторите копирование.",
            "view": view,
        }),
        CopyOutcome::ClipboardFailed { text, message } => serde_json::json!({
            "status": "failed",
            "text": text,
            "message": message,
            "view": view,
        }),
        CopyOutcome::Blocked { text, reason } => serde_json::json!({
            "status": "blocked",
            "text": text,
            "reason": picker_block_reason(*reason),
            "message": picker_block_message(*reason),
            "view": view,
        }),
        CopyOutcome::NoSelection => serde_json::json!({
            "status": "none",
            "message": "Выбор уже закрыт.",
        }),
    };
    if copied {
        *picker = None;
    }
    drop(picker);
    if copied {
        hide_and_close(&app, PICKER_WINDOW);
    }
    Ok(result)
}

/// Esc закрывает выбор и не меняет буфер обмена.
#[tauri::command]
fn picker_cancel(app: AppHandle) {
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut picker) = state.picker.lock() {
            if let Some(session) = picker.as_mut() {
                session.escape();
            }
            *picker = None;
        }
    }
    hide_and_close(&app, PICKER_WINDOW);
}

#[tauri::command]
fn copy_text(text: String) -> Result<(), String> {
    clipboard::write_text(&text).map_err(|error| error.to_string())
}

#[tauri::command]
fn close_result(app: AppHandle) {
    if let Some(window) = app.get_webview_window(RESULT_WINDOW) {
        let _ = window.close();
    }
    // Текст прочитанного QR больше не показывается: не держим его в памяти.
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut slot) = state.last_result.lock() {
            if slot
                .as_ref()
                .is_some_and(|result| result.action_id == QR_READ_ACTION)
            {
                *slot = None;
            }
        }
    }
}

#[tauri::command]
fn hotkey_state_command(state: tauri::State<HotkeyDemo>) -> Vec<HotkeyStateDto> {
    hotkey_state(&state)
}

#[tauri::command]
fn check_hotkey(
    state: tauri::State<HotkeyDemo>,
    combo: String,
    module_id: String,
    action_id: String,
) -> Result<HotkeyCheckDto, String> {
    let candidate = candidate_for(&combo, &module_id, &action_id);
    let mut registry = state.registry.lock().expect("реестр шорткеев");
    registry
        .check(&candidate)
        .map(check_dto)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn assign_hotkey(
    state: tauri::State<HotkeyDemo>,
    combo: String,
    module_id: String,
    action_id: String,
    old_combo: Option<String>,
    confirmation: Option<HotkeyConfirmationDto>,
) -> Result<Vec<HotkeyStateDto>, String> {
    let candidate = candidate_for(&combo, &module_id, &action_id);
    let confirmation = confirmation.map(|confirmation| {
        let canonical = util_maker_core::hotkeys::Hotkey::parse(&confirmation.combo)
            .map(|hotkey| hotkey.canonical())
            .unwrap_or(confirmation.combo.clone());
        Confirmation {
            combo: canonical,
            expected_owner: confirmation.expected_owner.into(),
        }
    });
    let mut registry = state.registry.lock().expect("реестр шорткеев");
    registry
        .assign(&candidate, confirmation.as_ref())
        .map_err(|error| error.to_string())?;

    // «Изменить» заменяет конкретную привязку, а не добавляет новую.
    if let Some(old_combo) = old_combo {
        let new_canonical = util_maker_core::hotkeys::Hotkey::parse(&combo)
            .map(|hotkey| hotkey.canonical())
            .unwrap_or_else(|_| combo.clone());
        let old_canonical = util_maker_core::hotkeys::Hotkey::parse(&old_combo)
            .map(|hotkey| hotkey.canonical())
            .unwrap_or(old_combo.clone());
        if !old_combo.is_empty() && old_canonical != new_canonical {
            registry
                .unassign(&module_id, &action_id, &old_combo)
                .map_err(|error| error.to_string())?;
        }
    }
    drop(registry);
    Ok(hotkey_state(&state))
}

#[tauri::command]
fn set_hotkey_save_failure(state: tauri::State<HotkeyDemo>, enabled: bool) {
    let mut registry = state.registry.lock().expect("реестр шорткеев");
    registry.store_mut().fail_saves = enabled;
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    logging::write("команда выхода из интерфейса");
    quit_application(&app);
}

/// Завершает хост и не оставляет дочерних процессов модулей.
fn spawn_session_watchdog(app: AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(5));
        let Some(runtime) = app.try_state::<ShortcutRuntime>() else {
            continue;
        };
        if runtime.paused.load(Ordering::SeqCst) {
            continue;
        }
        let probe_lost = {
            let Ok(registrations) = runtime.registrations.lock() else {
                continue;
            };
            let service = runtime.service.lock().expect("служба сочетаний");
            let lost = registrations
                .values()
                .any(|registration| service.register(&registration.combo, 0xBFFF).is_ok());
            if lost {
                service.unregister(0xBFFF);
            }
            lost
        };
        if probe_lost || !runtime.registered.load(Ordering::SeqCst) {
            runtime.unregister_all();
            let registered = runtime.register_all();
            runtime.registered.store(registered, Ordering::SeqCst);
            if registered {
                set_shortcut_status(&app, "зарегистрировано");
            } else {
                set_shortcut_status(&app, "занято другой программой");
            }
        }
        if let Some(tray) = app.tray_by_id("main") {
            let _ = tray.set_visible(true);
        }
    });
}

fn quit_application(app: &AppHandle) {
    logging::write("выход: освобождаю сочетание и завершаю дочерние процессы");
    if let Some(runtime) = app.try_state::<ShortcutRuntime>() {
        runtime.unregister_all();
    }
    if let Some(state) = app.try_state::<AppState>() {
        state.catalog.registry.cancel_all();
    }
    terminate_module_processes();
    app.exit(0);
}

fn terminate_module_processes() {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        for name in [
            "control-module.exe",
            "screenshot-plugin.exe",
            "translator-module.exe",
            "qr-code-module.exe",
            "color-picker-module.exe",
        ] {
            let _ = std::process::Command::new("taskkill")
                .args(["/F", "/IM", name])
                .creation_flags(0x0800_0000)
                .status();
        }
    }
    #[cfg(not(windows))]
    {
        for name in [
            "control-module",
            "screenshot-plugin",
            "translator-module",
            "qr-code-module",
            "color-picker-module",
        ] {
            let _ = std::process::Command::new("pkill")
                .args(["-f", name])
                .status();
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct PersistedState {
    #[serde(default)]
    first_run_done: bool,
    #[serde(default)]
    shortcut: Option<String>,
    #[serde(default)]
    paused: bool,
}

fn persisted_state(app: &AppHandle) -> PersistedState {
    state_path(app)
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_paused(app: &AppHandle, paused: bool) {
    let Ok(path) = state_path(app) else {
        return;
    };
    let mut state = persisted_state(app);
    state.paused = paused;
    if let Ok(text) = serde_json::to_string_pretty(&state) {
        let _ = std::fs::write(path, text);
    }
}

fn save_shortcut(app: &AppHandle, combo: &str) {
    let Ok(path) = state_path(app) else {
        return;
    };
    let mut state = persisted_state(app);
    state.shortcut = Some(combo.to_owned());
    if let Ok(text) = serde_json::to_string_pretty(&state) {
        let _ = std::fs::write(path, text);
    }
}

fn state_path(app: &AppHandle) -> Result<PathBuf, String> {
    let directory = app
        .path()
        .app_local_data_dir()
        .map_err(|error| format!("каталог данных приложения: {error}"))?;
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("каталог {}: {error}", directory.display()))?;
    Ok(directory.join("prototype-state.json"))
}

fn mark_and_check_first_run(app: &AppHandle) -> Result<bool, String> {
    let path = state_path(app)?;
    let mut state = if path.is_file() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    } else {
        PersistedState::default()
    };
    let first_run = !state.first_run_done;
    if first_run {
        state.first_run_done = true;
        let text = serde_json::to_string_pretty(&state)
            .map_err(|error| format!("сохранение состояния: {error}"))?;
        std::fs::write(&path, text)
            .map_err(|error| format!("запись {}: {error}", path.display()))?;
    }
    Ok(first_run)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_directory(name: &str) -> PathBuf {
        let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
        let directory = std::env::temp_dir().join(format!(
            "util-maker-desktop-{name}-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("каталог теста");
        directory
    }

    fn write_module(
        root: &Path,
        id: &str,
        name: &str,
        action_id: &str,
        action_name: &str,
        marker: Option<&Path>,
    ) -> PathBuf {
        let directory = root.join(id);
        std::fs::create_dir_all(&directory).expect("каталог модуля");
        let manifest = format!(
            r#"{{
              "manifestVersion": 1,
              "id": "{id}",
              "publisher": "org.test",
              "name": "{name}",
              "version": "1.0.0",
              "hostApi": ">=1.0.0 <2.0.0",
              "entrypoints": {{ "linux-x64": "module" }},
              "icon": "icon.png",
              "capabilities": [],
              "actions": [{{ "id": "{action_id}", "name": "{action_name}", "recommendedHotkeys": ["Ctrl+Alt+1"] }}]
            }}"#
        );
        std::fs::write(directory.join("manifest.json"), manifest).expect("манифест");
        std::fs::write(directory.join("icon.png"), [0x89, b'P', b'N', b'G']).expect("иконка");
        let executable = directory.join("module");
        let script = match marker {
            Some(marker) => format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
            None => "#!/bin/sh\nexit 0\n".to_owned(),
        };
        std::fs::write(&executable, script).expect("точка входа");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&executable).expect("файл").permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&executable, permissions).expect("права");
        }
        executable
    }

    #[test]
    fn catalog_reads_two_manifests_without_starting_processes() {
        let root = temp_directory("catalog");
        let marker = root.join("spawned");
        write_module(
            &root,
            "org.test.one",
            "Первый",
            "run",
            "Запустить",
            Some(&marker),
        );
        write_module(&root, "org.test.two", "Второй", "ping", "Проверить", None);

        let (entries, errors) = discover_modules(&root, "linux-x64").expect("каталог");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(entries.len(), 2);
        assert!(!marker.exists(), "чтение каталога запустило процесс");

        let registry = Registry::new();
        registry.replace_packages(entries).expect("реестр");
        let actions = registry.actions();
        assert_eq!(actions.len(), 2);
        assert!(actions.iter().any(|action| {
            action.module_id == "org.test.one"
                && action.action_id == "run"
                && action.module_version == "1.0.0"
        }));
        assert!(!marker.exists(), "построение меню запустило процесс");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn removing_package_drops_only_its_saved_shortcuts() {
        let mut map = BTreeMap::from([
            ("org.test.one/run".to_owned(), vec!["Ctrl+Alt+1".to_owned()]),
            (
                "org.test.two/ping".to_owned(),
                vec!["Ctrl+Alt+2".to_owned()],
            ),
        ]);
        drop_module_shortcuts(&mut map, "org.test.one");
        assert!(!map.contains_key("org.test.one/run"));
        assert!(map.contains_key("org.test.two/ping"));
    }

    #[test]
    fn disabled_manifest_disappears_from_action_list() {
        let root = temp_directory("disabled");
        write_module(&root, "org.test.one", "Первый", "run", "Запустить", None);
        write_module(&root, "org.test.two", "Второй", "ping", "Проверить", None);
        let (entries, errors) = discover_modules(&root, "linux-x64").expect("каталог");
        assert!(errors.is_empty(), "{errors:?}");
        let registry = Registry::new();
        registry.replace_packages(entries).expect("реестр");
        registry
            .set_enabled("org.test.one", false)
            .expect("отключение");
        let actions = registry.actions();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].module_id, "org.test.two");
        assert!(!registry.module("org.test.one").expect("модуль").enabled);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn data_url_and_base64_keep_manifest_icon() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
        let root = temp_directory("icon");
        let icon = root.join("icon.png");
        std::fs::write(&icon, b"\x89PNG\r\n\x1a\n").expect("иконка");
        let url = icon_data_url(&icon).expect("data URL");
        assert!(url.starts_with("data:image/png;base64,"));
        assert_eq!(url, "data:image/png;base64,iVBORw0KGgo=");
        let broken = root.join("broken.png");
        std::fs::write(&broken, b"not an image").expect("повреждённый файл");
        assert!(
            icon_data_url(&broken).is_none(),
            "повреждённая иконка не должна попадать в интерфейс"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn menu_icons_are_distinct_and_decode() {
        let app_icon: &[u8] = include_bytes!("../icons/32x32.png");
        let service: [(&str, &[u8]); 5] = [
            ("settings", include_bytes!("../icons/menu-settings.png")),
            ("pause", include_bytes!("../icons/menu-pause.png")),
            ("retry", include_bytes!("../icons/menu-retry.png")),
            ("quit", include_bytes!("../icons/menu-quit.png")),
            ("fallback", include_bytes!("../icons/menu-fallback.png")),
        ];
        for (name, bytes) in service {
            let image = Image::from_bytes(bytes).unwrap_or_else(|error| panic!("{name}: {error}"));
            assert_eq!((image.width(), image.height()), (32, 32), "{name}");
            assert_ne!(bytes, app_icon, "{name} повторяет логотип приложения");
        }
        for (index, (name, bytes)) in service.iter().enumerate() {
            for (other_name, other) in service.iter().skip(index + 1) {
                assert_ne!(bytes, other, "{name} и {other_name} совпадают");
            }
        }
        let screenshot: &[u8] = include_bytes!("../../../plugins/screenshot/assets/icon.png");
        let translator: &[u8] = include_bytes!("../../../plugins/translator/assets/icon.png");
        for (name, bytes) in [("screenshot", screenshot), ("translator", translator)] {
            let image = Image::from_bytes(bytes).unwrap_or_else(|error| panic!("{name}: {error}"));
            assert_eq!((image.width(), image.height()), (128, 128), "{name}");
            assert_ne!(bytes, app_icon, "{name} повторяет логотип приложения");
        }
        assert_ne!(screenshot, translator, "иконки микроприложений совпадают");
    }

    #[test]
    fn pause_menu_label_reflects_tray_state() {
        assert_eq!(pause_menu_label(false), "Пауза сочетания");
        assert_eq!(pause_menu_label(true), "Возобновить сочетание");
    }

    #[test]
    fn overlay_frame_swaps_blue_and_red_channels() {
        assert_eq!(bgra_to_rgba(&[10, 20, 30, 255]), vec![30, 20, 10, 255]);
        assert_eq!(
            bgra_to_rgba(&[1, 2, 3, 4, 5, 6, 7, 8]),
            vec![3, 2, 1, 4, 7, 6, 5, 8]
        );
    }

    #[test]
    fn tray_visual_switches_icon_and_tooltip_by_state() {
        let (active_icon, active_tip) = tray_visual(false, true, "зарегистрировано");
        assert_eq!(
            active_icon,
            include_bytes!("../icons/tray-working.png").as_slice()
        );
        assert!(active_tip.contains("работает"));

        let (paused_icon, paused_tip) = tray_visual(true, true, "на паузе");
        assert_eq!(
            paused_icon,
            include_bytes!("../icons/tray-paused.png").as_slice()
        );
        assert!(paused_tip.contains("на паузе"));

        let (error_icon, error_tip) = tray_visual(false, false, "занято");
        assert_eq!(
            error_icon,
            include_bytes!("../icons/tray-working.png").as_slice()
        );
        assert!(error_tip.contains("недоступно"));
    }

    #[test]
    fn screenshot_save_directory_defaults_and_validates_custom_path() {
        let fallback = temp_directory("screenshot-default");
        assert_eq!(
            resolve_screenshot_directory(None, fallback.clone()).expect("путь по умолчанию"),
            fallback
        );
        assert_eq!(
            resolve_screenshot_directory(Some("   "), fallback.clone()).expect("пустой путь"),
            fallback
        );

        let custom = temp_directory("screenshot-custom");
        assert_eq!(
            resolve_screenshot_directory(Some(custom.to_str().expect("UTF-8")), fallback.clone())
                .expect("пользовательский путь"),
            custom
        );

        let nested = custom.join("nested").join("screenshots");
        let saved = write_screenshot_png(&nested, 1, 1, "ff0000ff").expect("сохранение PNG");
        assert!(saved.starts_with(&nested), "{}", saved.display());
        assert!(saved.is_file());
        assert_eq!(
            std::fs::read(&saved).expect("PNG").get(..8),
            Some([0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a].as_slice())
        );

        let relative = resolve_screenshot_directory(Some("Screenshots"), fallback.clone())
            .expect_err("относительный путь должен отклоняться");
        assert!(relative.contains("полный путь"), "{relative}");

        let file = fallback.join("not-a-directory");
        std::fs::write(&file, b"file").expect("файл");
        let not_directory =
            resolve_screenshot_directory(Some(file.to_str().expect("UTF-8")), fallback.clone())
                .expect_err("путь к файлу должен отклоняться");
        assert!(
            not_directory.contains("не является папкой"),
            "{not_directory}"
        );

        let _ = std::fs::remove_dir_all(fallback);
        let _ = std::fs::remove_dir_all(custom);
    }

    #[test]
    fn persisted_pause_is_optional_and_survives_shortcut_changes() {
        let old: PersistedState =
            serde_json::from_str(r#"{"shortcut":"Ctrl+Alt+1"}"#).expect("старое состояние");
        assert!(!old.paused);
        let paused: PersistedState = serde_json::from_str(
            r#"{"first_run_done":true,"shortcut":"Ctrl+Alt+1","paused":true}"#,
        )
        .expect("состояние с паузой");
        assert!(paused.paused);
        assert_eq!(paused.shortcut.as_deref(), Some("Ctrl+Alt+1"));
    }
}
