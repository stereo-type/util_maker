//! Регистрация через Win32 `RegisterHotKey` в отдельном потоке сообщений.

use std::collections::{BTreeMap, VecDeque};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use util_maker_core::hotkeys::Hotkey;
use windows::core::w;
use windows::Win32::Foundation::{
    GetLastError, ERROR_HOTKEY_ALREADY_REGISTERED, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, PostMessageW,
    PostQuitMessage, RegisterClassW, TranslateMessage, HWND_MESSAGE, MSG, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_APP, WM_DESTROY, WM_HOTKEY, WNDCLASSW,
};

use crate::{virtual_key, HotkeyEvent, RegisterFailure};

const SERVICE_MESSAGE: u32 = WM_APP + 1;

enum Command {
    Register {
        hotkey: Hotkey,
        id: u32,
        reply: Sender<Result<(), RegisterFailure>>,
    },
    Unregister {
        id: u32,
    },
    Shutdown,
}

struct WindowContext {
    commands: Arc<Mutex<VecDeque<Command>>>,
    events: Sender<HotkeyEvent>,
}

static CONTEXTS: OnceLock<Mutex<BTreeMap<isize, Arc<WindowContext>>>> = OnceLock::new();

fn contexts() -> &'static Mutex<BTreeMap<isize, Arc<WindowContext>>> {
    CONTEXTS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub struct Service {
    handle: isize,
    commands: Arc<Mutex<VecDeque<Command>>>,
    events: Option<Receiver<HotkeyEvent>>,
    thread: Option<JoinHandle<()>>,
}

impl Service {
    pub fn start() -> Result<Self, String> {
        let commands = Arc::new(Mutex::new(VecDeque::new()));
        let thread_commands = commands.clone();
        let (handle_sender, handle_receiver) = mpsc::channel();
        let (event_sender, events) = mpsc::channel();
        let thread = thread::spawn(move || {
            window_thread(handle_sender, thread_commands, event_sender);
        });
        match handle_receiver.recv_timeout(Duration::from_secs(3)) {
            Ok(handle) if handle != 0 => Ok(Self {
                handle,
                commands,
                events: Some(events),
                thread: Some(thread),
            }),
            Ok(_) => Err("не удалось создать окно сообщений Windows".to_owned()),
            Err(error) => Err(format!("поток сочетаний не запустился: {error}")),
        }
    }

    pub fn register(&self, hotkey: &Hotkey, id: u32) -> Result<(), RegisterFailure> {
        let (reply, response) = mpsc::channel();
        self.commands
            .lock()
            .expect("очередь команд")
            .push_back(Command::Register {
                hotkey: hotkey.clone(),
                id,
                reply,
            });
        self.post();
        match response.recv_timeout(Duration::from_secs(1)) {
            Ok(result) => result,
            Err(_) => Err(RegisterFailure::Unavailable(
                "поток сочетаний не ответил".to_owned(),
            )),
        }
    }

    pub fn unregister(&self, id: u32) {
        self.commands
            .lock()
            .expect("очередь команд")
            .push_back(Command::Unregister { id });
        self.post();
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<HotkeyEvent, RecvTimeoutError> {
        self.events
            .as_ref()
            .expect("события уже забраны")
            .recv_timeout(timeout)
    }

    pub fn take_events(&mut self) -> Receiver<HotkeyEvent> {
        self.events.take().expect("события уже забраны")
    }

    fn post(&self) {
        let hwnd = HWND(self.handle as *mut core::ffi::c_void);
        let _ = unsafe { PostMessageW(Some(hwnd), SERVICE_MESSAGE, WPARAM(0), LPARAM(0)) };
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.commands
            .lock()
            .expect("очередь команд")
            .push_back(Command::Shutdown);
        self.post();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn window_thread(
    handle_sender: Sender<isize>,
    commands: Arc<Mutex<VecDeque<Command>>>,
    events: Sender<HotkeyEvent>,
) {
    let module = match unsafe { GetModuleHandleW(None) } {
        Ok(module) => module,
        Err(_) => {
            let _ = handle_sender.send(0);
            return;
        }
    };
    let instance = HINSTANCE(module.0);
    let class_name = w!("UtilMakerHotkeysWindow");
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        lpszClassName: class_name,
        ..Default::default()
    };
    unsafe {
        RegisterClassW(&class);
    }

    let created = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            w!("Util Maker hotkeys"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance),
            None,
        )
    };
    let hwnd = match created {
        Ok(hwnd) => hwnd,
        Err(_) => {
            let _ = handle_sender.send(0);
            return;
        }
    };
    let key = hwnd.0 as isize;
    contexts()
        .lock()
        .expect("контексты")
        .insert(key, Arc::new(WindowContext { commands, events }));
    let _ = handle_sender.send(key);

    let mut message = MSG::default();
    loop {
        let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
        if result.0 <= 0 {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let key = hwnd.0 as isize;
    match message {
        SERVICE_MESSAGE => {
            drain_commands(hwnd);
            LRESULT(0)
        }
        WM_HOTKEY => {
            if let Some(context) = contexts().lock().expect("контексты").get(&key).cloned()
            {
                let _ = context.events.send(HotkeyEvent {
                    id: wparam.0 as u32,
                    pressed: true,
                });
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            contexts().lock().expect("контексты").remove(&key);
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn drain_commands(hwnd: HWND) {
    let context = contexts()
        .lock()
        .expect("контексты")
        .get(&(hwnd.0 as isize))
        .cloned();
    let Some(context) = context else {
        return;
    };
    loop {
        let command = context.commands.lock().expect("очередь команд").pop_front();
        match command {
            Some(Command::Register { hotkey, id, reply }) => {
                let _ = reply.send(register_hotkey(hwnd, &hotkey, id));
            }
            Some(Command::Unregister { id }) => unsafe {
                let _ = UnregisterHotKey(Some(hwnd), id as i32);
            },
            Some(Command::Shutdown) => {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                return;
            }
            None => return,
        }
    }
}

fn register_hotkey(hwnd: HWND, hotkey: &Hotkey, id: u32) -> Result<(), RegisterFailure> {
    let virtual_key = virtual_key(hotkey).ok_or_else(|| {
        RegisterFailure::Unavailable(format!("нет виртуального кода для {}", hotkey.key))
    })?;
    let mut modifiers = HOT_KEY_MODIFIERS(0);
    if hotkey.ctrl {
        modifiers.0 |= MOD_CONTROL.0;
    }
    if hotkey.alt {
        modifiers.0 |= MOD_ALT.0;
    }
    if hotkey.shift {
        modifiers.0 |= MOD_SHIFT.0;
    }
    if hotkey.win {
        modifiers.0 |= MOD_WIN.0;
    }
    match unsafe { RegisterHotKey(Some(hwnd), id as i32, modifiers, virtual_key) } {
        Ok(()) => Ok(()),
        Err(_) => {
            let code = unsafe { GetLastError() };
            if code == ERROR_HOTKEY_ALREADY_REGISTERED {
                Err(RegisterFailure::Occupied)
            } else {
                Err(RegisterFailure::Unavailable(format!(
                    "Win32 RegisterHotKey: код {}",
                    code.0
                )))
            }
        }
    }
}
