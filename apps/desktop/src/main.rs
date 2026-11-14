#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Нативный хост прототипа Util Maker.
//!
//! В трее живёт один экземпляр приложения. Глобальное сочетание или пункт меню
//! запускают контрольный модуль отдельным процессом, согласуют API по JSON-RPC
//! и показывают результат в компактном окне. Настройки создаются по запросу и
//! уничтожаются при закрытии; без окон процесс остаётся в трее.

mod logging;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{
    image::Image,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, WebviewUrl, WebviewWindowBuilder,
};
use util_maker_core::{ActionRef, InvokeOutcome, Registry};
use util_maker_platform::{HotkeyService, RegisterFailure};
use util_maker_sdk::{protocol::InvocationSource, Manifest};

const CONTROL_MODULE_ID: &str = "org.utilmaker.control";
const CONTROL_ACTION_ID: &str = "ping";
const CONTROL_SHORTCUT: &str = "Ctrl+Shift+U";
const SETTINGS_WINDOW: &str = "settings";
const RESULT_WINDOW: &str = "result";
const INVOKE_TIMEOUT: Duration = Duration::from_secs(30);
const HOTKEY_ID: u32 = 1;

#[derive(Default)]
struct AppState {
    last_result: Mutex<Option<LastResult>>,
}

/// Регистрация прототипа: одно сочетание, пауза и повторная попытка.
struct ShortcutRuntime {
    service: Mutex<HotkeyService>,
    paused: AtomicBool,
    registered: AtomicBool,
    status: Mutex<String>,
}

impl ShortcutRuntime {
    fn start() -> Result<Self, String> {
        let service = HotkeyService::start()?;
        let (registered, status) = register_shortcut(&service);
        Ok(Self {
            service: Mutex::new(service),
            paused: AtomicBool::new(false),
            registered: AtomicBool::new(registered),
            status: Mutex::new(status),
        })
    }
}

