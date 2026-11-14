#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Нативный хост прототипа Util Maker.
//!
//! В трее живёт один экземпляр приложения. Глобальное сочетание или пункт меню
//! запускают контрольный модуль отдельным процессом, согласуют API по JSON-RPC
//! и показывают результат в компактном окне. Настройки создаются по запросу и
//! уничтожаются при закрытии; без окон процесс остаётся в трее.

mod logging;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{
    image::Image,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, WebviewUrl, WebviewWindowBuilder,
};
use util_maker_core::hotkeys::{
    AssignmentRecord, Candidate, CheckResult, Confirmation, HotkeyPlatform, HotkeyRegistry,
    HotkeyStore, Owner, RegistrationError, StoreError,
};
use util_maker_core::{
    ActionRef, HostDispatcher, InvocationContext, InvokeOutcome, ModuleEntry, Registry,
};
use util_maker_platform::{HotkeyEvent, HotkeyService, RegisterFailure};
use util_maker_sdk::{protocol::InvocationSource, Manifest};

const CONTROL_MODULE_ID: &str = "org.utilmaker.control";
const CONTROL_ACTION_ID: &str = "ping";
const CONTROL_SHORTCUT: &str = "Ctrl+Shift+U";
const SETTINGS_WINDOW: &str = "settings";
const RESULT_WINDOW: &str = "result";
const INVOKE_TIMEOUT: Duration = Duration::from_secs(30);
const HOTKEY_ID: u32 = 1;

struct AppState {
    last_result: Mutex<Option<LastResult>>,
    catalog: CatalogState,
    tray_actions: Mutex<BTreeMap<String, ActionRef>>,
}

