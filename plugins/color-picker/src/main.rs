//! Исполняемый процесс плагина «Пипетка цвета».
//!
//! Процесс закрывает границу IPC: объявляет действие `pick-color`, читает
//! настройку «Формат цвета» и получает один снимок рабочего стола. Пиксели
//! читаются из локальной копии хоста ([`FrameRef::read_pixels`]), поэтому JSON
//! не содержит полноразмерного кадра; после копирования в память ссылка сразу
//! освобождается. Интерактивную лупу показывает окно выбора хоста, а процесс
//! обслуживает вызов с готовой точкой: проверяет её по геометрии мониторов,
//! форматирует значение и пишет строку в буфер обмена. Вызов без точки
//! возвращает исход `awaiting-selection`, а не поддельный цвет.

use serde_json::{json, Value};
use util_maker_color_picker::picker::FrameData;
use util_maker_color_picker::{ColorFormat, Point, SampleError};
use util_maker_sdk::{
    error_codes, host_methods, run_stdio, FrameRef, HostBridge, HostCallError, InvokeRequest,
    InvokeResult, Module, ModuleInfo, RpcError, HOST_API_VERSION,
};

/// Стабильный ID модуля из манифеста.
const MODULE_ID: &str = "org.utilmaker.color-picker";
/// Стабильный ID действия из манифеста.
const PICK_COLOR_ACTION: &str = "pick-color";
/// Источник кадра недоступен без моста возможностей.
const CAPTURE_UNAVAILABLE: i64 = 2001;
/// Хост не отдал пиксели кадра или его геометрию.
const FRAME_BYTES_UNAVAILABLE: i64 = 2002;