fn register_shortcut(service: &HotkeyService) -> (bool, String) {
    match service.register(CONTROL_SHORTCUT, HOTKEY_ID) {
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
    module_path: String,
    log_path: String,
    config_path: String,
}

fn main() {
    logging::init();
    logging::write("запуск Util Maker (прототип)");
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            logging::write("повторный запуск: показываю настройки существующего экземпляра");
            open_settings(app);
        }))
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            get_status,
            invoke_control,
            get_last_result,
            open_settings_command,
            close_settings,
            close_result,
            quit_app
        ])
        .setup(|app| {
            setup_tray(app)?;
            let runtime = ShortcutRuntime::start()?;
            let status = runtime.status.lock().expect("статус сочетания").clone();
            app.manage(runtime);
            set_shortcut_status(app.handle(), &status);
            spawn_hotkey_listener(app.handle().clone());
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
    let settings = MenuItem::with_id(app, "settings", "Настройки", true, None::<&str>)?;
    let invoke = MenuItem::with_id(app, "invoke", "Проверить связь", true, None::<&str>)?;
    let pause = MenuItem::with_id(
        app,
        "pause",
        "Пауза / возобновить сочетание",
        true,
        None::<&str>,
    )?;
    let retry = MenuItem::with_id(
        app,
        "retry",
        "Перерегистрировать сочетание",
        true,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let second_separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(
        app,
        &[
            &settings,
            &invoke,
            &separator,
            &pause,
            &retry,
            &second_separator,
            &quit,
        ],
    )?;
    let icon = Image::from_bytes(include_bytes!("../icons/32x32.png"))?;

    TrayIconBuilder::with_id("main")
        .icon(icon)
        .tooltip("Util Maker — прототип")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "settings" => {
                logging::write("меню трея: настройки");
                open_settings(app);
            }
            "invoke" => {
                logging::write("меню трея: проверка связи");
                invoke_and_show(app, InvocationSource::Tray);
            }
            "pause" => toggle_shortcut_pause(app),
            "retry" => retry_shortcut(app),
            "quit" => quit_application(app),
            _ => {}
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
    Ok(())
}

fn open_settings(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(SETTINGS_WINDOW) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    match WebviewWindowBuilder::new(app, SETTINGS_WINDOW, WebviewUrl::App("index.html".into()))
        .title("Util Maker — настройки")
        .inner_size(700.0, 620.0)
        .min_inner_size(560.0, 460.0)
        .center()
        .build()
    {
        Ok(_) => logging::write("окно настроек открыто"),
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

fn invoke_and_show(app: &AppHandle, source: InvocationSource) {
    let started = Instant::now();
    match run_control_action(app, source) {
        Ok(outcome) => {
            let since_event = started.elapsed();
            logging::write(&format!(
                "hotkey->result source={} core_elapsed_ms={} since_event_ms={}",
                source.as_str(),
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
                "ошибка вызова source={}: {error}",
                source.as_str()
            ));
            show_result(
                app,
                LastResult {
                    message: format!("Не удалось выполнить действие: {error}"),
                    action_name: "Проверить связь".to_owned(),
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
fn spawn_hotkey_listener(app: AppHandle) {
    std::thread::spawn(move || loop {
        let event = {
            let Some(runtime) = app.try_state::<ShortcutRuntime>() else {
                break;
            };
            let service = runtime.service.lock().expect("служба сочетаний");
            service.recv_timeout(Duration::from_millis(250))
        };
        match event {
            Ok(event) if event.pressed => {
                let handle = app.clone();
                let _ = app.run_on_main_thread(move || {
                    invoke_and_show(&handle, InvocationSource::Hotkey);
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
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(format!("Util Maker — {status}")));
    }
}

fn toggle_shortcut_pause(app: &AppHandle) {
    let Some(runtime) = app.try_state::<ShortcutRuntime>() else {
        return;
    };
    if runtime.paused.load(Ordering::SeqCst) {
        let (registered, status) = {
            let service = runtime.service.lock().expect("служба сочетаний");
            register_shortcut(&service)
        };
        runtime.registered.store(registered, Ordering::SeqCst);
        if registered {
            runtime.paused.store(false, Ordering::SeqCst);
        }
        set_shortcut_status(app, &status);
    } else {
        runtime
            .service
            .lock()
            .expect("служба сочетаний")
            .unregister(HOTKEY_ID);
        runtime.registered.store(false, Ordering::SeqCst);
        runtime.paused.store(true, Ordering::SeqCst);
        set_shortcut_status(app, "на паузе");
    }
}

fn retry_shortcut(app: &AppHandle) {
    let Some(runtime) = app.try_state::<ShortcutRuntime>() else {
        return;
    };
    if runtime.registered.load(Ordering::SeqCst) {
        set_shortcut_status(app, "уже зарегистрировано");
        return;
    }
    let (registered, status) = {
        let service = runtime.service.lock().expect("служба сочетаний");
        register_shortcut(&service)
    };
    runtime.registered.store(registered, Ordering::SeqCst);
    if registered {
        runtime.paused.store(false, Ordering::SeqCst);
    }
    set_shortcut_status(app, &status);
}

fn run_control_action(app: &AppHandle, source: InvocationSource) -> Result<InvokeOutcome, String> {
    let directory = module_directory(app)?;
    let manifest_path = directory.join("manifest.json");
    let manifest = Manifest::load(&manifest_path)
        .map_err(|error| format!("манифест {}: {error}", manifest_path.display()))?;
    let entrypoint = manifest
        .resolved_entrypoint(platform_key())
        .map_err(|error| format!("точка входа модуля: {error}"))?;
    let executable = directory.join(entrypoint);
    if !executable.is_file() {
        return Err(format!(
            "не найден исполняемый файл модуля: {}",
            executable.display()
        ));
    }
    let mut registry = Registry::new();
    registry
        .add(manifest, executable)
        .map_err(|error| error.to_string())?;
    registry
        .invoke(
            &ActionRef::new(CONTROL_MODULE_ID, CONTROL_ACTION_ID),
            source,
            INVOKE_TIMEOUT,
            Some(logging::logger()),
        )
        .map_err(|error| error.to_string())
}

fn module_directory(app: &AppHandle) -> Result<PathBuf, String> {
    if let Some(directory) = std::env::var_os("UTIL_MAKER_MODULES_DIR") {
        return Ok(PathBuf::from(directory).join(CONTROL_MODULE_ID));
    }
    let executable =
        std::env::current_exe().map_err(|error| format!("путь приложения: {error}"))?;
    let beside_exe = executable
        .parent()
        .ok_or_else(|| "у приложения нет каталога".to_owned())?
        .join("modules")
        .join(CONTROL_MODULE_ID);
    if beside_exe.is_dir() {
        return Ok(beside_exe);
    }
    let resources = app
        .path()
        .resource_dir()
        .map_err(|error| format!("каталог ресурсов: {error}"))?;
    Ok(resources.join("modules").join(CONTROL_MODULE_ID))
}

/// Ключ платформы в манифесте пакета.
fn platform_key() -> &'static str {
    if cfg!(windows) {
        "windows-x64"
    } else {
        "linux-x64"
    }
}

#[tauri::command]
fn get_status(app: AppHandle) -> Result<StatusPayload, String> {
    let directory = module_directory(&app)?;
    let manifest_path = directory.join("manifest.json");
    let manifest = Manifest::load(&manifest_path)
        .map_err(|error| format!("манифест {}: {error}", manifest_path.display()))?;
    let action = manifest
        .action(CONTROL_ACTION_ID)
        .ok_or_else(|| format!("в манифесте нет действия {CONTROL_ACTION_ID}"))?;
    let config_path = state_path(&app)
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    Ok(StatusPayload {
        module_id: manifest.id.clone(),
        module_name: manifest.name.clone(),
        module_version: manifest.version.clone(),
        action_id: action.id.clone(),
        action_name: action.name.clone(),
        shortcut: CONTROL_SHORTCUT.to_owned(),
        shortcut_status: app
            .try_state::<ShortcutRuntime>()
            .and_then(|runtime| runtime.status.lock().ok().map(|status| status.clone()))
            .unwrap_or_else(|| "неизвестно".to_owned()),
        module_path: directory.display().to_string(),
        log_path: logging::log_path(),
        config_path,
    })
}

#[tauri::command]
async fn invoke_control(app: AppHandle, source: Option<String>) -> Result<LastResult, String> {
    let parsed = match source.as_deref() {
        Some("hotkey") => InvocationSource::Hotkey,
        _ => InvocationSource::Tray,
    };
    let handle = app.clone();
    let started = Instant::now();
    let outcome = tauri::async_runtime::spawn_blocking(move || run_control_action(&handle, parsed))
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
