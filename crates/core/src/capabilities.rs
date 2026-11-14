//! Контекст активного вызова и маршрутизация обращений к возможностям хоста.
//!
//! Ядро не знает Win32, Tauri и графической сессии. Платформенный адаптер
//! реализует [`PlatformHost`], а [`HostDispatcher`] закрепляет кадры и прочие
//! ресурсы за конкретным `moduleId + actionId + callId`. После завершения
//! вызова все его буферы освобождаются, даже если модуль завершился аварийно.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::Value;
use util_maker_sdk::{
    error_codes, host_methods, FrameRef, FrameRequest, HostCall, HostCallContext, HostHandler,
    InvocationSource, RpcError,
};

/// Непрозрачная ссылка на внешний источник, зафиксированный адаптером ОС.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceRef(pub String);

/// Контекст, зафиксированный до открытия собственных окон.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvocationContext {
    pub source: InvocationSource,
    pub source_ref: Option<SourceRef>,
    pub source_lost: bool,
}

impl InvocationContext {
    pub fn new(source: InvocationSource) -> Self {
        Self {
            source,
            source_ref: None,
            source_lost: false,
        }
    }

    /// Вызов из меню: используется последнее внешнее окно до открытия меню.
    pub fn tray(last_external: Option<SourceRef>) -> Self {
        Self {
            source: InvocationSource::Tray,
            source_lost: last_external.is_none(),
            source_ref: last_external,
        }
    }

    /// Вызов сочетанием: источник фиксируется в момент события.
    pub fn hotkey(at_event: Option<SourceRef>) -> Self {
        Self {
            source: InvocationSource::Hotkey,
            source_lost: at_event.is_none(),
            source_ref: at_event,
        }
    }

    pub fn with_lost(mut self, lost: bool) -> Self {
        self.source_lost = lost;
        self
    }
}

/// Закреплённый за вызовом контекст модуля и действия.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSession {
    pub call_id: String,
    pub module_id: String,
    pub action_id: String,
    pub context: InvocationContext,
}

impl CallSession {
    pub fn new(
        call_id: impl Into<String>,
        module_id: impl Into<String>,
        action_id: impl Into<String>,
        context: InvocationContext,
    ) -> Self {
        Self {
            call_id: call_id.into(),
            module_id: module_id.into(),
            action_id: action_id.into(),
            context,
        }
    }
}

/// Хук начала и окончания активного вызова. Регистрируя его в реестре, хост
/// получает гарантированное освобождение ресурсов при любом исходе.
pub trait HostSession {
    fn begin_call(&mut self, context: &CallSession);
    fn finish_call(&mut self, call_id: &str);
}

/// Платформенный адаптер. Все методы получают только контекст вызова, а не
/// дескрипторы Win32 или адреса памяти.
pub trait PlatformHost {
    fn capture_frame(
        &mut self,
        context: &CallSession,
        request: &FrameRequest,
    ) -> Result<FrameRef, RpcError> {
        Err(unsupported(context, host_methods::FRAME_CAPTURE, request))
    }

    fn release_frame(&mut self, context: &CallSession, buffer_id: &str) -> Result<(), RpcError> {
        Err(RpcError::new(
            error_codes::CAPABILITY_DENIED,
            format!("кадр {buffer_id} недоступен модулю {}", context.module_id),
        ))
    }

    fn selection_get(&mut self, context: &CallSession) -> Result<Value, RpcError> {
        Err(unsupported(
            context,
            host_methods::SELECTION_GET,
            &Value::Null,
        ))
    }

    fn buffer_copy(&mut self, context: &CallSession, params: &Value) -> Result<Value, RpcError> {
        Err(unsupported(context, host_methods::BUFFER_COPY, params))
    }

    fn settings_read(&mut self, context: &CallSession, params: &Value) -> Result<Value, RpcError> {
        Err(unsupported(context, host_methods::SETTINGS_READ, params))
    }

    fn secret_read(&mut self, context: &CallSession, params: &Value) -> Result<Value, RpcError> {
        Err(unsupported(context, host_methods::SECRET_READ, params))
    }
}

fn unsupported(context: &CallSession, method: &str, _params: &impl serde::Serialize) -> RpcError {
    RpcError::new(
        error_codes::CAPABILITY_DENIED,
        format!("операция {method} недоступна модулю {}", context.module_id),
    )
}

#[derive(Debug)]
struct FrameLease {
    call_id: String,
    expires_at: Instant,
}

/// Диспетчер возможностей поверх платформенного адаптера.
pub struct HostDispatcher<H: PlatformHost> {
    platform: H,
    sessions: BTreeMap<String, CallSession>,
    frames: BTreeMap<String, FrameLease>,
}

impl<H: PlatformHost> HostDispatcher<H> {
    pub fn new(platform: H) -> Self {
        Self {
            platform,
            sessions: BTreeMap::new(),
            frames: BTreeMap::new(),
        }
    }

    pub fn platform(&self) -> &H {
        &self.platform
    }

    pub fn platform_mut(&mut self) -> &mut H {
        &mut self.platform
    }

    pub fn finish_all(&mut self) {
        let call_ids: Vec<_> = self.sessions.keys().cloned().collect();
        for call_id in call_ids {
            self.finish_call(&call_id);
        }
    }

