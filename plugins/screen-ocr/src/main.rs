//! Исполняемый процесс плагина «Текст с экрана».
//!
//! Процесс закрывает границу IPC: читает языки через `settings.read`,
//! захватывает один кадр, копирует пиксели в память и сразу освобождает
//! ссылку `frame.release`. Дальше выбор области работает по сохранённой копии.
//! Оконный overlay и окно результата подключаются следующими задачами.

use serde_json::{json, Value};
use util_maker_screen_ocr::{
    OcrLanguage, Point, RegionError, RegionFrame, RegionPicker, Selection, SelectionPhase,
    SelectionUpdate, StartOutcome,
};
use util_maker_sdk::{
    error_codes, host_methods, run_stdio, FrameRef, HostBridge, HostCallError, InvokeRequest,
    InvokeResult, Module, ModuleInfo, RpcError, HOST_API_VERSION,
};

/// Стабильный ID модуля из манифеста.
const MODULE_ID: &str = "org.utilmaker.screen-ocr";
/// Стабильный ID действия из манифеста.
const RECOGNIZE_REGION_ACTION: &str = "recognize-region";
/// Источник кадра недоступен без моста возможностей.
const CAPTURE_UNAVAILABLE: i64 = 2001;
/// Хост вернул ссылку, но пиксели или геометрия недоступны.
const FRAME_BYTES_UNAVAILABLE: i64 = 2002;

