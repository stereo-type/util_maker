//! Исполняемый процесс плагина «QR-коды».
//!
//! Действие `show-qr` получает выделение через `selection.get` до показа окна
//! либо открывает ручное поле. Матрица и целый масштаб возвращаются хосту;
//! системный буфер обмена показ QR не изменяет.

use serde_json::{json, Value};
use util_maker_qr_code::{
    ManualReason, SelectionRead, ShowError, ShowWindow, WindowSize, DEFAULT_WINDOW_HEIGHT,
    DEFAULT_WINDOW_WIDTH,
};
use util_maker_sdk::{
    error_codes, host_methods, run_stdio, HostBridge, HostCallError, InvokeRequest, InvokeResult,
    Module, ModuleInfo, RpcError, HOST_API_VERSION,
};

/// Стабильный ID модуля из манифеста.
const MODULE_ID: &str = "org.utilmaker.qr-code";
/// Действие показа QR из текста.
const SHOW_QR_ACTION: &str = "show-qr";
/// Действие чтения QR из области экрана. Handler появится в следующей карточке.
const READ_QR_ACTION: &str = "read-qr-region";
/// Возможность чтения выделения.
const SELECTION_CAPABILITY: &str = "selection.read";
/// Возможность захвата кадра нужна второму действию.
const SCREEN_CAPABILITY: &str = "screen.capture";
/// Возможность буфера обмена нужна второму действию.
const CLIPBOARD_CAPABILITY: &str = "clipboard.write";
/// Код причины, когда чтение QR ещё не подключено.
const READ_UNAVAILABLE: i64 = 2001;

const CAPABILITIES: &[&str] = &[
    SELECTION_CAPABILITY,
    SCREEN_CAPABILITY,
    CLIPBOARD_CAPABILITY,
];

const MODULE_VERSION: &str = match option_env!("UTIL_MAKER_MODULE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

struct QrCodeModule {
    info: ModuleInfo,
    window: Option<ShowWindow>,
}

impl QrCodeModule {
    fn new() -> Self {
        Self {
            info: ModuleInfo {
                id: MODULE_ID,
                version: MODULE_VERSION,
                host_api: HOST_API_VERSION,
                capabilities: CAPABILITIES,
            },
            window: None,
        }
    }

    fn result(request: &InvokeRequest, window: &ShowWindow, outcome: &str) -> InvokeResult {
        let view = window.view();
        let mut data = serde_json::to_value(&view).unwrap_or_else(|_| json!({}));
        if let Value::Object(ref mut object) = data {
            object.insert("callId".to_owned(), json!(request.call_id));
            object.insert("actionId".to_owned(), json!(request.action_id));
            object.insert("outcome".to_owned(), json!(outcome));
            object.insert("source".to_owned(), json!(request.context.source.as_str()));
            object.insert("moduleVersion".to_owned(), json!(MODULE_VERSION));
            object.insert("clipboardChanged".to_owned(), json!(false));
        }
        InvokeResult::ok(view.message).with_data(data)
    }

    fn outcome_name(window: &ShowWindow) -> &'static str {
        if !window.is_open() {
            "closed"
        } else if window.qr().is_some() {
            "showing"
        } else if matches!(
            window.error(),
            Some(ShowError::TooLong { .. } | ShowError::TooSmall { .. })
        ) {
            "invalid-input"
        } else {
            "manual-input"
        }
    }

    fn handle(
        &mut self,
        request: &InvokeRequest,
        host: Option<&mut dyn HostBridge>,
    ) -> Result<InvokeResult, RpcError> {
        let size = WindowSize::from_params(&request.params);
        let manual = request
            .params
            .get("manualText")
            .and_then(Value::as_str)
            .map(str::to_owned);

        // Повторный вызов активирует открытое окно и не читает выделение заново.
        if let Some(window) = self.window.as_mut() {
            if window.is_open() {
                let mut changed = false;
                if let Some(text) = manual {
                    window.edit(text);
                    changed = true;
                }
                if window.size() != size {
                    window.resize(size.width, size.height);
                    changed = true;
                }
                let outcome = if changed { "updated" } else { "already-active" };
                return Ok(Self::result(request, window, outcome));
            }
        }

        let window = if let Some(text) = manual {
            ShowWindow::manual_text(text, size)
        } else {
            match host {
                Some(host) => {
                    match host.request(SELECTION_CAPABILITY, host_methods::SELECTION_GET, json!({}))
                    {
                        Ok(value) => {
                            ShowWindow::from_selection(SelectionRead::from_response(&value), size)
                        }
                        Err(HostCallError::Cancelled | HostCallError::Shutdown) => {
                            return Ok(InvokeResult::cancelled().with_data(json!({
                                "callId": request.call_id,
                                "actionId": request.action_id,
                                "outcome": "cancelled",
                                "moduleVersion": MODULE_VERSION,
                            })));
                        }
                        Err(error) => ShowWindow::manual(
                            ManualReason::SelectionUnavailable,
                            Some(error.to_string()),
                            size,
                        ),
                    }
                }
                None => ShowWindow::manual(
                    ManualReason::SelectionUnavailable,
                    Some("мост возможностей хоста недоступен".to_owned()),
                    size,
                ),
            }
        };

        let outcome = Self::outcome_name(&window);
        let result = Self::result(request, &window, outcome);
        self.window = Some(window);
        Ok(result)
    }

    fn read_unavailable(request: &InvokeRequest) -> RpcError {
        RpcError::new(
            READ_UNAVAILABLE,
            "чтение QR с экрана ещё не подключено в этом процессе",
        )
        .with_data(json!({
            "callId": request.call_id,
            "actionId": request.action_id,
            "outcome": "blocked",
            "reason": "read-qr-not-implemented",
            "capabilities": [SCREEN_CAPABILITY, CLIPBOARD_CAPABILITY],
            "moduleVersion": MODULE_VERSION,
        }))
    }
}

