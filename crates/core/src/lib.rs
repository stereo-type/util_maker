//! Переносимое ядро Util Maker.
//!
//! Крейт не зависит от Tauri, Win32 и графической сессии: он работает в тестах
//! на Ubuntu и используется нативным хостом на границе приложения.

pub mod diagnostics;
pub mod error;
pub mod hotkeys;
pub mod ipc;
pub mod registry;

pub use error::CoreError;
pub use ipc::{Logger, ModuleProcess};
pub use registry::{ActionRef, InvokeOutcome, ModuleEntry, Registry};
pub use util_maker_sdk::{Action, Manifest, ManifestError, HOST_API_VERSION};