const MODULE_VERSION: &str = match option_env!("UTIL_MAKER_MODULE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

struct ScreenOcrModule {
    info: ModuleInfo,
    region: RegionPicker,
    language: OcrLanguage,
}

impl ScreenOcrModule {
    fn new() -> Self {
        Self {
            info: ModuleInfo {
                id: MODULE_ID,
                version: MODULE_VERSION,
                host_api: HOST_API_VERSION,
                capabilities: &["screen.capture", "settings.read", "clipboard.write"],
            },
            region: RegionPicker::new(),
            language: OcrLanguage::default(),
        }
    }

    fn read_language(host: &mut dyn HostBridge) -> Result<OcrLanguage, HostCallError> {
        let value = match host.request("settings.read", host_methods::SETTINGS_READ, json!({})) {
            Ok(value) => value,
            // Если хост не дал настройку, безопасный вариант — режим из ТЗ.
            Err(HostCallError::Denied(_)) => return Ok(OcrLanguage::default()),
            Err(error) => return Err(error),
        };
        let setting = value
            .get("languages")
            .or_else(|| value.get("language"))
            .and_then(Value::as_str);
        Ok(setting
            .and_then(OcrLanguage::from_setting)
            .unwrap_or_default())
    }

    fn capture_frame(
        host: &mut dyn HostBridge,
        request: &InvokeRequest,
    ) -> Result<RegionFrame, RpcError> {
        let captured = host
            .request("screen.capture", host_methods::FRAME_CAPTURE, json!({}))
            .map_err(|error| match error {
                HostCallError::Denied(message) => RpcError::new(
                    CAPTURE_UNAVAILABLE,
                    format!("источник кадра недоступен: {message}"),
                )
                .with_data(json!({
                    "callId": request.call_id,
                    "actionId": request.action_id,
                    "outcome": "blocked",
                    "reason": "capture-unavailable",
                    "moduleVersion": MODULE_VERSION,
                })),
                other => other.into_rpc_error(),
            })?;
        let frame: FrameRef = serde_json::from_value(captured).map_err(|error| {
            RpcError::new(
                error_codes::INVALID_PARAMS,
                format!("ответ frame.capture: {error}"),
            )
        })?;

        // Читаем локальную копию до release: после него путь уже недействителен.
        let pixels = frame.read_pixels();
        let released = host.request(
            "screen.capture",
            host_methods::FRAME_RELEASE,
            json!({ "bufferId": frame.buffer_id }),
        );
        released.map_err(HostCallError::into_rpc_error)?;
        let pixels = pixels.map_err(|error| {
            RpcError::new(
                FRAME_BYTES_UNAVAILABLE,
                format!("не удалось прочитать пиксели кадра: {error}"),
            )
            .with_data(json!({
                "callId": request.call_id,
                "actionId": request.action_id,
                "outcome": "blocked",
                "reason": "frame-bytes-unavailable",
                "bufferId": frame.buffer_id,
                "frameReleased": true,
                "moduleVersion": MODULE_VERSION,
            }))
        })?;

        RegionFrame::from_frame_ref(&frame, pixels).map_err(|error| {
            let reason = match &error {
                RegionError::GeometryUnavailable => "geometry-unavailable",
                RegionError::Frame(_) => "invalid-frame",
                _ => "invalid-frame",
            };
            let code = if matches!(error, RegionError::GeometryUnavailable) {
                FRAME_BYTES_UNAVAILABLE
            } else {
                error_codes::INVALID_PARAMS
            };
            RpcError::new(code, error.to_string()).with_data(json!({
                "callId": request.call_id,
                "actionId": request.action_id,
                "outcome": "blocked",
                "reason": reason,
                "bufferId": frame.buffer_id,
                "frameReleased": true,
                "moduleVersion": MODULE_VERSION,
            }))
        })
    }

    fn cancel_requested(params: &Value) -> bool {
        params
            .get("cancel")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    fn apply_params(
        region: &mut RegionPicker,
        params: &Value,
    ) -> Result<Option<SelectionUpdate>, RpcError> {
        let Some(session) = region.session_mut() else {
            return Ok(None);
        };
        if let Some(value) = params.get("selection") {
            let selection = parse_selection(value)?;
            return session
                .select_rect(selection)
                .map(Some)
                .map_err(region_error);
        }
        let anchor = params.get("anchor").map(parse_point).transpose()?;
        let cursor = params.get("cursor").map(parse_point).transpose()?;
        match (anchor, cursor) {
            (Some(anchor), Some(cursor)) => session
                .set_selection(anchor, cursor)
                .map(Some)
                .map_err(region_error),
            (Some(anchor), None) => session.begin(anchor).map(Some).map_err(region_error),
            (None, Some(cursor)) => session.move_to(cursor).map(Some).map_err(region_error),
            (None, None) => Ok(None),
        }
    }

    fn state_result(
        &self,
        request: &InvokeRequest,
        outcome: &str,
        update: Option<SelectionUpdate>,
    ) -> InvokeResult {
        let session = self.region.session();
        let phase = update
            .map(|update| update.phase)
            .or_else(|| session.map(|session| session.phase()))
            .unwrap_or(SelectionPhase::Idle);
        let selection = update
            .and_then(|update| update.selection)
            .or_else(|| session.and_then(|session| session.selection()));
        let frame = session.map(|session| session.frame());
        let message = match outcome {
            "already-active" => "Выбор области уже открыт.",
            "selection-update" if selection.is_some() => "Область выбрана.",
            "selection-update" => "Выделите область мышью.",
            _ => "Выделите область мышью.",
        };
        InvokeResult::ok(message).with_data(json!({
            "callId": request.call_id,
            "actionId": request.action_id,
            "outcome": outcome,
            "phase": phase_name(phase),
            "language": self.language.codes(),
            "languageLabel": self.language.display_name(),
            "selection": selection.map(selection_json),
            "frame": frame.map(frame_json),
            "frameReleased": true,
            "moduleVersion": MODULE_VERSION,
        }))
    }

    fn invoke_region(
        &mut self,
        request: &InvokeRequest,
        host: &mut dyn HostBridge,
    ) -> Result<InvokeResult, RpcError> {
        if Self::cancel_requested(&request.params) {
            let cancelled = self.region.cancel_any();
            return Ok(InvokeResult::cancelled().with_data(json!({
                "callId": request.call_id,
                "actionId": request.action_id,
                "outcome": "cancelled",
                "cancelled": cancelled,
                "moduleVersion": MODULE_VERSION,
            })));
        }

        if self.region.repeat_shortcut() {
            let update = Self::apply_params(&mut self.region, &request.params)?;
            let outcome = if update.is_some() {
                "selection-update"
            } else {
                "already-active"
            };
            return Ok(self.state_result(request, outcome, update));
        }

        let language = Self::read_language(host).map_err(|error| match error {
            HostCallError::Denied(message) => RpcError::new(
                error_codes::CAPABILITY_DENIED,
                format!("языки распознавания недоступны: {message}"),
            ),
            other => other.into_rpc_error(),
        })?;
        let frame = Self::capture_frame(host, request)?;
        self.language = language;
        if self.region.start(frame, &request.call_id) == StartOutcome::AlreadyActive {
            return Ok(self.state_result(request, "already-active", None));
        }
        let update = Self::apply_params(&mut self.region, &request.params)?;
        let outcome = if update.is_some() {
            "selection-update"
        } else {
            "awaiting-selection"
        };
        Ok(self.state_result(request, outcome, update))
    }
}

impl Module for ScreenOcrModule {
    fn info(&self) -> &ModuleInfo {
        &self.info
    }

    fn invoke(&mut self, request: &InvokeRequest) -> Result<InvokeResult, RpcError> {
        if request.action_id != RECOGNIZE_REGION_ACTION {
            return Err(RpcError::new(
                error_codes::ACTION_NOT_FOUND,
                format!("неизвестное действие: {}", request.action_id),
            ));
        }
        if self.region.repeat_shortcut() {
            return Ok(self.state_result(request, "already-active", None));
        }
        Err(RpcError::new(
            CAPTURE_UNAVAILABLE,
            "источник кадра недоступен: нужен мост возможностей хоста",
        )
        .with_data(json!({
            "callId": request.call_id,
            "actionId": request.action_id,
            "outcome": "blocked",
            "reason": "capture-unavailable",
            "moduleVersion": MODULE_VERSION,
        })))
    }

    fn invoke_with_host(
        &mut self,
        request: &InvokeRequest,
        host: &mut dyn HostBridge,
    ) -> Result<InvokeResult, RpcError> {
        if request.action_id != RECOGNIZE_REGION_ACTION {
            return Err(RpcError::new(
                error_codes::ACTION_NOT_FOUND,
                format!("неизвестное действие: {}", request.action_id),
            ));
        }
        self.invoke_region(request, host)
    }

    fn cancel(&mut self, call_id: &str) {
        self.region.cancel(call_id);
    }

    fn shutdown(&mut self) {
        self.region.cancel_any();
    }
}

fn parse_point(value: &Value) -> Result<Point, RpcError> {
    let x = value
        .get("x")
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| invalid_params("координата x должна быть i32"))?;
    let y = value
        .get("y")
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| invalid_params("координата y должна быть i32"))?;
    Ok(Point::new(x, y))
}

