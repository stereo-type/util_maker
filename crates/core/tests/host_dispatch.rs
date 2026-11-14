//! Возможности хоста закреплены за активным вызовом и конкретным модулем.

mod support;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use util_maker_core::capabilities::{
    CallSession, HostDispatcher, HostSession, InvocationContext, PlatformHost, SourceRef,
};
use util_maker_core::ipc::ModuleProcess;
use util_maker_sdk::{
    error_codes, host_methods, FrameOrigin, FrameRef, FrameRequest, HostCall, HostCallContext,
    HostHandler, InvocationSource, RpcError, FRAME_FORMAT_BGRA8,
};

#[derive(Default)]
struct FakePlatform {
    next_frame: u64,
    secrets: BTreeMap<(String, String), String>,
    settings: BTreeMap<String, Value>,
    released: Vec<String>,
    expires_in_ms: u32,
}

impl FakePlatform {
    fn with_module(&mut self, module_id: &str) {
        self.secrets.insert(
            (module_id.to_owned(), "token".to_owned()),
            format!("secret-{module_id}"),
        );
        self.settings
            .insert(module_id.to_owned(), json!({"module": module_id}));
    }
}

#[derive(Clone)]
struct SharedPlatform(Arc<Mutex<FakePlatform>>);

impl PlatformHost for SharedPlatform {
    fn capture_frame(
        &mut self,
        context: &CallSession,
        _request: &FrameRequest,
    ) -> Result<FrameRef, RpcError> {
        let mut platform = self.0.lock().expect("платформа");
        platform.next_frame += 1;
        Ok(FrameRef {
            buffer_id: format!("{}-frame-{}", context.module_id, platform.next_frame),
            format: FRAME_FORMAT_BGRA8.to_owned(),
            width: 10,
            height: 10,
            stride: 40,
            origin: FrameOrigin { x: 0, y: 0 },
            scale_factor: 1.0,
            expires_in_ms: platform.expires_in_ms,
        })
    }

    fn release_frame(&mut self, _context: &CallSession, buffer_id: &str) -> Result<(), RpcError> {
        self.0
            .lock()
            .expect("платформа")
            .released
            .push(buffer_id.to_owned());
        Ok(())
    }

    fn selection_get(&mut self, context: &CallSession) -> Result<Value, RpcError> {
        Ok(json!({"text": format!("selection-{}", context.module_id)}))
    }

    fn buffer_copy(&mut self, context: &CallSession, _params: &Value) -> Result<Value, RpcError> {
        Ok(json!({"copiedBy": context.module_id}))
    }

    fn settings_read(&mut self, context: &CallSession, _params: &Value) -> Result<Value, RpcError> {
        Ok(self
            .0
            .lock()
            .expect("платформа")
            .settings
            .get(&context.module_id)
            .cloned()
            .unwrap_or(Value::Null))
    }

    fn secret_read(&mut self, context: &CallSession, params: &Value) -> Result<Value, RpcError> {
        let key = params
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Ok(self
            .0
            .lock()
            .expect("платформа")
            .secrets
            .get(&(context.module_id.clone(), key.to_owned()))
            .cloned()
            .map(Value::String)
            .unwrap_or(Value::Null))
    }
}

fn platform() -> Arc<Mutex<FakePlatform>> {
    let platform = Arc::new(Mutex::new(FakePlatform {
        expires_in_ms: 5_000,
        ..FakePlatform::default()
    }));
    platform
        .lock()
        .expect("платформа")
        .with_module("org.example.one");
    platform
        .lock()
        .expect("платформа")
        .with_module("org.example.two");
    platform
}

fn session(call_id: &str, module_id: &str, _capabilities: &[&str]) -> CallSession {
    CallSession::new(
        call_id,
        module_id,
        "run",
        InvocationContext::hotkey(Some(SourceRef("window-1".to_owned()))),
    )
}

fn context(call_id: &str, module_id: &str, capabilities: &[&str]) -> HostCallContext {
    HostCallContext::new(
        module_id,
        call_id,
        capabilities
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
    )
}

