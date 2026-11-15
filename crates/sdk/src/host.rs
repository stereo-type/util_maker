//! Обращения модуля к возможностям хоста во время активного вызова.
//!
//! Сообщения идут по тому же JSON-RPC 2.0 каналу, что и вызовы хоста, но
//! имеют собственный метод [`HOST_CALL_METHOD`]. Хост сопоставляет запрос с
//! живым `callId`, проверяет объявленную возможность и отвечает ровно один раз.
//! Кадр передаётся ссылкой на временный буфер: пиксели не сериализуются в JSON.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::protocol::{error_codes, RpcError};

/// Метод JSON-RPC, которым модуль обращается к хосту.
pub const HOST_CALL_METHOD: &str = "host.call";

/// Имена операций хоста, доступных через [`HostBridge::request`].
pub mod host_methods {
    pub const FRAME_CAPTURE: &str = "frame.capture";
    pub const FRAME_RELEASE: &str = "frame.release";
    pub const SELECTION_GET: &str = "selection.get";
    pub const SELECTION_WAIT: &str = "selection.wait";
    pub const BUFFER_COPY: &str = "buffer.copy";
    pub const SETTINGS_READ: &str = "settings.read";
    pub const SECRET_READ: &str = "secret.read";
}

/// Формат пикселей кадра. Первая версия принимает BGRA8.
pub const FRAME_FORMAT_BGRA8: &str = "bgra8";

/// Запрос модуля к хосту. `callId` обязан совпадать с живым вызовом.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HostCall {
    pub call_id: String,
    pub capability: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// Проверенный контекст модуля и вызова на стороне хоста.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCallContext {
    pub module_id: String,
    pub call_id: String,
    pub capabilities: Vec<String>,
}

impl HostCallContext {
    pub fn new(
        module_id: impl Into<String>,
        call_id: impl Into<String>,
        capabilities: Vec<String>,
    ) -> Self {
        Self {
            module_id: module_id.into(),
            call_id: call_id.into(),
            capabilities,
        }
    }

    /// Объявлена ли возможность в манифесте этого модуля.
    pub fn allows(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|item| item == capability)
    }
}

/// Реализация возможностей хоста. Адаптер платформы остаётся за этим
/// интерфейсом; в JSON не попадают Win32-дескрипторы или адреса памяти.
pub trait HostHandler {
    fn handle(&mut self, context: &HostCallContext, call: &HostCall) -> Result<Value, RpcError>;
}

/// Хост без подключённого адаптера: любые обращения отклоняются.
#[derive(Debug, Default)]
pub struct DeniedHost;

impl HostHandler for DeniedHost {
    fn handle(&mut self, _context: &HostCallContext, call: &HostCall) -> Result<Value, RpcError> {
        Err(RpcError::new(
            error_codes::CAPABILITY_DENIED,
            format!("возможность {} недоступна", call.capability),
        ))
    }
}

/// Ошибка обращения модуля к хосту. Отличает отказ, потерю контекста,
/// отмену и запрос завершения процесса.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostCallError {
    #[error("возможность отклонена: {0}")]
    Denied(String),
    #[error("контекст вызова потерян: {0}")]
    ContextLost(String),
    #[error("действие отменено")]
    Cancelled,
    #[error("хост запросил завершение модуля")]
    Shutdown,
    #[error("ошибка протокола обращения к хосту: {0}")]
    Protocol(String),
}

impl HostCallError {
    pub fn rpc_code(&self) -> i64 {
        match self {
            Self::Denied(_) => error_codes::CAPABILITY_DENIED,
            Self::ContextLost(_) => error_codes::CONTEXT_LOST,
            Self::Cancelled => error_codes::CANCELLED,
            Self::Shutdown => error_codes::CANCELLED,
            Self::Protocol(_) => error_codes::INTERNAL_ERROR,
        }
    }

    pub fn into_rpc_error(self) -> RpcError {
        RpcError::new(self.rpc_code(), self.to_string())
    }
}

