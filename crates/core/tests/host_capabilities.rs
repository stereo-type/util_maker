//! Обращения модуля к возможностям хоста во время активного вызова:
//! проверка возможности, потеря контекста и передача кадра по ссылке.

mod support;

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use util_maker_core::ipc::ModuleProcess;
use util_maker_core::CoreError;
use util_maker_sdk::{
    error_codes, FrameOrigin, FrameRef, HostCall, HostCallContext, HostHandler, InvocationSource,
    RpcError, FRAME_FORMAT_BGRA8, MAX_MESSAGE_BYTES,
};

const FIXTURE_ID: &str = "org.utilmaker.fixture";

#[derive(Default)]
struct FrameHost {
    calls: usize,
}

impl HostHandler for FrameHost {
    fn handle(&mut self, context: &HostCallContext, call: &HostCall) -> Result<Value, RpcError> {
        assert_eq!(call.call_id, context.call_id);
        assert!(context.allows("screen.frame"));
        assert_eq!(call.method, "frame.capture");
        self.calls += 1;
        Ok(serde_json::to_value(FrameRef {
            buffer_id: "frame-42".to_owned(),
            format: FRAME_FORMAT_BGRA8.to_owned(),
            width: 1920,
            height: 1080,
            stride: 1920 * 4,
            origin: FrameOrigin { x: 0, y: 0 },
            scale_factor: 1.0,
            expires_in_ms: 5_000,
            geometry: None,
            pixel_path: None,
        })
        .expect("сериализация FrameRef"))
    }
}

fn fixture(mode: &str) -> ModuleProcess {
    let executable = support::binary("fixture-module");
    assert!(executable.is_file(), "не найден {}", executable.display());
    ModuleProcess::spawn_with_args(&executable, &[mode], Arc::new(|_: &str| {}))
        .expect("запуск тестового модуля")
}

fn invoke(
    process: &mut ModuleProcess,
    host: &mut FrameHost,
    capabilities: &[String],
) -> Result<Value, CoreError> {
    process
        .invoke_with_host(
            "call-host-1",
            "any",
            InvocationSource::Hotkey,
            Duration::from_secs(5),
            host,
            capabilities,
        )
        .map(|result| result.data.unwrap_or(Value::Null))
}

#[test]
fn frame_is_returned_as_small_reference() {
    let capabilities = vec!["screen.frame".to_owned()];
    let mut process = fixture("host-call");
    process
        .initialize_with_capabilities(FIXTURE_ID, &capabilities)
        .expect("рукопожатие");
    let mut host = FrameHost::default();
    let data = invoke(&mut process, &mut host, &capabilities).expect("кадр");
    process.shutdown();

    assert_eq!(host.calls, 1);
    assert_eq!(data["bufferId"], "frame-42");
    assert_eq!(data["format"], FRAME_FORMAT_BGRA8);
    assert_eq!(data["stride"], 7680);
    assert!(data.get("pixels").is_none());
    assert!(data.get("data").is_none());
    let text = serde_json::to_string(&data).unwrap();
    assert!(text.len() < MAX_MESSAGE_BYTES);
    assert!(text.contains("expiresInMs"));
}

#[test]
fn undeclared_capability_is_denied_without_host_call() {
    let mut process = fixture("host-call");
    process
        .initialize_with_capabilities(FIXTURE_ID, &[])
        .expect("рукопожатие");
    let mut host = FrameHost::default();
    let error = invoke(&mut process, &mut host, &[]).expect_err("возможность не объявлена");
    process.shutdown();

    assert_eq!(host.calls, 0);
    match error {
        CoreError::Module { code, .. } => assert_eq!(code, error_codes::CAPABILITY_DENIED),
        other => panic!("ожидалась ошибка модуля, получено {other:?}"),
    }
}

#[test]
fn stale_call_context_is_rejected() {
    let capabilities = vec!["screen.frame".to_owned()];
    let mut process = fixture("host-context");
    process
        .initialize_with_capabilities(FIXTURE_ID, &capabilities)
        .expect("рукопожатие");
    let mut host = FrameHost::default();
    let error = invoke(&mut process, &mut host, &capabilities).expect_err("контекст потерян");
    process.shutdown();

    assert_eq!(host.calls, 0);
    match error {
        CoreError::Module { code, .. } => assert_eq!(code, error_codes::CONTEXT_LOST),
        other => panic!("ожидалась потеря контекста, получено {other:?}"),
    }
}
