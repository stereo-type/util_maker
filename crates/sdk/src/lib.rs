//! Публичный SDK микроприложения Util Maker.
//!
//! Крейт не зависит от Tauri, Win32 и графической сессии. Он содержит схему
//! манифеста, сообщения JSON-RPC 2.0 контракта v1 и цикл stdio, который
//! использует процесс микроприложения.

pub mod host;
pub mod manifest;
pub mod protocol;
pub mod server;

pub use host::{
    host_methods, BufferRef, DeniedHost, FrameOrigin, FrameRef, FrameRegion, FrameRequest,
    HostBridge, HostCall, HostCallContext, HostCallError, HostHandler, MonitorGeometry,
    ScreenGeometry, FRAME_FORMAT_BGRA8, HOST_CALL_METHOD,
};
pub use manifest::{
    Action, FormCommand, FormState, Manifest, ManifestError, PackageFile, SecretPresence,
    SettingField, SettingKind, SettingOption, SettingsProfile, SettingsSchema, MANIFEST_VERSION,
    SETTINGS_SCHEMA_VERSION,
};
pub use protocol::{
    error_codes, CallContext, FormCommandRequest, FormCommandResult, FormFieldError,
    InitializeParams, InitializeResult, InvocationSource, InvokeRequest, InvokeResult,
    InvokeStatus, RpcError, RpcResponse, HOST_API_VERSION, INITIALIZE_TIMEOUT, JSONRPC_VERSION,
    MAX_MESSAGE_BYTES, SHUTDOWN_TIMEOUT,
};
pub use server::{run_stdio, run_stdio_with_io, Module, ModuleInfo, RunError};
