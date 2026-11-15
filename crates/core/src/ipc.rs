//! Двунаправленный JSON-RPC 2.0 по stdin/stdout дочернего процесса.
//!
//! Один объект UTF-8 на строку, предел 1 МиБ. Чтение stdout идёт в отдельном
//! потоке, поэтому таймаут не блокирует вызывающего навсегда.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use semver::Version;
use serde_json::{json, Value};
use util_maker_sdk::{
    error_codes, CallContext, DeniedHost, FormCommandRequest, FormCommandResult, HostCall,
    HostCallContext, HostHandler, InitializeParams, InitializeResult, InvocationSource,
    InvokeRequest, InvokeResult, RpcError, RpcResponse, HOST_API_VERSION, HOST_CALL_METHOD,
    INITIALIZE_TIMEOUT, JSONRPC_VERSION, MAX_MESSAGE_BYTES, SHUTDOWN_TIMEOUT,
};

use crate::diagnostics::sanitize;
use crate::error::CoreError;

/// Приёмник диагностических строк модуля (stderr и служебные сообщения).
pub type Logger = Arc<dyn Fn(&str) + Send + Sync>;

enum Incoming {
    Line(String),
    Closed,
}

/// Запущенный процесс микроприложения с открытым каналом JSON-RPC.
pub struct ModuleProcess {
    executable: PathBuf,
    child: Child,
    stdin: Option<ChildStdin>,
    incoming: Receiver<Incoming>,
    stdout_reader: Option<JoinHandle<()>>,
    stderr_reader: Option<JoinHandle<()>>,
    logger: Logger,
    next_id: u64,
    module_id: Option<String>,
}

impl ModuleProcess {
    /// Запускает модуль, выводя диагностику в stderr родителя.
    pub fn spawn(executable: &Path) -> Result<Self, CoreError> {
        Self::spawn_with_args(executable, &[], Arc::new(|line: &str| eprintln!("{line}")))
    }

    /// Запускает модуль с собственным приёмником диагностики.
    pub fn spawn_with_logger(executable: &Path, logger: Logger) -> Result<Self, CoreError> {
        Self::spawn_with_args(executable, &[], logger)
    }

    /// Запускает модуль с аргументами командной строки и приёмником диагностики.
    pub fn spawn_with_args(
        executable: &Path,
        args: &[&str],
        logger: Logger,
    ) -> Result<Self, CoreError> {
        let mut command = Command::new(executable);
        command.args(args);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW: консоль модуля не должна мелькать поверх рабочего стола.
            command.creation_flags(0x0800_0000);
        }
        let mut child = command.spawn().map_err(|source| CoreError::Spawn {
            path: executable.display().to_string(),
            source,
        })?;
        let stdin = child.stdin.take().ok_or_else(|| CoreError::Spawn {
            path: executable.display().to_string(),
            source: std::io::Error::other("stdin модуля недоступен"),
        })?;
        let stdout = child.stdout.take().ok_or_else(|| CoreError::Spawn {
            path: executable.display().to_string(),
            source: std::io::Error::other("stdout модуля недоступен"),
        })?;
        let stderr = child.stderr.take().ok_or_else(|| CoreError::Spawn {
            path: executable.display().to_string(),
            source: std::io::Error::other("stderr модуля недоступен"),
        })?;