/// Каталог установленных модулей и реестр ядра.
struct CatalogState {
    root: PathBuf,
    registry: Registry,
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
        let registry = Registry::with_state_path(data_directory.join("packages.json"))
            .map_err(|error| format!("состояние пакетов: {error}"))?;
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
            ("Ctrl+Shift+1", "org.utilmaker.screenshot", "capture-region"),
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

/// Регистрация прототипа: одно сочетание, пауза и повторная попытка.
struct ShortcutRuntime {
    service: Mutex<HotkeyService>,
    combo: Mutex<String>,
    paused: AtomicBool,
    registered: AtomicBool,
    status: Mutex<String>,
}

impl ShortcutRuntime {
    fn start(app: &AppHandle) -> Result<(Self, Receiver<HotkeyEvent>), String> {
        let mut service = HotkeyService::start()?;
        let events = service.take_events();
        let combo = persisted_shortcut(app).unwrap_or_else(|| CONTROL_SHORTCUT.to_owned());
        let (registered, status) = register_shortcut(&service, &combo);
        Ok((
            Self {
                service: Mutex::new(service),
                combo: Mutex::new(combo),
                paused: AtomicBool::new(false),
                registered: AtomicBool::new(registered),
                status: Mutex::new(status),
            },
            events,
        ))
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
    build_number: String,
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
            refresh_plugins,
            get_last_result,
            open_settings_command,
            close_settings,
            close_result,
            get_autostart,
            set_autostart,
            hotkey_state_command,
            check_hotkey,
            assign_hotkey,
            set_hotkey_save_failure,
            plugins,
            set_plugin_enabled,
            change_control_shortcut,
            quit_app
        ])
        .setup(|app| {
            let state = AppState {
                last_result: Mutex::new(None),
                catalog: CatalogState::open(app.handle())?,
                tray_actions: Mutex::new(BTreeMap::new()),
            };
            app.manage(state);
            let (runtime, events) = ShortcutRuntime::start(app.handle())?;
            let status = runtime.status.lock().expect("статус сочетания").clone();
            app.manage(runtime);
            app.manage(HotkeyDemo::new());
            setup_tray(app)?;
            set_shortcut_status(app.handle(), &status);
            spawn_hotkey_listener(app.handle().clone(), events);
            let first_run = mark_and_check_first_run(app.handle())?;
            if first_run {
                logging::write("первый запуск: открываю настройки");
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

fn setup_tray(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let icon = Image::from_bytes(include_bytes!("../icons/32x32.png"))?;

    TrayIconBuilder::with_id("main")
        .icon(icon)
        .tooltip("Util Maker — прототип")
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
fn rebuild_tray_menu(app: &AppHandle) -> Result<(), String> {
    let actions = app
        .try_state::<AppState>()
        .map(|state| state.catalog.registry.actions())
        .unwrap_or_default();
    let menu = Menu::new(app).map_err(|error| format!("меню трея: {error}"))?;
    let settings = MenuItem::with_id(app, "settings", "Настройки", true, None::<&str>)
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
            let item = MenuItem::with_id(
                app,
                format!("action:{index}"),
                format!("{} — {}", action.module_name, action.action_name),
                action.enabled,
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
        ("pause", "Пауза / возобновить сочетание"),
        ("retry", "Перерегистрировать сочетание"),
        ("quit", "Выход"),
    ] {
        let item = MenuItem::with_id(app, id, label, true, None::<&str>)
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
        .center()
        .build()
    {
        Ok(window) => {
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

fn show_result(app: &AppHandle, result: LastResult) {
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut slot) = state.last_result.lock() {
            *slot = Some(result);
        }
    }
    if let Some(window) = app.get_webview_window(RESULT_WINDOW) {
        let _ = window.eval("window.__umRefresh && window.__umRefresh()");
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    let builder =
        WebviewWindowBuilder::new(app, RESULT_WINDOW, WebviewUrl::App("result.html".into()))
            .title("Util Maker — результат")
            .inner_size(520.0, 320.0)
            .min_inner_size(420.0, 260.0)
            .resizable(false)
            .always_on_top(true)
            .center();
    match builder.build() {
        Ok(_) => logging::write("окно результата открыто"),
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

fn invoke_action(
    app: &AppHandle,
    target: &ActionRef,
    source: InvocationSource,
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
    let mut host = HostDispatcher::new(util_maker_platform::screen_service());
    state
        .catalog
        .registry
        .invoke_session(
            target,
            InvocationContext::new(source),
            INVOKE_TIMEOUT,
            Some(logging::logger()),
            &mut host,
        )
        .map_err(|failure| failure.message)
}

fn invoke_and_show(app: &AppHandle, target: ActionRef, source: InvocationSource) {
    let started = Instant::now();
    match invoke_action(app, &target, source) {
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
                    action_name: outcome.action_name.clone(),
                    source: source.as_str().to_owned(),
                    core_elapsed_ms: outcome.elapsed.as_millis(),
                    since_event_ms: since_event.as_millis(),
                    status: "ok".to_owned(),
                },
            );
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
                    action_name: action_name_for(app, &target),
                    source: source.as_str().to_owned(),
                    core_elapsed_ms: 0,
                    since_event_ms: started.elapsed().as_millis(),
                    status: "error".to_owned(),
                },
            );
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
                let handle = app.clone();
                let _ = app.run_on_main_thread(move || {
                    invoke_and_show(&handle, control_action_ref(), InvocationSource::Hotkey);
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

fn apply_tray_state(app: &AppHandle) {
    let (icon, tooltip) = if let Some(runtime) = app.try_state::<ShortcutRuntime>() {
        if runtime.paused.load(Ordering::SeqCst) {
            (
                include_bytes!("../icons/status-paused.png").as_slice(),
                "Util Maker — на паузе".to_owned(),
            )
        } else if runtime.registered.load(Ordering::SeqCst) {
            (
                include_bytes!("../icons/status-active.png").as_slice(),
                "Util Maker — работает".to_owned(),
            )
        } else {
            let status = runtime
                .status
                .lock()
                .ok()
                .map(|value| value.clone())
                .unwrap_or_default();
            (
                include_bytes!("../icons/status-error.png").as_slice(),
                format!("Util Maker — сочетание недоступно: {status}"),
            )
        }
    } else {
        (
            include_bytes!("../icons/status-error.png").as_slice(),
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

fn toggle_shortcut_pause(app: &AppHandle) {
    logging::debug("меню: переключение паузы сочетания");
    let Some(runtime) = app.try_state::<ShortcutRuntime>() else {
        return;
    };
    if runtime.paused.load(Ordering::SeqCst) {
        let (registered, status) = {
            let combo = runtime.combo.lock().expect("сочетание").clone();
            let service = runtime.service.lock().expect("служба сочетаний");
            register_shortcut(&service, &combo)
        };
        runtime.registered.store(registered, Ordering::SeqCst);
        if registered {
            runtime.paused.store(false, Ordering::SeqCst);
        }
        set_shortcut_status(app, &status);
    } else {
        // Сначала закрываем обработку событий, затем освобождаем регистрацию:
        // иначе уже поставленное в очередь WM_HOTKEY успеет вызвать действие.
        runtime.paused.store(true, Ordering::SeqCst);
        runtime
            .service
            .lock()
            .expect("служба сочетаний")
            .unregister(HOTKEY_ID);
        runtime.registered.store(false, Ordering::SeqCst);
        set_shortcut_status(app, "на паузе");
    }
}

fn retry_shortcut(app: &AppHandle) {
    logging::debug("меню: повторная регистрация сочетания");
    let Some(runtime) = app.try_state::<ShortcutRuntime>() else {
        return;
    };
    if runtime.paused.load(Ordering::SeqCst) {
        set_shortcut_status(app, "на паузе");
        return;
    }
    if runtime.registered.load(Ordering::SeqCst) {
        set_shortcut_status(app, "уже зарегистрировано");
        return;
    }
    let (registered, status) = {
        let combo = runtime.combo.lock().expect("сочетание").clone();
        let service = runtime.service.lock().expect("служба сочетаний");
        register_shortcut(&service, &combo)
    };
    runtime.registered.store(registered, Ordering::SeqCst);
    if registered {
        runtime.paused.store(false, Ordering::SeqCst);
    }
    set_shortcut_status(app, &status);
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
                actions: entry
                    .manifest
                    .actions
                    .iter()
                    .map(|action| PluginActionDto {
                        id: action.id.clone(),
                        name: action.name.clone(),
                        description: action.description().to_owned(),
                        hotkey: action.recommended_hotkeys.first().cloned(),
                    })
                    .collect(),
            }
        })
        .collect()
}

fn refresh_plugins_inner(app: &AppHandle) -> Vec<PluginDto> {
    if let Some(state) = app.try_state::<AppState>() {
        state.catalog.refresh();
    }
    if let Err(error) = rebuild_tray_menu(app) {
        logging::write(&format!("меню трея: {error}"));
    }
    collect_plugins(app)
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
        rebuild_tray_menu(&app)?;
    }
    Ok(())
}

/// Небольшой PNG-значок из манифеста передаётся в WebView как data URL.
fn icon_data_url(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > 1024 * 1024 {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
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
            state.registered.store(true, Ordering::SeqCst);
            state.paused.store(false, Ordering::SeqCst);
            drop(service);
            set_shortcut_status(&app, "зарегистрировано");
            save_shortcut(&app, &normalized);
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
        build_number: logging::build_number().to_owned(),
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
    let parsed = match source.as_deref() {
        Some("hotkey") => InvocationSource::Hotkey,
        _ => InvocationSource::Tray,
    };
    let handle = app.clone();
    let call_target = target.clone();
    let started = Instant::now();
    let outcome =
        tauri::async_runtime::spawn_blocking(move || invoke_action(&handle, &call_target, parsed))
            .await
            .map_err(|error| format!("задача вызова прервана: {error}"))??;
    let last = LastResult {
        message: outcome.message.clone(),
        action_name: outcome.action_name.clone(),
        source: source.unwrap_or_else(|| "settings".to_owned()),
        core_elapsed_ms: outcome.elapsed.as_millis(),
        since_event_ms: started.elapsed().as_millis(),
        status: format!("{:?}", outcome.status).to_lowercase(),
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

#[tauri::command]
fn close_result(app: AppHandle) {
    if let Some(window) = app.get_webview_window(RESULT_WINDOW) {
        let _ = window.close();
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
fn quit_application(app: &AppHandle) {
    logging::write("выход: освобождаю сочетание и завершаю дочерние процессы");
    if let Some(runtime) = app.try_state::<ShortcutRuntime>() {
        runtime
            .service
            .lock()
            .expect("служба сочетаний")
            .unregister(HOTKEY_ID);
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
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/IM", "control-module.exe"])
            .creation_flags(0x0800_0000)
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("pkill")
            .args(["-f", "control-module"])
            .status();
    }
}

#[derive(Default, Serialize, Deserialize)]
struct PersistedState {
    #[serde(default)]
    first_run_done: bool,
    #[serde(default)]
    shortcut: Option<String>,
}

fn persisted_state(app: &AppHandle) -> PersistedState {
    state_path(app)
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn persisted_shortcut(app: &AppHandle) -> Option<String> {
    persisted_state(app).shortcut
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
        std::fs::write(&icon, [0x89, b'P', b'N', b'G']).expect("иконка");
        let url = icon_data_url(&icon).expect("data URL");
        assert!(url.starts_with("data:image/png;base64,"));
        assert_eq!(url, "data:image/png;base64,iVBORw==");
        let _ = std::fs::remove_dir_all(root);
    }
}
