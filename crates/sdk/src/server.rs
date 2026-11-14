//! Цикл stdio для процесса микроприложения.
//!
//! Модуль получает запросы по stdin, отвечает по stdout и пишет диагностику в
//! stderr. Цикл завершается по `shutdown`, закрытию stdin или ошибке ввода-вывода.

use std::io::{self, BufRead, Write};

use semver::Version;
use serde_json::{json, Value};

use crate::protocol::{
    error_codes, InitializeParams, InitializeResult, InvokeRequest, InvokeResult, RpcError,
    RpcRequest, RpcResponse, JSONRPC_VERSION, MAX_MESSAGE_BYTES,
};

/// Постоянные сведения о процессе модуля.
#[derive(Debug, Clone, Copy)]
pub struct ModuleInfo {
    pub id: &'static str,
    pub version: &'static str,
    /// Major-версия API хоста, с которой собран модуль.
    pub host_api: &'static str,
    pub capabilities: &'static [&'static str],
}

/// Реализация микроприложения.
pub trait Module {
    fn info(&self) -> &ModuleInfo;

    /// Подтверждение версии API и возможностей. По умолчанию проверяет major.
    fn initialize(&mut self, params: &InitializeParams) -> Result<InitializeResult, RpcError> {
        let host = Version::parse(&params.host_api).map_err(|error| {
            RpcError::new(
                error_codes::INVALID_PARAMS,
                format!("некорректная версия hostApi: {error}"),
            )
        })?;
        let own = Version::parse(self.info().host_api).map_err(|error| {
            RpcError::new(
                error_codes::INTERNAL_ERROR,
                format!("некорректная версия API модуля: {error}"),
            )
        })?;
        if host.major != own.major {
            return Err(RpcError::new(
                error_codes::HOST_API_INCOMPATIBLE,
                format!(
                    "модуль собран для API {}.x, хост предлагает {}",
                    own.major, host
                ),
            ));
        }
        Ok(InitializeResult {
            api_version: own.to_string(),
            module_id: self.info().id.to_owned(),
            capabilities: self
                .info()
                .capabilities
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
        })
    }

    /// Единственный handler действия.
    fn invoke(&mut self, request: &InvokeRequest) -> Result<InvokeResult, RpcError>;

    /// Запрос отмены активного вызова.
    fn cancel(&mut self, _call_id: &str) {}

    /// Освобождение ресурсов перед выходом.
    fn shutdown(&mut self) {}
}

/// Ошибка цикла stdio (только ввод-вывод, протокол кодируется в ответах).
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("ошибка ввода-вывода модуля: {0}")]
    Io(#[from] io::Error),
}

/// Обслуживает один процесс модуля до закрытия stdin или `shutdown`.
pub fn run_stdio<M: Module>(module: &mut M) -> Result<(), RunError> {
    let stdin = io::stdin();
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if line.len() > MAX_MESSAGE_BYTES {
            write_response(
                &mut stdout,
                &RpcResponse::error(
                    Value::Null,
                    RpcError::new(error_codes::INVALID_REQUEST, "сообщение больше 1 МиБ"),
                ),
            )?;
            continue;
        }
        let request: RpcRequest = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(error) => {
                write_response(
                    &mut stdout,
                    &RpcResponse::error(
                        Value::Null,
                        RpcError::new(
                            error_codes::PARSE_ERROR,
                            format!("некорректный JSON: {error}"),
                        ),
                    ),
                )?;
                continue;
            }
        };
        if request.jsonrpc != JSONRPC_VERSION {
            write_response(
                &mut stdout,
                &RpcResponse::error(
                    request.id.clone().unwrap_or(Value::Null),
                    RpcError::new(error_codes::INVALID_REQUEST, "ожидается JSON-RPC 2.0"),
                ),
            )?;
            continue;
        }
        let id = request.id.clone().unwrap_or(Value::Null);
        let method = request.method.clone();
        let outcome = match method.as_str() {
            "initialize" => match serde_json::from_value::<InitializeParams>(request.params) {
                Ok(params) => module.initialize(&params).and_then(|result| {
                    serde_json::to_value(result).map_err(|error| {
                        RpcError::new(
                            error_codes::INTERNAL_ERROR,
                            format!("сериализация initialize: {error}"),
                        )
                    })
                }),
                Err(error) => Err(RpcError::new(
                    error_codes::INVALID_PARAMS,
                    format!("параметры initialize: {error}"),
                )),
            },
            "invoke" => match serde_json::from_value::<InvokeRequest>(request.params) {
                Ok(params) => module.invoke(&params).and_then(|result| {
                    serde_json::to_value(result).map_err(|error| {
                        RpcError::new(
                            error_codes::INTERNAL_ERROR,
                            format!("сериализация invoke: {error}"),
                        )
                    })
                }),
                Err(error) => Err(RpcError::new(
                    error_codes::INVALID_PARAMS,
                    format!("параметры invoke: {error}"),
                )),
            },
            "cancel" => {
                let call_id = request
                    .params
                    .get("callId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                module.cancel(&call_id);
                Ok(json!({"cancelled": call_id}))
            }
            "shutdown" => {
                module.shutdown();
                write_response(&mut stdout, &RpcResponse::ok(id, json!({"stopped": true})))?;
                return Ok(());
            }
            other => Err(RpcError::new(
                error_codes::METHOD_NOT_FOUND,
                format!("неизвестный метод: {other}"),
            )),
        };
        let response = match outcome {
            Ok(result) => RpcResponse::ok(id, result),
            Err(error) => RpcResponse::error(id, error),
        };
        write_response(&mut stdout, &response)?;
    }
    Ok(())
}

fn write_response(stdout: &mut impl Write, response: &RpcResponse) -> io::Result<()> {
    let line = serde_json::to_string(response)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    stdout.write_all(line.as_bytes())?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}
