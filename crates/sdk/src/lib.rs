//! Публичный SDK микроприложения Util Maker.
//!
//! Крейт не зависит от Tauri, Win32 и графической сессии. Он содержит схему
//! манифеста, сообщения JSON-RPC 2.0 контракта v1 и цикл stdio, который
//! использует процесс микроприложения.

pub mod manifest;
pub mod protocol;
pub mod server;

pub use manifest::{Action, Manifest, ManifestError, PackageFile, MANIFEST_VERSION};
pub use protocol::{
    error_codes, CallContext, InitializeParams, InitializeResult, InvocationSource, InvokeRequest,
    InvokeResult, InvokeStatus, RpcError, HOST_API_VERSION, INITIALIZE_TIMEOUT, JSONRPC_VERSION,
    MAX_MESSAGE_BYTES, SHUTDOWN_TIMEOUT,
};
pub use server::{run_stdio, Module, ModuleInfo, RunError};
