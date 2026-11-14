//! Переносимое ядро Util Maker.
//!
//! Крейт не зависит от Tauri, Win32 и графической сессии: он работает в тестах
//! на Ubuntu и используется нативным хостом на границе приложения.

pub mod capabilities;
pub mod diagnostics;
pub mod error;
pub mod failure;
pub mod hotkeys;
pub mod ipc;
pub mod metrics;
pub mod registry;

pub use capabilities::{
    CallSession, HostDispatcher, HostSession, InvocationContext, PlatformHost, SessionHost,
    SourceRef,
};
pub use error::CoreError;
pub use failure::{FailureCategory, InvokeFailure};
pub use ipc::{Logger, ModuleProcess};
pub use metrics::{ActionMetrics, MetricsSummary, Percentiles};
pub use registry::{ActionListing, ActionRef, InvokeOutcome, ModuleEntry, Registry};
pub use util_maker_sdk::{Action, Manifest, ManifestError, HOST_API_VERSION};