/// Мост, который SDK передаёт обработчику действия. Он отправляет запрос
/// хосту и ждёт ответ, одновременно принимая `cancel` и `shutdown`.
pub trait HostBridge {
    fn request(
        &mut self,
        capability: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, HostCallError>;

    /// Был ли вызов отменён, пока обработчик ждал хост.
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// Прямоугольник в координатах экрана.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameRegion {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Параметры запроса кадра. Полноразмерные пиксели в сообщение не входят.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<FrameRegion>,
}

/// Ссылка на временный буфер кадра. Действует до истечения `expiresInMs` или
/// до завершения вызова, смотря что наступит раньше.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameRef {
    pub buffer_id: String,
    pub format: String,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub origin: FrameOrigin,
    pub scale_factor: f64,
    pub expires_in_ms: u32,
    /// Геометрия виртуального рабочего стола в физических пикселях. Старые
    /// ответы без неё остаются совместимыми.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<ScreenGeometry>,
    /// Локальный файл с BGRA8-пикселями. В JSON передаётся только путь:
    /// процесс читает байты сам, а хост удаляет файл при release/истечении
    /// ссылки или завершении вызова. Старые клиенты поле игнорируют.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pixel_path: Option<String>,
}

impl FrameRef {
    /// Читает локальную копию BGRA8-пикселей, созданную хостом для этого кадра.
    ///
    /// Файл принадлежит вызову и удаляется хостом после `frame.release`,
    /// истечения `expiresInMs` или завершения вызова. Процесс должен скопировать
    /// байты в память, если ему нужен кадр дольше срока ссылки.
    pub fn read_pixels(&self) -> std::io::Result<Vec<u8>> {
        let path = self.pixel_path.as_deref().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "хост не предоставил файл пикселей кадра",
            )
        })?;
        std::fs::read(path)
    }
}

/// Положение и масштаб одного монитора в физических пикселях виртуального
/// рабочего стола. Порядок элементов соответствует системному перечислению;
/// для запроса используйте `primary` или индекс.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorGeometry {
    pub rect: FrameRegion,
    pub scale_factor: f64,
    pub primary: bool,
}

/// Единая геометрия виртуального рабочего стола и всех его мониторов.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenGeometry {
    pub virtual_screen: FrameRegion,
    pub monitors: Vec<MonitorGeometry>,
}

/// Начало кадра в координатах рабочего стола.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameOrigin {
    pub x: i32,
    pub y: i32,
}

/// Ссылка на вспомогательный буфер, например изображение для буфера обмена.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BufferRef {
    pub buffer_id: String,
    pub byte_len: u64,
    pub expires_in_ms: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn frame_reference_contains_no_pixels() {
        let frame = FrameRef {
            buffer_id: "frame-17".to_owned(),
            format: FRAME_FORMAT_BGRA8.to_owned(),
            width: 1920,
            height: 1080,
            stride: 1920 * 4,
            origin: FrameOrigin { x: 0, y: 0 },
            scale_factor: 1.0,
            expires_in_ms: 5_000,
            pixel_path: None,
            geometry: Some(ScreenGeometry {
                virtual_screen: FrameRegion {
                    x: -1920,
                    y: 0,
                    width: 3840,
                    height: 1080,
                },
                monitors: vec![
                    MonitorGeometry {
                        rect: FrameRegion {
                            x: -1920,
                            y: 0,
                            width: 1920,
                            height: 1080,
                        },
                        scale_factor: 1.0,
                        primary: false,
                    },
                    MonitorGeometry {
                        rect: FrameRegion {
                            x: 0,
                            y: 0,
                            width: 1920,
                            height: 1080,
                        },
                        scale_factor: 1.5,
                        primary: true,
                    },
                ],
            }),
        };
        let text = serde_json::to_string(&frame).unwrap();
        assert!(text.len() < 512);
        assert!(text.contains("frame-17"));
        assert!(!text.contains("pixels"));
        assert!(!text.contains("data"));
    }

    #[test]
    fn context_checks_declared_capability() {
        let context = HostCallContext::new(
            "org.example.echo",
            "call-1",
            vec!["screen.frame".to_owned()],
        );
        assert!(context.allows("screen.frame"));
        assert!(!context.allows("clipboard.write"));
    }

    #[test]
    fn denied_host_uses_machine_readable_code() {
        let context = HostCallContext::new("org.example.echo", "call-1", Vec::new());
        let call = HostCall {
            call_id: "call-1".to_owned(),
            capability: "screen.frame".to_owned(),
            method: host_methods::FRAME_CAPTURE.to_owned(),
            params: json!({}),
        };
        let error = DeniedHost.handle(&context, &call).unwrap_err();
        assert_eq!(error.code, error_codes::CAPABILITY_DENIED);
    }
}