    fn session(&self, context: &HostCallContext) -> Result<&CallSession, RpcError> {
        let session = self.sessions.get(&context.call_id).ok_or_else(|| {
            RpcError::new(
                error_codes::CONTEXT_LOST,
                format!("вызов {} уже завершён", context.call_id),
            )
        })?;
        if session.module_id != context.module_id {
            return Err(RpcError::new(
                error_codes::CAPABILITY_DENIED,
                "модуль не совпадает с владельцем вызова",
            ));
        }
        Ok(session)
    }

    fn require_source(&self, session: &CallSession) -> Result<(), RpcError> {
        if session.context.source_lost {
            return Err(RpcError::new(
                error_codes::CONTEXT_LOST,
                "внешний источник вызова потерян",
            ));
        }
        Ok(())
    }
}

impl<H: PlatformHost> HostSession for HostDispatcher<H> {
    fn begin_call(&mut self, context: &CallSession) {
        if self.sessions.contains_key(&context.call_id) {
            self.finish_call(&context.call_id);
        }
        self.sessions
            .insert(context.call_id.clone(), context.clone());
    }

    fn finish_call(&mut self, call_id: &str) {
        let session = self.sessions.remove(call_id);
        let buffers: Vec<_> = self
            .frames
            .iter()
            .filter(|(_, lease)| lease.call_id == call_id)
            .map(|(buffer, _)| buffer.clone())
            .collect();
        for buffer in buffers {
            self.frames.remove(&buffer);
            if let Some(session) = session.as_ref() {
                let _ = self.platform.release_frame(session, &buffer);
            }
        }
    }
}

impl<H: PlatformHost> HostHandler for HostDispatcher<H> {
    fn handle(&mut self, context: &HostCallContext, call: &HostCall) -> Result<Value, RpcError> {
        if call.call_id != context.call_id {
            return Err(RpcError::new(
                error_codes::CONTEXT_LOST,
                "запрос относится к другому вызову",
            ));
        }
        if !context.allows(&call.capability) {
            return Err(RpcError::new(
                error_codes::CAPABILITY_DENIED,
                format!("возможность {} не объявлена", call.capability),
            ));
        }
        let session = self.session(context)?.clone();
        match call.method.as_str() {
            host_methods::FRAME_CAPTURE => {
                self.require_source(&session)?;
                let request: FrameRequest =
                    serde_json::from_value(call.params.clone()).map_err(|error| {
                        RpcError::new(
                            error_codes::INVALID_PARAMS,
                            format!("параметры кадра: {error}"),
                        )
                    })?;
                let frame = self.platform.capture_frame(&session, &request)?;
                if self.frames.contains_key(&frame.buffer_id) {
                    return Err(RpcError::new(
                        error_codes::INVALID_PARAMS,
                        "идентификатор кадра уже используется",
                    ));
                }
                if frame.width == 0 || frame.height == 0 || frame.stride == 0 {
                    return Err(RpcError::new(
                        error_codes::INVALID_PARAMS,
                        "кадр имеет нулевой размер",
                    ));
                }
                self.frames.insert(
                    frame.buffer_id.clone(),
                    FrameLease {
                        call_id: session.call_id.clone(),
                        expires_at: Instant::now()
                            + Duration::from_millis(u64::from(frame.expires_in_ms.max(1))),
                    },
                );
                serde_json::to_value(frame).map_err(|error| {
                    RpcError::new(
                        error_codes::INTERNAL_ERROR,
                        format!("сериализация кадра: {error}"),
                    )
                })
            }
            host_methods::FRAME_RELEASE => {
                let buffer_id = call
                    .params
                    .get("bufferId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        RpcError::new(error_codes::INVALID_PARAMS, "не передан bufferId")
                    })?
                    .to_owned();
                let Some(lease) = self.frames.get(&buffer_id) else {
                    return Err(RpcError::new(
                        error_codes::BUFFER_EXPIRED,
                        "ссылка на кадр истекла",
                    ));
                };
                if lease.call_id != session.call_id {
                    return Err(RpcError::new(
                        error_codes::CAPABILITY_DENIED,
                        "кадр принадлежит другому вызову",
                    ));
                }
                if Instant::now() >= lease.expires_at {
                    self.frames.remove(&buffer_id);
                    let _ = self.platform.release_frame(&session, &buffer_id);
                    return Err(RpcError::new(
                        error_codes::BUFFER_EXPIRED,
                        "ссылка на кадр истекла",
                    ));
                }
                self.frames.remove(&buffer_id);
                self.platform.release_frame(&session, &buffer_id)?;
                Ok(Value::Null)
            }
            host_methods::SELECTION_GET => {
                self.require_source(&session)?;
                self.platform.selection_get(&session)
            }
            host_methods::BUFFER_COPY => {
                self.require_source(&session)?;
                self.platform.buffer_copy(&session, &call.params)
            }
            host_methods::SETTINGS_READ => self.platform.settings_read(&session, &call.params),
            host_methods::SECRET_READ => self.platform.secret_read(&session, &call.params),
            other => Err(RpcError::new(
                error_codes::METHOD_NOT_FOUND,
                format!("неизвестная операция хоста: {other}"),
            )),
        }
    }
}

impl<H: PlatformHost> Drop for HostDispatcher<H> {
    fn drop(&mut self) {
        self.finish_all();
    }
}

/// Хост, который умеет и обслуживать запрос, и закреплять активный вызов.
pub trait SessionHost: HostHandler + HostSession {}
impl<T: HostHandler + HostSession> SessionHost for T {}