const MODULE_VERSION: &str = match option_env!("UTIL_MAKER_MODULE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

struct ColorPickerModule {
    info: ModuleInfo,
}

impl ColorPickerModule {
    fn new() -> Self {
        Self {
            info: ModuleInfo {
                id: MODULE_ID,
                version: MODULE_VERSION,
                host_api: HOST_API_VERSION,
                capabilities: &["screen.capture", "clipboard.write", "settings.read"],
            },
        }
    }

    fn read_format(host: &mut dyn HostBridge) -> Result<ColorFormat, HostCallError> {
        match host.request("settings.read", host_methods::SETTINGS_READ, json!({})) {
            Ok(value) => Ok(value
                .get("format")
                .and_then(Value::as_str)
                .and_then(ColorFormat::from_setting)
                .unwrap_or_default()),
            // Если хост не дал настройку, безопасный вариант — HEX из ТЗ.
            Err(HostCallError::Denied(_)) => Ok(ColorFormat::Hex),
            Err(error) => Err(error),
        }
    }

    fn release(host: &mut dyn HostBridge, buffer_id: &str) -> Result<(), HostCallError> {
        host.request(
            "screen.capture",
            host_methods::FRAME_RELEASE,
            json!({ "bufferId": buffer_id }),
        )
        .map(|_| ())
    }

    /// Захватывает кадр, копирует пиксели в память и сразу освобождает ссылку
    /// хоста: выбор продолжается по собственной копии без ограничения срока.
    fn capture_frame(
        host: &mut dyn HostBridge,
        request: &InvokeRequest,
    ) -> Result<FrameData, RpcError> {
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

        // Сначала читаем локальную копию, затем освобождаем ссылку: после
        // `frame.release` путь к пикселям уже недействителен.
        let pixels = frame.read_pixels();
        Self::release(host, &frame.buffer_id).map_err(HostCallError::into_rpc_error)?;
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

        FrameData::from_frame_ref(&frame, pixels).map_err(|error| {
            let reason = if matches!(
                error,
                util_maker_color_picker::FrameError::GeometryUnavailable
            ) {
                "geometry-unavailable"
            } else {
                "invalid-frame"
            };
            let code = if reason == "geometry-unavailable" {
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

    /// Точка выбора из `params.point`. Без неё действие ждёт окно выбора хоста.
    fn requested_point(params: &Value) -> Result<Option<Point>, RpcError> {
        let Some(value) = params.get("point") else {
            return Ok(None);
        };
        let coordinate = |axis: &str| {
            value
                .get(axis)
                .and_then(Value::as_i64)
                .and_then(|number| i32::try_from(number).ok())
                .ok_or_else(|| {
                    RpcError::new(
                        error_codes::INVALID_PARAMS,
                        format!("координата point.{axis} должна быть i32"),
                    )
                })
        };
        Ok(Some(Point::new(coordinate("x")?, coordinate("y")?)))
    }

    fn sample_reason(error: &SampleError) -> &'static str {
        match error {
            SampleError::OutsideMonitors { .. } => "outside-monitors",
            SampleError::OutsideFrame { .. } => "outside-frame",
            SampleError::OutOfBounds => "out-of-bounds",
        }
    }
}

impl Module for ColorPickerModule {
    fn info(&self) -> &ModuleInfo {
        &self.info
    }

    fn invoke(&mut self, request: &InvokeRequest) -> Result<InvokeResult, RpcError> {
        if request.action_id != PICK_COLOR_ACTION {
            return Err(RpcError::new(
                error_codes::ACTION_NOT_FOUND,
                format!("неизвестное действие: {}", request.action_id),
            ));
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
        if request.action_id != PICK_COLOR_ACTION {
            return Err(RpcError::new(
                error_codes::ACTION_NOT_FOUND,
                format!("неизвестное действие: {}", request.action_id),
            ));
        }

        let format = Self::read_format(host).map_err(|error| match error {
            HostCallError::Denied(message) => RpcError::new(
                error_codes::CAPABILITY_DENIED,
                format!("формат цвета недоступен: {message}"),
            ),
            other => other.into_rpc_error(),
        })?;
        let frame = Self::capture_frame(host, request)?;
        let frame_info = json!({
            "width": frame.width(),
            "height": frame.height(),
            "stride": frame.stride(),
            "origin": {
                "x": frame.origin().x,
                "y": frame.origin().y,
            },
            "monitorCount": frame.monitors().len(),
        });

        let Some(point) = Self::requested_point(&request.params)? else {
            // Выбор показывает окно хоста: без точки возвращаем машинный исход
            // и не выдумываем цвет.
            return Ok(
                InvokeResult::ok("Выберите пиксель в окне выбора.").with_data(json!({
                    "callId": request.call_id,
                    "actionId": request.action_id,
                    "outcome": "awaiting-selection",
                    "format": format.setting_value(),
                    "point": null,
                    "frame": frame_info,
                    "frameReleased": true,
                    "moduleVersion": MODULE_VERSION,
                })),
            );
        };

        let color = frame.sample(point).map_err(|error| {
            RpcError::new(error_codes::INVALID_PARAMS, error.to_string()).with_data(json!({
                "callId": request.call_id,
                "actionId": request.action_id,
                "outcome": "blocked",
                "reason": Self::sample_reason(&error),
                "point": { "x": point.x, "y": point.y },
                "frame": frame_info,
                "frameReleased": true,
                "moduleVersion": MODULE_VERSION,
            }))
        })?;
        let text = color.format(format);

        host.request(
            "clipboard.write",
            host_methods::BUFFER_COPY,
            json!({ "text": text }),
        )
        .map_err(|error| match error {
            HostCallError::Denied(message) => RpcError::new(
                error_codes::CAPABILITY_DENIED,
                format!("буфер обмена недоступен: {message}"),
            )
            .with_data(json!({
                "callId": request.call_id,
                "actionId": request.action_id,
                "outcome": "blocked",
                "reason": "clipboard-denied",
                "value": text,
                "frameReleased": true,
            })),
            other => other.into_rpc_error(),
        })?;

        Ok(
            InvokeResult::ok(format!("Скопировано: {text}")).with_data(json!({
                "callId": request.call_id,
                "actionId": request.action_id,
                "outcome": "copied",
                "format": format.setting_value(),
                "point": { "x": point.x, "y": point.y },
                "value": text,
                "frame": frame_info,
                "frameReleased": true,
                "moduleVersion": MODULE_VERSION,
            })),
        )
    }
}

fn main() {
    eprintln!(
        "[color-picker-module] старт: id {MODULE_ID}, версия {MODULE_VERSION}, API {HOST_API_VERSION}"
    );
    let mut module = ColorPickerModule::new();
    match run_stdio(&mut module) {
        Ok(()) => eprintln!("[color-picker-module] штатное завершение"),
        Err(error) => {
            eprintln!("[color-picker-module] ошибка stdio: {error}");
            std::process::exit(1);
        }
    }
}