        let (sender, incoming) = mpsc::channel();
        let stdout_reader = thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        if sender.send(Incoming::Line(line)).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = sender.send(Incoming::Closed);
        });

        let stderr_logger = logger.clone();
        let stderr_reader = thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                if !line.trim().is_empty() {
                    // Необработанный stderr не попадает в журнал как есть:
                    // заголовки авторизации и URL с параметрами скрываются.
                    stderr_logger(&format!("[module] {}", sanitize(&line)));
                }
            }
        });

        Ok(Self {
            executable: executable.to_path_buf(),
            child,
            stdin: Some(stdin),
            incoming,
            stdout_reader: Some(stdout_reader),
            stderr_reader: Some(stderr_reader),
            logger,
            next_id: 0,
            module_id: None,
        })
    }

    /// Идентификатор дочернего процесса для журнала и замеров.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Путь к исполняемому файлу модуля.
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Согласует версию API и возможности. Ожидание ограничено 3 секундами.
    pub fn initialize(&mut self, module_id: &str) -> Result<InitializeResult, CoreError> {
        self.initialize_with_capabilities(module_id, &[])
    }

    /// Согласует API и передаёт модулю объявленные в манифесте возможности.
    pub fn initialize_with_capabilities(
        &mut self,
        module_id: &str,
        capabilities: &[String],
    ) -> Result<InitializeResult, CoreError> {
        self.module_id = Some(module_id.to_owned());
        let params = InitializeParams {
            host_api: HOST_API_VERSION.to_owned(),
            module_id: module_id.to_owned(),
            capabilities: capabilities.to_vec(),
        };
        let result = match self.request(
            "initialize",
            serde_json::to_value(params)?,
            INITIALIZE_TIMEOUT,
        ) {
            Ok(result) => result,
            Err(CoreError::Module { code, message })
                if code == error_codes::HOST_API_INCOMPATIBLE =>
            {
                return Err(CoreError::IncompatibleApi(message));
            }
            Err(error) => return Err(error),
        };
        let initialized: InitializeResult = serde_json::from_value(result)
            .map_err(|error| CoreError::Protocol(format!("ответ initialize: {error}")))?;
        let host = Version::parse(HOST_API_VERSION)
            .map_err(|error| CoreError::IncompatibleApi(error.to_string()))?;
        let module = Version::parse(&initialized.api_version).map_err(|error| {
            CoreError::IncompatibleApi(format!(
                "модуль вернул некорректную версию API {}: {error}",
                initialized.api_version
            ))
        })?;
        if host.major != module.major {
            return Err(CoreError::IncompatibleApi(format!(
                "хост {} несовместим с модулем {}",
                host, module
            )));
        }
        (self.logger)(&format!(
            "[ipc] модуль {} готов, api {}, pid {}",
            initialized.module_id,
            initialized.api_version,
            self.pid()
        ));
        Ok(initialized)
    }

    /// Передаёт действие и ждёт конечный исход.
    /// Выполняет команду формы настроек; секреты передаются только через
    /// возможности хоста, а не в этом запросе.
    pub fn form_command(
        &mut self,
        request: &FormCommandRequest,
        timeout: Duration,
    ) -> Result<FormCommandResult, CoreError> {
        let params = serde_json::to_value(request)
            .map_err(|error| CoreError::Protocol(format!("команда формы: {error}")))?;
        let value = self.request("command", params, timeout)?;
        serde_json::from_value(value)
            .map_err(|error| CoreError::Protocol(format!("ответ команды формы: {error}")))
    }

    pub fn invoke(
        &mut self,
        call_id: &str,
        action_id: &str,
        source: InvocationSource,
        timeout: Duration,
    ) -> Result<InvokeResult, CoreError> {
        let never = AtomicBool::new(false);
        let mut host = DeniedHost;
        self.invoke_cancellable_with_host(
            call_id,
            action_id,
            source,
            timeout,
            &never,
            &mut host,
            &[],
        )
    }

    /// Передаёт действие, прерывает ожидание по флагу и обслуживает запросы
    /// модуля к возможностям хоста.
    pub fn invoke_with_host(
        &mut self,
        call_id: &str,
        action_id: &str,
        source: InvocationSource,
        timeout: Duration,
        host: &mut dyn HostHandler,
        capabilities: &[String],
    ) -> Result<InvokeResult, CoreError> {
        let never = AtomicBool::new(false);
        self.invoke_cancellable_with_host(
            call_id,
            action_id,
            source,
            timeout,
            &never,
            host,
            capabilities,
        )
    }

    /// Передаёт действие и прерывает ожидание, когда поднят флаг отмены.
    ///
    /// После отмены модуль получает уведомление `cancel`; если он не завершился
    /// за отведённую паузу, процесс убивается. Поздний ответ уже не читается.
    pub fn invoke_cancellable(
        &mut self,
        call_id: &str,
        action_id: &str,
        source: InvocationSource,
        timeout: Duration,
        cancel: &AtomicBool,
    ) -> Result<InvokeResult, CoreError> {
        let mut host = DeniedHost;
        self.invoke_cancellable_with_host(
            call_id,
            action_id,
            source,
            timeout,
            cancel,
            &mut host,
            &[],
        )
    }

    /// Вариант [`ModuleProcess::invoke_cancellable`] с обработчиком хоста.
    #[allow(clippy::too_many_arguments)]
    pub fn invoke_cancellable_with_host(
        &mut self,
        call_id: &str,
        action_id: &str,
        source: InvocationSource,
        timeout: Duration,
        cancel: &AtomicBool,
        host: &mut dyn HostHandler,
        capabilities: &[String],
    ) -> Result<InvokeResult, CoreError> {
        let params = InvokeRequest {
            call_id: call_id.to_owned(),
            action_id: action_id.to_owned(),
            context: CallContext { source },
        };
        let id = self.next_id();
        self.send_value(&json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": id,
            "method": "invoke",
            "params": serde_json::to_value(params)?,
        }))?;
        let context = HostCallContext::new(
            self.module_id.clone().unwrap_or_default(),
            call_id,
            capabilities.to_vec(),
        );
        let result = match self.wait_result(id, "invoke", timeout, Some(cancel), host, &context) {
            Ok(result) => result,
            Err(CoreError::Cancelled { .. }) => {
                self.cancel_and_stop(call_id);
                return Err(CoreError::Cancelled {
                    call_id: call_id.to_owned(),
                });
            }
            Err(error) => return Err(error),
        };
        serde_json::from_value(result)
            .map_err(|error| CoreError::Protocol(format!("ответ invoke: {error}")))
    }

    /// Просит модуль отменить активный вызов.
    pub fn cancel(&mut self, call_id: &str) -> Result<(), CoreError> {
        self.request("cancel", json!({"callId": call_id}), Duration::from_secs(1))
            .map(|_| ())
    }

    /// Штатно завершает процесс, а при отказе убивает его.
    pub fn shutdown(mut self) {
        let _ = self.request("shutdown", Value::Null, SHUTDOWN_TIMEOUT);
        let _ = self.wait_for_exit(SHUTDOWN_TIMEOUT);
        self.terminate();
    }

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn send_value(&mut self, value: &Value) -> Result<(), CoreError> {
        let line = serde_json::to_string(value)?;
        if line.len() > MAX_MESSAGE_BYTES {
            return Err(CoreError::MessageTooLarge);
        }
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| CoreError::ProcessExited("поток stdin модуля закрыт".to_owned()))?;
        stdin.write_all(line.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        Ok(())
    }

    fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, CoreError> {
        let id = self.next_id();
        self.send_value(&json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": id,
            "method": method,
            "params": params,
        }))?;
        let mut host = DeniedHost;
        let context = HostCallContext::new(
            self.module_id.clone().unwrap_or_default(),
            String::new(),
            Vec::new(),
        );
        self.wait_result(id, method, timeout, None, &mut host, &context)
    }

    fn wait_result(
        &mut self,
        expected_id: u64,
        method: &str,
        timeout: Duration,
        cancel: Option<&AtomicBool>,
        host: &mut dyn HostHandler,
        context: &HostCallContext,
    ) -> Result<Value, CoreError> {
        let deadline = Instant::now() + timeout;
        loop {
            if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
                return Err(CoreError::Cancelled {
                    call_id: String::new(),
                });
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(CoreError::Timeout {
                    method: method.to_owned(),
                    timeout,
                });
            }
            // Проверяем отмену не реже, чем раз в 50 мс.
            let slice = if cancel.is_some() {
                remaining.min(Duration::from_millis(50))
            } else {
                remaining
            };
            match self.incoming.recv_timeout(slice) {
                Ok(Incoming::Line(line)) => {
                    if line.len() > MAX_MESSAGE_BYTES {
                        return Err(CoreError::MessageTooLarge);
                    }
                    let value: Value = serde_json::from_str(&line).map_err(|error| {
                        CoreError::Protocol(format!("некорректный JSON от модуля: {error}"))
                    })?;
                    if value.get("method").is_some() {
                        self.handle_host_request(&value, host, context)?;
                        continue;
                    }
                    if !has_id(&value, expected_id) {
                        // Чужое/запоздалое сообщение не должно протащить в
                        // журнал заголовки авторизации или параметры URL.
                        let preview = sanitize(&truncate(&line));
                        (self.logger)(&format!(
                            "[ipc] пропущено сообщение без ожидаемого id {expected_id}: {preview}"
                        ));
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
                        return Err(CoreError::Module { code, message });
                    }
                    return Ok(value.get("result").cloned().unwrap_or(Value::Null));
                }
                Ok(Incoming::Closed) => {
                    return Err(CoreError::ProcessExited(self.exit_description()));
                }
                Err(RecvTimeoutError::Timeout) => {
                    if Instant::now() >= deadline {
                        return Err(CoreError::Timeout {
                            method: method.to_owned(),
                            timeout,
                        });
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(CoreError::ProcessExited(self.exit_description()));
                }
            }
        }
    }

    /// Обслуживает запрос модуля к хосту и возвращает ровно один ответ.
    fn handle_host_request(
        &mut self,
        value: &Value,
        host: &mut dyn HostHandler,
        context: &HostCallContext,
    ) -> Result<(), CoreError> {
        let id = value.get("id").cloned().unwrap_or(Value::Null);
        if value.get("jsonrpc").and_then(Value::as_str) != Some(JSONRPC_VERSION) {
            let response = RpcResponse::error(
                id,
                RpcError::new(error_codes::INVALID_REQUEST, "ожидается JSON-RPC 2.0"),
            );
            return self.send_value(&serde_json::to_value(response)?);
        }
        let method = value
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if method != HOST_CALL_METHOD {
            let response = RpcResponse::error(
                id,
                RpcError::new(
                    error_codes::METHOD_NOT_FOUND,
                    format!("неизвестный метод обращения к хосту: {method}"),
                ),
            );
            return self.send_value(&serde_json::to_value(response)?);
        }
        let call: HostCall =
            match serde_json::from_value(value.get("params").cloned().unwrap_or(Value::Null)) {
                Ok(call) => call,
                Err(error) => {
                    let response = RpcResponse::error(
                        id,
                        RpcError::new(
                            error_codes::INVALID_PARAMS,
                            format!("параметры host.call: {error}"),
                        ),
                    );
                    return self.send_value(&serde_json::to_value(response)?);
                }
            };
        let outcome = if call.call_id != context.call_id {
            Err(RpcError::new(
                error_codes::CONTEXT_LOST,
                format!(
                    "запрос относится к вызову {}, активен {}",
                    call.call_id, context.call_id
                ),
            ))
        } else if call.capability.trim().is_empty() || !context.allows(&call.capability) {
            Err(RpcError::new(
                error_codes::CAPABILITY_DENIED,
                format!(
                    "модуль {} не объявил возможность {}",
                    context.module_id, call.capability
                ),
            ))
        } else {
            host.handle(context, &call)
        };
        let response = match outcome {
            Ok(result) => RpcResponse::ok(id, result),
            Err(error) => RpcResponse::error(id, error),
        };
        self.send_value(&serde_json::to_value(response)?)
    }

    /// Отправляет `cancel`, даёт процессу короткую паузу и завершает его.
    fn cancel_and_stop(&mut self, call_id: &str) {
        let id = self.next_id();
        let _ = self.send_value(&json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": id,
            "method": "cancel",
            "params": {"callId": call_id},
        }));
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        self.terminate();
    }

    fn exit_description(&mut self) -> String {
        match self.child.try_wait() {
            Ok(Some(status)) => format!("код {status}"),
            Ok(None) => "процесс ещё работает, но закрыл stdout".to_owned(),
            Err(error) => format!("не удалось получить код: {error}"),
        }
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> Result<(), CoreError> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait()? {
                Some(_) => return Ok(()),
                None if Instant::now() >= deadline => {
                    return Err(CoreError::Timeout {
                        method: "exit".to_owned(),
                        timeout,
                    })
                }
                None => thread::sleep(Duration::from_millis(10)),
            }
        }
    }

    fn terminate(&mut self) {
        self.stdin.take();
        let running = matches!(self.child.try_wait(), Ok(None));
        if running {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        if let Some(handle) = self.stdout_reader.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.stderr_reader.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for ModuleProcess {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn has_id(value: &Value, expected: u64) -> bool {
    match value.get("id") {
        Some(Value::Number(number)) => number.as_u64() == Some(expected),
        Some(Value::String(text)) => text == &expected.to_string(),
        _ => false,
    }
}

fn truncate(line: &str) -> String {
    const LIMIT: usize = 256;
    if line.len() <= LIMIT {
        return line.to_owned();
    }
    let mut end = LIMIT;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &line[..end])
}
