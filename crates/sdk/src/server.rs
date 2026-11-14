//! Цикл stdio микроприложения: запросы хоста, ответы и обращения к хосту.
//!
//! Модуль получает запросы по stdin, отвечает по stdout и пишет диагностику в
//! stderr. Цикл завершается по `shutdown`, закрытию stdin или ошибке ввода-вывода.
//! Пока обработчик действия ждёт ответ хоста, цикл принимает `cancel` и
//! `shutdown`, поэтому отмена не теряется на время операции.

use std::io::{self, BufRead, Write};
use std::sync::mpsc::{self, Receiver, RecvError};
use std::thread;

use semver::Version;
use serde_json::{json, Value};

use crate::host::{HostBridge, HostCall, HostCallError, HOST_CALL_METHOD};
use crate::protocol::{
    error_codes, FormCommandRequest, FormCommandResult, InitializeParams, InitializeResult,
    InvokeRequest, InvokeResult, RpcError, RpcRequest, RpcResponse, JSONRPC_VERSION,
    MAX_MESSAGE_BYTES,
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

    /// Вариант handler'а, которому доступны возможности хоста. По умолчанию
    /// делегирует в [`Module::invoke`], поэтому старые модули не меняются.
    fn invoke_with_host(
        &mut self,
        request: &InvokeRequest,
        _host: &mut dyn HostBridge,
    ) -> Result<InvokeResult, RpcError> {
        self.invoke(request)
    }

    /// Команда формы настроек, например проверка подключения.
    fn command(&mut self, _request: &FormCommandRequest) -> Result<FormCommandResult, RpcError> {
        Err(RpcError::new(
            error_codes::METHOD_NOT_FOUND,
            "модуль не поддерживает команды формы",
        ))
    }

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

enum InputEvent {
    Line(String),
    Closed,
}

struct StdioHostBridge<'a, W: Write> {
    output: &'a mut W,
    incoming: &'a Receiver<InputEvent>,
    call_id: String,
    capabilities: Vec<String>,
    next_id: u64,
    cancelled: bool,
    shutdown_requested: bool,
}

impl<'a, W: Write> StdioHostBridge<'a, W> {
    fn new(
        output: &'a mut W,
        incoming: &'a Receiver<InputEvent>,
        call_id: String,
        capabilities: Vec<String>,
    ) -> Self {
        Self {
            output,
            incoming,
            call_id,
            capabilities,
            next_id: 1,
            cancelled: false,
            shutdown_requested: false,
        }
    }

    fn respond(&mut self, response: &RpcResponse) -> Result<(), HostCallError> {
        write_response(self.output, response)
            .map_err(|error| HostCallError::Protocol(error.to_string()))
    }