fn parse_selection(value: &Value) -> Result<Selection, RpcError> {
    let x = value
        .get("x")
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| invalid_params("selection.x должен быть i32"))?;
    let y = value
        .get("y")
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| invalid_params("selection.y должен быть i32"))?;
    let width = value
        .get("width")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| invalid_params("selection.width должен быть u32"))?;
    let height = value
        .get("height")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| invalid_params("selection.height должен быть u32"))?;
    Ok(Selection::new(x, y, width, height))
}

fn invalid_params(message: impl Into<String>) -> RpcError {
    RpcError::new(error_codes::INVALID_PARAMS, message)
}

fn region_error(error: RegionError) -> RpcError {
    invalid_params(error.to_string()).with_data(json!({
        "outcome": "invalid-selection",
        "reason": "invalid-selection",
    }))
}

fn phase_name(phase: SelectionPhase) -> &'static str {
    match phase {
        SelectionPhase::Idle => "idle",
        SelectionPhase::Selecting => "selecting",
        SelectionPhase::Selected => "selected",
        SelectionPhase::Cancelled => "cancelled",
    }
}

fn selection_json(selection: Selection) -> Value {
    json!({
        "x": selection.x,
        "y": selection.y,
        "width": selection.width,
        "height": selection.height,
    })
}

fn frame_json(frame: &RegionFrame) -> Value {
    json!({
        "width": frame.width(),
        "height": frame.height(),
        "stride": frame.stride(),
        "origin": {
            "x": frame.origin().x,
            "y": frame.origin().y,
        },
        "monitors": frame.monitors(),
        "expiresInMs": frame.expires_in_ms(),
    })
}

fn main() {
    eprintln!(
        "[screen-ocr-module] старт: id {MODULE_ID}, версия {MODULE_VERSION}, API {HOST_API_VERSION}"
    );
    let mut module = ScreenOcrModule::new();
    match run_stdio(&mut module) {
        Ok(()) => eprintln!("[screen-ocr-module] штатное завершение"),
        Err(error) => {
            eprintln!("[screen-ocr-module] ошибка stdio: {error}");
            std::process::exit(1);
        }
    }
}
