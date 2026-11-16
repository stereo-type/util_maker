//! Исполняемый процесс плагина «QR-коды».
//!
//! Действие `show-qr` получает выделение через `selection.get` до показа окна
//! либо открывает ручное поле. Матрица и целый масштаб возвращаются хосту;
//! системный буфер обмена показ QR не изменяет.

use serde_json::{json, Value};
use util_maker_qr_code::{
    decode_qr, DecodeOutcome, ManualReason, QrFrame, Selection as QrSelection, SelectionRead,
    ShowError, ShowWindow, WindowSize, DEFAULT_WINDOW_HEIGHT, DEFAULT_WINDOW_WIDTH,
};
use util_maker_sdk::{
    error_codes, host_methods, run_stdio, FrameRef, HostBridge, HostCallError, InvokeRequest,
    InvokeResult, Module, ModuleInfo, RpcError, HOST_API_VERSION,
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
/// Источник кадра недоступен без моста возможностей.
const CAPTURE_UNAVAILABLE: i64 = 2001;
/// Не удалось получить кадр или декодировать выбранную область.
const READ_FAILED: i64 = 2002;

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

    fn selection_from_params(params: &Value) -> Result<QrSelection, RpcError> {
        let selection = params
            .get("selection")
            .ok_or_else(|| RpcError::new(error_codes::INVALID_PARAMS, "не передана область"))?;
        let number = |name: &str| -> Result<i64, RpcError> {
            selection
                .get(name)
                .and_then(Value::as_i64)
                .ok_or_else(|| RpcError::new(error_codes::INVALID_PARAMS, format!("нет {name}")))
        };
        let width = u32::try_from(number("width")?)
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| RpcError::new(error_codes::INVALID_PARAMS, "нулевая ширина"))?;
        let height = u32::try_from(number("height")?)
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| RpcError::new(error_codes::INVALID_PARAMS, "нулевая высота"))?;
        Ok(QrSelection::new(
            i32::try_from(number("x")?).unwrap_or(i32::MIN),
            i32::try_from(number("y")?).unwrap_or(i32::MIN),
            width,
            height,
        ))
    }

    fn read_failed(reason: &str, message: &str, needs_new_selection: bool) -> RpcError {
        RpcError::new(READ_FAILED, message).with_data(json!({
            "outcome": "read-failed",
            "reason": reason,
            "needsNewSelection": needs_new_selection,
            "moduleVersion": MODULE_VERSION,
        }))
    }

    fn outcome_error(outcome: &DecodeOutcome) -> RpcError {
        match outcome {
            DecodeOutcome::NoCode => Self::read_failed(
                "no-code",
                "QR-код не найден. Выделите область заново.",
                true,
            ),
            DecodeOutcome::Damaged => Self::read_failed(
                "damaged",
                "QR-код повреждён и не читается. Выделите область заново.",
                true,
            ),
            DecodeOutcome::Multiple { count } => Self::read_failed(
                "multiple",
                &format!("В области несколько QR-кодов ({count}). Выделите один код."),
                true,
            ),
            DecodeOutcome::UnsupportedEncoding => Self::read_failed(
                "unsupported-encoding",
                "Кодировка QR не поддерживается.",
                false,
            ),
            DecodeOutcome::BinaryPayload => Self::read_failed(
                "binary-payload",
                "QR содержит двоичные данные, их нельзя показать как текст.",
                false,
            ),
            DecodeOutcome::Empty => Self::read_failed("empty", "QR не содержит текста.", false),
            DecodeOutcome::Text(_) => Self::read_failed("decoded", "QR прочитан.", false),
        }
    }

    fn handle_read(
        &mut self,
        request: &InvokeRequest,
        host: Option<&mut dyn HostBridge>,
    ) -> Result<InvokeResult, RpcError> {
        // Окно показа QR больше не актуально: результат чтения заменяет его.
        if let Some(window) = self.window.as_mut() {
            window.close();
        }
        let Some(host) = host else {
            return Err(RpcError::new(
                CAPTURE_UNAVAILABLE,
                "мост возможностей хоста недоступен",
            ));
        };
        let selection = Self::selection_from_params(&request.params)?;
        let region = json!({
            "x": selection.x,
            "y": selection.y,
            "width": selection.width,
            "height": selection.height,
        });
        let captured = host
            .request(
                SCREEN_CAPABILITY,
                host_methods::FRAME_CAPTURE,
                json!({ "region": region }),
            )
            .map_err(|error| match error {
                HostCallError::Denied(message) => RpcError::new(
                    CAPTURE_UNAVAILABLE,
                    format!("источник кадра недоступен: {message}"),
                ),
                other => other.into_rpc_error(),
            })?;
        let frame: FrameRef = serde_json::from_value(captured).map_err(|error| {
            RpcError::new(
                error_codes::INVALID_PARAMS,
                format!("ответ frame.capture: {error}"),
            )
        })?;
        let pixels = frame.read_pixels().map_err(|error| {
            RpcError::new(
                READ_FAILED,
                format!("не удалось прочитать пиксели кадра: {error}"),
            )
        });
        let released = host.request(
            SCREEN_CAPABILITY,
            host_methods::FRAME_RELEASE,
            json!({ "bufferId": frame.buffer_id }),
        );
        let pixels = pixels?;
        released.map_err(HostCallError::into_rpc_error)?;
        let qr_frame = QrFrame::from_frame_ref(&frame, &pixels)
            .map_err(|error| RpcError::new(error_codes::INVALID_PARAMS, error.to_string()))?;
        let outcome = decode_qr(&qr_frame, selection)
            .map_err(|error| RpcError::new(error_codes::INVALID_PARAMS, error.to_string()))?;
        match outcome {
            DecodeOutcome::Text(ref text) => {
                let value = text.text().to_owned();
                let mut data = json!({
                    "callId": request.call_id,
                    "actionId": request.action_id,
                    "outcome": "decoded",
                    "text": value,
                    "version": text.version(),
                    "selection": {
                        "x": selection.x,
                        "y": selection.y,
                        "width": selection.width,
                        "height": selection.height,
                    },
                    "moduleVersion": MODULE_VERSION,
                    "clipboardChanged": false,
                });
                if let Value::Object(ref mut object) = data {
                    object.insert("source".to_owned(), json!(request.context.source.as_str()));
                }
                Ok(InvokeResult::ok(text.text().to_owned()).with_data(data))
            }
            other => Err(Self::outcome_error(&other).with_data(json!({
                "callId": request.call_id,
                "actionId": request.action_id,
                "outcome": "read-failed",
                "moduleVersion": MODULE_VERSION,
            }))),
        }
    }
}

impl Module for QrCodeModule {
    fn info(&self) -> &ModuleInfo {
        &self.info
    }

    fn invoke(&mut self, request: &InvokeRequest) -> Result<InvokeResult, RpcError> {
        match request.action_id.as_str() {
            SHOW_QR_ACTION => self.handle(request, None),
            READ_QR_ACTION => self.handle_read(request, None),
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
            READ_QR_ACTION => self.handle_read(request, Some(host)),
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