#[test]
fn frames_and_secrets_stay_with_their_call() {
    let platform = platform();
    let mut dispatcher = HostDispatcher::new(SharedPlatform(platform.clone()));
    dispatcher.begin_call(&session("one", "org.example.one", &["screen.frame"]));
    dispatcher.begin_call(&session("two", "org.example.two", &["screen.frame"]));

    let frame = dispatcher
        .handle(
            &context("one", "org.example.one", &["screen.frame"]),
            &HostCall {
                call_id: "one".to_owned(),
                capability: "screen.frame".to_owned(),
                method: host_methods::FRAME_CAPTURE.to_owned(),
                params: json!({}),
            },
        )
        .expect("кадр");
    let buffer = frame["bufferId"].as_str().expect("ID").to_owned();
    assert!(buffer.contains("org.example.one"));

    let foreign = dispatcher
        .handle(
            &context("two", "org.example.two", &["screen.frame"]),
            &HostCall {
                call_id: "two".to_owned(),
                capability: "screen.frame".to_owned(),
                method: host_methods::FRAME_RELEASE.to_owned(),
                params: json!({"bufferId": buffer}),
            },
        )
        .expect_err("чужой кадр");
    assert_eq!(foreign.code, error_codes::CAPABILITY_DENIED);

    let secret_one = dispatcher
        .handle(
            &context("one", "org.example.one", &["secret.read"]),
            &HostCall {
                call_id: "one".to_owned(),
                capability: "secret.read".to_owned(),
                method: host_methods::SECRET_READ.to_owned(),
                params: json!({"key": "token"}),
            },
        )
        .expect("секрет one");
    let secret_two = dispatcher
        .handle(
            &context("two", "org.example.two", &["secret.read"]),
            &HostCall {
                call_id: "two".to_owned(),
                capability: "secret.read".to_owned(),
                method: host_methods::SECRET_READ.to_owned(),
                params: json!({"key": "token"}),
            },
        )
        .expect("секрет two");
    assert_eq!(secret_one, json!("secret-org.example.one"));
    assert_eq!(secret_two, json!("secret-org.example.two"));

    dispatcher.finish_call("one");
    let released = platform.lock().expect("платформа").released.clone();
    assert!(released.iter().any(|item| item == &buffer));
}

#[test]
fn expired_frame_is_rejected() {
    let platform = platform();
    let mut dispatcher = HostDispatcher::new(SharedPlatform(platform));
    dispatcher
        .platform_mut()
        .0
        .lock()
        .expect("платформа")
        .expires_in_ms = 1;
    dispatcher.begin_call(&session("one", "org.example.one", &["screen.frame"]));
    let frame = dispatcher
        .handle(
            &context("one", "org.example.one", &["screen.frame"]),
            &HostCall {
                call_id: "one".to_owned(),
                capability: "screen.frame".to_owned(),
                method: host_methods::FRAME_CAPTURE.to_owned(),
                params: json!({}),
            },
        )
        .expect("кадр");
    let buffer = frame["bufferId"].as_str().expect("ID").to_owned();
    std::thread::sleep(Duration::from_millis(10));
    let error = dispatcher
        .handle(
            &context("one", "org.example.one", &["screen.frame"]),
            &HostCall {
                call_id: "one".to_owned(),
                capability: "screen.frame".to_owned(),
                method: host_methods::FRAME_RELEASE.to_owned(),
                params: json!({"bufferId": buffer}),
            },
        )
        .expect_err("просроченный кадр");
    assert_eq!(error.code, error_codes::BUFFER_EXPIRED);
}

#[test]
fn lost_context_is_distinct_from_denied_capability() {
    let platform = platform();
    let mut dispatcher = HostDispatcher::new(SharedPlatform(platform));
    dispatcher.begin_call(&CallSession::new(
        "lost",
        "org.example.one",
        "run",
        InvocationContext::tray(None),
    ));
    let error = dispatcher
        .handle(
            &context("lost", "org.example.one", &["screen.frame"]),
            &HostCall {
                call_id: "lost".to_owned(),
                capability: "screen.frame".to_owned(),
                method: host_methods::FRAME_CAPTURE.to_owned(),
                params: json!({}),
            },
        )
        .expect_err("источник потерян");
    assert_eq!(error.code, error_codes::CONTEXT_LOST);

    let denied = dispatcher
        .handle(
            &context("lost", "org.example.one", &[]),
            &HostCall {
                call_id: "lost".to_owned(),
                capability: "screen.frame".to_owned(),
                method: host_methods::FRAME_CAPTURE.to_owned(),
                params: json!({}),
            },
        )
        .expect_err("возможность не объявлена");
    assert_eq!(denied.code, error_codes::CAPABILITY_DENIED);
}

#[test]
fn module_call_is_dispatched_with_live_context() {
    let executable = support::binary("fixture-module");
    if !executable.is_file() {
        return;
    }
    let platform = platform();
    let mut dispatcher = HostDispatcher::new(SharedPlatform(platform));
    dispatcher.begin_call(&session(
        "call-host-1",
        "org.utilmaker.fixture",
        &["screen.frame"],
    ));
    let mut process = ModuleProcess::spawn_with_args(
        &executable,
        &["host-call"],
        Arc::new(|line: &str| eprintln!("{line}")),
    )
    .expect("процесс");
    process
        .initialize_with_capabilities("org.utilmaker.fixture", &["screen.frame".to_owned()])
        .expect("рукопожатие");
    let result = process
        .invoke_with_host(
            "call-host-1",
            "any",
            InvocationSource::Hotkey,
            Duration::from_secs(5),
            &mut dispatcher,
            &["screen.frame".to_owned()],
        )
        .expect("вызов");
    process.shutdown();
    dispatcher.finish_call("call-host-1");
    assert_eq!(result.status, util_maker_sdk::InvokeStatus::Ok);
    assert!(result
        .message
        .unwrap_or_default()
        .contains("fixture-module: кадр получен"));
}