    fn answer_control(&mut self, value: &Value, method: &str) -> Result<(), HostCallError> {
        let id = value.get("id").cloned().unwrap_or(Value::Null);
        match method {
            "cancel" => {
                let call_id = value
                    .get("params")
                    .and_then(|params| params.get("callId"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if call_id != self.call_id {
                    return self.respond(&RpcResponse::error(
                        id,
                        RpcError::new(
                            error_codes::INVALID_PARAMS,
                            format!("нет активного вызова {call_id}"),
                        ),
                    ));
                }
                self.cancelled = true;
                self.respond(&RpcResponse::ok(id, json!({"cancelled": call_id})))?;
                Err(HostCallError::Cancelled)
            }
            "shutdown" => {
                self.shutdown_requested = true;
                self.respond(&RpcResponse::ok(id, json!({"stopped": true})))?;
                Err(HostCallError::Shutdown)
            }
            other => self.respond(&RpcResponse::error(
                id,
                RpcError::new(
                    error_codes::METHOD_NOT_FOUND,
                    format!("во время вызова неизвестный метод: {other}"),
                ),
            )),
        }
    }
}

impl<W: Write> HostBridge for StdioHostBridge<'_, W> {
    fn request(
        &mut self,
        capability: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, HostCallError> {
        if !self.capabilities.iter().any(|item| item == capability) {
            return Err(HostCallError::Denied(format!(
                "модуль не объявил возможность {capability}"
            )));
        }
        let id = Value::from(self.next_id);
        self.next_id += 1;
        let call = HostCall {
            call_id: self.call_id.clone(),
            capability: capability.to_owned(),
            method: method.to_owned(),
            params,
        };
        let request = json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": id,
            "method": HOST_CALL_METHOD,
            "params": serde_json::to_value(call)
                .map_err(|error| HostCallError::Protocol(error.to_string()))?,
        });
        let line = serde_json::to_string(&request)
            .map_err(|error| HostCallError::Protocol(error.to_string()))?;
        if line.len() > MAX_MESSAGE_BYTES {
            return Err(HostCallError::Protocol(
                "запрос хосту больше 1 МиБ".to_owned(),
            ));
        }
        self.output
            .write_all(line.as_bytes())
            .and_then(|()| self.output.write_all(b"\n"))
            .and_then(|()| self.output.flush())
            .map_err(|error| HostCallError::Protocol(error.to_string()))?;

        loop {
            let event = self.incoming.recv().map_err(closed_error)?;
            let InputEvent::Line(line) = event else {
                return Err(HostCallError::Protocol("поток ввода закрыт".to_owned()));
            };
            if line.trim().is_empty() {
                continue;
            }
            if line.len() > MAX_MESSAGE_BYTES {
                return Err(HostCallError::Protocol(
                    "ответ хоста больше 1 МиБ".to_owned(),
                ));
            }
            let value: Value = serde_json::from_str(&line)
                .map_err(|error| HostCallError::Protocol(format!("ответ хоста: {error}")))?;
            if let Some(method) = value.get("method").and_then(Value::as_str) {
                self.answer_control(&value, method)?;
                continue;
            }
            if value.get("id") != Some(&id) {
                continue;
            }
            if let Some(error) = value.get("error") {
                let code = error
                    .get("code")
                    .and_then(Value::as_i64)
                    .unwrap_or(error_codes::INTERNAL_ERROR);
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("без описания")
                    .to_owned();
                return Err(match code {
                    error_codes::CAPABILITY_DENIED => HostCallError::Denied(message),
                    error_codes::CONTEXT_LOST => HostCallError::ContextLost(message),
                    error_codes::CANCELLED => HostCallError::Cancelled,
                    _ => HostCallError::Protocol(format!("{message} (код {code})")),
                });
            }
            return Ok(value.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled
    }
}

fn closed_error(_error: RecvError) -> HostCallError {
    HostCallError::Protocol("канал ввода модуля закрыт".to_owned())
}

/// Обслуживает один процесс модуля до закрытия stdin или `shutdown`.
pub fn run_stdio<M: Module>(module: &mut M) -> Result<(), RunError> {
    let mut stdout = io::BufWriter::new(io::stdout());
    run_stdio_with_io(module, io::BufReader::new(io::stdin()), &mut stdout)
}

/// Вариант [`run_stdio`] с явными потоками. Используется тестами и позволяет
/// проверить протокол без реального процесса.
pub fn run_stdio_with_io<M, R, W>(module: &mut M, input: R, output: &mut W) -> Result<(), RunError>
where
    M: Module,
    R: BufRead + Send + 'static,
    W: Write,
{
    let (sender, incoming) = mpsc::channel();
    // Поток чтения отделяет ожидание строки stdin от работы handler'а. Это
    // позволяет принимать cancel/shutdown, пока handler ждёт ответ хоста.
    thread::spawn(move || {
        let mut reader = input;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if sender.send(InputEvent::Line(line)).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = sender.send(InputEvent::Closed);
    });

    while let Ok(event) = incoming.recv() {
        let InputEvent::Line(line) = event else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        if line.len() > MAX_MESSAGE_BYTES {
            write_response(
                output,
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
                    output,
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
                output,
                &RpcResponse::error(
                    request.id.clone().unwrap_or(Value::Null),
                    RpcError::new(error_codes::INVALID_REQUEST, "ожидается JSON-RPC 2.0"),
                ),
            )?;
            continue;
        }
        let id = request.id.clone().unwrap_or(Value::Null);
        let method = request.method.clone();
        let mut shutdown_after = false;
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
                Ok(params) => {
                    let call_id = params.call_id.clone();
                    let capabilities = module
                        .info()
                        .capabilities
                        .iter()
                        .map(|value| (*value).to_owned())
                        .collect();
                    let result = {
                        let mut host =
                            StdioHostBridge::new(output, &incoming, call_id, capabilities);
                        let result =
                            module
                                .invoke_with_host(&params, &mut host)
                                .and_then(|result| {
                                    serde_json::to_value(result).map_err(|error| {
                                        RpcError::new(
                                            error_codes::INTERNAL_ERROR,
                                            format!("сериализация invoke: {error}"),
                                        )
                                    })
                                });
                        shutdown_after = host.shutdown_requested;
                        result
                    };
                    if shutdown_after {
                        module.shutdown();
                    }
                    result
                }
                Err(error) => Err(RpcError::new(
                    error_codes::INVALID_PARAMS,
                    format!("параметры invoke: {error}"),
                )),
            },
            "command" => match serde_json::from_value::<FormCommandRequest>(request.params) {
                Ok(params) => module.command(&params).and_then(|result| {
                    serde_json::to_value(result).map_err(|error| {
                        RpcError::new(
                            error_codes::INTERNAL_ERROR,
                            format!("сериализация command: {error}"),
                        )
                    })
                }),
                Err(error) => Err(RpcError::new(
                    error_codes::INVALID_PARAMS,
                    format!("параметры command: {error}"),
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
                write_response(output, &RpcResponse::ok(id, json!({"stopped": true})))?;
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
        write_response(output, &response)?;
        if shutdown_after {
            return Ok(());
        }
    }
    Ok(())
}

fn write_response(output: &mut impl Write, response: &RpcResponse) -> io::Result<()> {
    let line = serde_json::to_string(response)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    output.write_all(line.as_bytes())?;
    output.write_all(b"\n")?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::host_methods;
    use crate::protocol::FormFieldError;
    use serde_json::json;

    static INFO: ModuleInfo = ModuleInfo {
        id: "org.example.host-test",
        version: "0.1.0",
        host_api: "1.0.0",
        capabilities: &["screen.frame"],
    };

    struct TestModule;

    impl Module for TestModule {
        fn info(&self) -> &ModuleInfo {
            &INFO
        }

        fn invoke(&mut self, request: &InvokeRequest) -> Result<InvokeResult, RpcError> {
            match request.action_id.as_str() {
                "host" => Ok(InvokeResult::ok("готово")),
                other => Err(RpcError::new(
                    error_codes::ACTION_NOT_FOUND,
                    format!("нет действия {other}"),
                )),
            }
        }

        fn invoke_with_host(
            &mut self,
            request: &InvokeRequest,
            host: &mut dyn HostBridge,
        ) -> Result<InvokeResult, RpcError> {
            if request.action_id != "host" {
                return self.invoke(request);
            }
            match host.request("screen.frame", host_methods::FRAME_CAPTURE, json!({})) {
                Ok(frame) => Ok(InvokeResult::ok("кадр получен").with_data(frame)),
                Err(error) => Err(error.into_rpc_error()),
            }
        }

        fn command(&mut self, request: &FormCommandRequest) -> Result<FormCommandResult, RpcError> {
            Ok(FormCommandResult {
                message: Some(format!("команда {} выполнена", request.command_id)),
                data: None,
                field_errors: vec![FormFieldError {
                    field_id: "apiKey".to_owned(),
                    code: "required".to_owned(),
                    message: "Укажите ключ".to_owned(),
                }],
            })
        }
    }

    fn run(lines: &[&str]) -> Vec<Value> {
        let input = lines.join("\n") + "\n";
        let mut output = Vec::new();
        run_stdio_with_io(&mut TestModule, io::Cursor::new(input), &mut output).unwrap();
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|value| value.get("method").is_none())
            .collect()
    }

    fn initialize() -> &'static str {
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"hostApi":"1.0.0","moduleId":"org.example.host-test"}}"#
    }

