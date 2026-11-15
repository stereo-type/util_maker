//! Исполняемый процесс плагина «Пипетка цвета».
//!
//! Процесс закрывает границу IPC: объявляет действие `pick-color`, читает
//! настройку «Формат цвета» и запрашивает один снимок. Передача полноразмерных
//! пикселей дочернему процессу ещё не реализована в SDK, поэтому после
//! `frame.release` действие возвращает явную машинную причину, а не поддельный
//! цвет. Переносимое состояние выбора и лупы проверяется в
//! `util_maker_color_picker::picker`.

use serde_json::{json, Value};
use util_maker_color_picker::ColorFormat;
use util_maker_sdk::{
    error_codes, host_methods, run_stdio, FrameRef, HostBridge, HostCallError, InvokeRequest,
    InvokeResult, Module, ModuleInfo, RpcError, FRAME_FORMAT_BGRA8, HOST_API_VERSION,
};

/// Стабильный ID модуля из манифеста.
const MODULE_ID: &str = "org.utilmaker.color-picker";
/// Стабильный ID действия из манифеста.
const PICK_COLOR_ACTION: &str = "pick-color";
/// Источник кадра недоступен без моста возможностей.
const CAPTURE_UNAVAILABLE: i64 = 2001;
/// Хост вернул ссылку на кадр, но способа прочитать его байты ещё нет.
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

        let captured = host
            .request("screen.capture", host_methods::FRAME_CAPTURE, json!({}))
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

        // Ссылку освобождаем даже если ниже выяснится, что кадр неполный.
        let release_error = Self::release(host, &frame.buffer_id).err();
        if let Some(error) = release_error {
            return Err(error.into_rpc_error());
        }

        if frame.format != FRAME_FORMAT_BGRA8 {
            return Err(RpcError::new(
                error_codes::INVALID_PARAMS,
                format!("формат кадра {} не поддерживается", frame.format),
            )
            .with_data(json!({
                "callId": request.call_id,
                "actionId": request.action_id,
                "reason": "unsupported-frame-format",
                "format": frame.format,
            })));
        }
        if frame.width == 0 || frame.height == 0 || frame.stride == 0 {
            return Err(
                RpcError::new(error_codes::INVALID_PARAMS, "кадр имеет нулевой размер").with_data(
                    json!({
                        "callId": request.call_id,
                        "actionId": request.action_id,
                        "reason": "invalid-frame",
                    }),
                ),
            );
        }

        let monitor_count = frame
            .geometry
            .as_ref()
            .map_or(0, |geometry| geometry.monitors.len());
        let reason = if frame.geometry.is_some() {
            "frame-bytes-unavailable"
        } else {
            "geometry-unavailable"
        };
        let message = if frame.geometry.is_some() {
            "хост вернул кадр, но передача пикселей дочернему процессу ещё не реализована; ссылка на кадр освобождена"
        } else {
            "в ответе кадра нет геометрии мониторов; ссылка на кадр освобождена"
        };
        Err(
            RpcError::new(FRAME_BYTES_UNAVAILABLE, message).with_data(json!({
                "callId": request.call_id,
                "actionId": request.action_id,
                "outcome": "blocked",
                "reason": reason,
                "format": format.setting_value(),
                "bufferId": frame.buffer_id,
                "width": frame.width,
                "height": frame.height,
                "stride": frame.stride,
                "monitorCount": monitor_count,
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