impl Module for QrCodeModule {
    fn info(&self) -> &ModuleInfo {
        &self.info
    }

    fn invoke(&mut self, request: &InvokeRequest) -> Result<InvokeResult, RpcError> {
        match request.action_id.as_str() {
            SHOW_QR_ACTION => self.handle(request, None),
            READ_QR_ACTION => Err(Self::read_unavailable(request)),
            other => Err(RpcError::new(
                error_codes::ACTION_NOT_FOUND,
                format!("неизвестное действие: {other}"),
            )),
        }
    }

    fn invoke_with_host(
        &mut self,
        request: &InvokeRequest,
        host: &mut dyn HostBridge,
    ) -> Result<InvokeResult, RpcError> {
        match request.action_id.as_str() {
            SHOW_QR_ACTION => self.handle(request, Some(host)),
            READ_QR_ACTION => Err(Self::read_unavailable(request)),
            other => Err(RpcError::new(
                error_codes::ACTION_NOT_FOUND,
                format!("неизвестное действие: {other}"),
            )),
        }
    }

    fn cancel(&mut self, _call_id: &str) {
        if let Some(window) = self.window.as_mut() {
            window.close();
        }
    }

    fn shutdown(&mut self) {
        if let Some(window) = self.window.as_mut() {
            window.close();
        }
    }
}

fn main() {
    eprintln!(
        "[qr-code-module] старт: id {MODULE_ID}, версия {MODULE_VERSION}, API {HOST_API_VERSION}; окно по умолчанию {DEFAULT_WINDOW_WIDTH}×{DEFAULT_WINDOW_HEIGHT}"
    );
    let mut module = QrCodeModule::new();
    match run_stdio(&mut module) {
        Ok(()) => eprintln!("[qr-code-module] штатное завершение"),
        Err(error) => {
            eprintln!("[qr-code-module] ошибка stdio: {error}");
            std::process::exit(1);
        }
    }
}