    #[test]
    fn host_call_is_matched_with_live_call() {
        let responses = run(&[
            initialize(),
            r#"{"jsonrpc":"2.0","id":2,"method":"invoke","params":{"callId":"call-7","actionId":"host"}}"#,
            r#"{"jsonrpc":"2.0","id":1,"result":{"bufferId":"frame-1","format":"bgra8","width":1920,"height":1080,"stride":7680,"origin":{"x":0,"y":0},"scaleFactor":1.0,"expiresInMs":5000}}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"shutdown","params":null}"#,
        ]);
        assert_eq!(responses.len(), 3);
        assert_eq!(responses[1]["result"]["status"], "ok");
        assert_eq!(responses[1]["result"]["data"]["bufferId"], "frame-1");
        assert!(responses[1]["result"]["data"].get("pixels").is_none());
    }

    #[test]
    fn cancel_is_accepted_while_handler_waits_for_host() {
        let responses = run(&[
            initialize(),
            r#"{"jsonrpc":"2.0","id":2,"method":"invoke","params":{"callId":"call-8","actionId":"host"}}"#,
            r#"{"jsonrpc":"2.0","id":9,"method":"cancel","params":{"callId":"call-8"}}"#,
        ]);
        assert_eq!(responses[1]["result"]["cancelled"], "call-8");
        assert_eq!(responses[2]["id"], 2);
        assert_eq!(responses[2]["error"]["code"], error_codes::CANCELLED);
    }

    #[test]
    fn shutdown_is_accepted_while_handler_waits_for_host() {
        let responses = run(&[
            initialize(),
            r#"{"jsonrpc":"2.0","id":2,"method":"invoke","params":{"callId":"call-9","actionId":"host"}}"#,
            r#"{"jsonrpc":"2.0","id":10,"method":"shutdown","params":null}"#,
        ]);
        assert_eq!(responses[1]["result"]["stopped"], true);
        assert_eq!(responses[2]["error"]["code"], error_codes::CANCELLED);
    }

    #[test]
    fn command_returns_field_errors() {
        let responses = run(&[
            initialize(),
            r#"{"jsonrpc":"2.0","id":2,"method":"command","params":{"commandId":"testConnection","profile":"yandex","values":{"apiKey":"***"}}}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"shutdown","params":null}"#,
        ]);
        assert_eq!(
            responses[1]["result"]["fieldErrors"][0]["fieldId"],
            "apiKey"
        );
        assert_eq!(responses[1]["result"]["fieldErrors"][0]["code"], "required");
    }
}
