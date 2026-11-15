//! Интеграционные проверки реестра плагинов-расширений: выбор вклада,
//! сохранение, изоляция возможностей, ошибки и отмена дочернего процесса.

mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use util_maker_core::{CallSession, HostSession, InvocationContext, Registry};
use util_maker_sdk::{
    error_codes, ExtensionInvokeRequest, HostCall, HostCallContext, HostHandler, InvocationSource,
    Manifest, RpcError,
};

const PARENT_ID: &str = "org.example.parent";
const POINT_ID: &str = "translation-provider";
const CONTRACT: &str = "util-maker.translation.provider";
const YANDEX: &str = "org.example.parent.yandex";
const GOOGLE: &str = "org.example.parent.google";

#[derive(Default)]
struct TestHost {
    calls: Vec<String>,
    capabilities: Vec<Vec<String>>,
    begun: Vec<String>,
    finished: Vec<String>,
}

impl HostHandler for TestHost {
    fn handle(&mut self, context: &HostCallContext, call: &HostCall) -> Result<Value, RpcError> {
        self.calls
            .push(format!("{}:{}", context.module_id, call.method));
        self.capabilities.push(context.capabilities.clone());
        Ok(json!({
            "moduleId": context.module_id,
            "capabilities": context.capabilities,
        }))
    }
}

impl HostSession for TestHost {
    fn begin_call(&mut self, context: &CallSession) {
        self.begun.push(context.call_id.clone());
    }

    fn finish_call(&mut self, call_id: &str) {
        self.finished.push(call_id.to_owned());
    }
}

fn manifest(value: Value) -> Manifest {
    serde_json::from_value(value).expect("манифест")
}

fn parent_manifest() -> Manifest {
    manifest(json!({
        "manifestVersion": 1,
        "id": PARENT_ID,
        "publisher": "org.example",
        "name": "Родитель",
        "version": "1.0.0",
        "hostApi": ">=1.1.0 <2.0.0",
        "entrypoints": {"linux-x64": "fixture-extension"},
        "capabilities": ["extensions.read", "extensions.call"],
        "actions": [{"id": "translate", "name": "Перевести"}],
        "extensionPoints": [{
            "id": POINT_ID,
            "name": "Провайдер перевода",
            "contract": CONTRACT,
            "contractVersion": "1.0.0",
            "selection": "single",
            "required": true
        }]
    }))
}

fn provider_manifest(id: &str, name: &str, contract: &str, interface: &str) -> Manifest {
    manifest(json!({
        "manifestVersion": 1,
        "id": id,
        "publisher": "org.example",
        "name": name,
        "version": "1.0.0",
        "hostApi": ">=1.1.0 <2.0.0",
        "entrypoints": {"linux-x64": "fixture-extension"},
        "capabilities": ["settings.read"],
        "actions": [],
        "provides": [{
            "moduleId": PARENT_ID,
            "pointId": POINT_ID,
            "contract": contract,
            "interfaceVersion": interface,
            "name": name
        }]
    }))
}

fn yandex() -> Manifest {
    provider_manifest(YANDEX, "Яндекс", CONTRACT, ">=1.0.0 <2.0.0")
}

fn google() -> Manifest {
    provider_manifest(GOOGLE, "Google", CONTRACT, ">=1.0.0 <2.0.0")
}

fn binary() -> PathBuf {
    support::binary("fixture-extension")
}

fn temp_directory(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "util-maker-extension-{label}-{}-{sequence}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("каталог");
    path
}

fn registry(path: Option<PathBuf>) -> Registry {
    match path {
        Some(path) => Registry::with_state_path(path.join("packages.json")).expect("реестр"),
        None => Registry::new(),
    }
}

fn add(registry: &Registry, manifest: Manifest) {
    let module_id = manifest.id.clone();
    registry
        .add_entry(
            util_maker_core::ModuleEntry::new(manifest, binary(), true)
                .with_arguments(vec!["--module-id".to_owned(), module_id]),
        )
        .expect("модуль");
}

fn parent_call(call_id: &str) -> CallSession {
    CallSession::new(
        call_id,
        PARENT_ID,
        "translate",
        InvocationContext::new(InvocationSource::Tray),
    )
}

fn request(operation: &str, params: Value) -> ExtensionInvokeRequest {
    ExtensionInvokeRequest {
        call_id: "child-call".to_owned(),
        point_id: POINT_ID.to_owned(),
        operation: operation.to_owned(),
        timeout_ms: Some(10_000),
        params,
    }
}

#[test]
fn selection_is_automatic_then_explicit_and_persists() {
    let root = temp_directory("selection");

    {
        let registry = registry(Some(root.clone()));
        add(&registry, parent_manifest());
        add(&registry, yandex());
        add(&registry, google());

        let points = registry.extension_points(PARENT_ID);
        assert_eq!(points.len(), 1);
        assert!(points[0].needs_selection);
        assert_eq!(points[0].contributions.len(), 2);

        let list = registry
            .extension_list(PARENT_ID, POINT_ID)
            .expect("список");
        assert!(list.selected.is_none());

        registry
            .select_extension(PARENT_ID, POINT_ID, YANDEX)
            .expect("выбор");
        let list = registry
            .extension_list(PARENT_ID, POINT_ID)
            .expect("список");
        assert_eq!(list.selected.as_deref(), Some(YANDEX));
    }

    let registry = registry(Some(root.clone()));
    add(&registry, parent_manifest());
    add(&registry, yandex());
    add(&registry, google());
    let list = registry
        .extension_list(PARENT_ID, POINT_ID)
        .expect("список");
    assert_eq!(list.selected.as_deref(), Some(YANDEX));
    assert!(root.join("extensions.json").exists());

    registry.set_enabled(YANDEX, false).expect("отключение");
    let list = registry
        .extension_list(PARENT_ID, POINT_ID)
        .expect("список");
    assert_eq!(list.selected.as_deref(), Some(GOOGLE));
    assert!(list
        .contributions
        .iter()
        .any(|item| item.module_id == YANDEX && !item.available && item.reason.is_some()));

    registry.set_enabled(YANDEX, true).expect("включение");
    let list = registry
        .extension_list(PARENT_ID, POINT_ID)
        .expect("список");
    assert_eq!(list.selected.as_deref(), Some(YANDEX));

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn incompatible_contribution_is_rejected() {
    let registry = registry(None);
    add(&registry, parent_manifest());
    add(
        &registry,
        provider_manifest(YANDEX, "Яндекс", "other.contract", ">=1.0.0 <2.0.0"),
    );
    add(
        &registry,
        provider_manifest(GOOGLE, "Google", CONTRACT, ">=2.0.0"),
    );

    let list = registry
        .extension_list(PARENT_ID, POINT_ID)
        .expect("список");
    assert!(list.selected.is_none());
    for contribution in &list.contributions {
        assert!(!contribution.available);
        assert!(contribution.reason.is_some());
    }
    assert!(registry
        .select_extension(PARENT_ID, POINT_ID, YANDEX)
        .is_err());
}

#[test]
fn invoke_uses_selected_contribution_with_own_capabilities() {
    let registry = registry(None);
    add(&registry, parent_manifest());
    add(&registry, yandex());

    let mut host = TestHost::default();
    let result = registry
        .invoke_extension(
            &parent_call("call-parent"),
            &request("echo", json!({"text": "hello"})),
            None,
            &mut host,
        )
        .expect("вызов");
    assert_eq!(result.data["text"], "hello");
    assert_eq!(result.data["pointId"], POINT_ID);
    assert_eq!(result.data["callId"], format!("call-parent/{YANDEX}"));

    let settings = registry
        .invoke_extension(
            &parent_call("call-parent-2"),
            &request("settings", json!({})),
            None,
            &mut host,
        )
        .expect("настройки");
    assert_eq!(settings.data["moduleId"], YANDEX);
    assert_eq!(settings.data["capabilities"], json!(["settings.read"]));
    assert_eq!(host.calls[0], format!("{YANDEX}:settings.read"));
    assert_eq!(host.begun.len(), 2);
    assert_eq!(host.finished.len(), 2);
}

#[test]
fn missing_selection_and_unknown_point_are_machine_readable() {
    let registry = registry(None);
    add(&registry, parent_manifest());
    add(&registry, yandex());
    add(&registry, google());

    let mut host = TestHost::default();
    let error = registry
        .invoke_extension(
            &parent_call("call-none"),
            &request("echo", json!({})),
            None,
            &mut host,
        )
        .expect_err("нет выбора");
    assert_eq!(error.code, error_codes::EXTENSION_NOT_SELECTED);

    registry
        .select_extension(PARENT_ID, POINT_ID, YANDEX)
        .expect("выбор");
    let mut unknown = request("echo", json!({}));
    unknown.point_id = "missing".to_owned();
    let error = registry
        .invoke_extension(&parent_call("call-unknown"), &unknown, None, &mut host)
        .expect_err("нет точки");
    assert_eq!(error.code, error_codes::EXTENSION_NOT_FOUND);
}

#[test]
fn failed_and_timed_out_plugin_do_not_break_parent() {
    let registry = registry(None);
    add(&registry, parent_manifest());
    add(&registry, yandex());
    registry
        .select_extension(PARENT_ID, POINT_ID, YANDEX)
        .expect("выбор");

    let mut host = TestHost::default();
    let error = registry
        .invoke_extension(
            &parent_call("call-fail"),
            &request("fail", json!({})),
            None,
            &mut host,
        )
        .expect_err("отказ");
    assert_eq!(error.code, error_codes::EXTENSION_FAILED);
    assert_eq!(error.data.as_ref().unwrap()["pluginId"], YANDEX);

    let mut slow = request("sleep", json!({"ms": 400}));
    slow.timeout_ms = Some(100);
    let error = registry
        .invoke_extension(&parent_call("call-slow"), &slow, None, &mut host)
        .expect_err("таймаут");
    assert_eq!(error.code, error_codes::EXTENSION_FAILED);

    let result = registry
        .invoke_extension(
            &parent_call("call-after"),
            &request("echo", json!({"text": "ok"})),
            None,
            &mut host,
        )
        .expect("после сбоя");
    assert_eq!(result.data["text"], "ok");
}

#[test]
fn parent_cancellation_stops_child_call() {
    let registry = Arc::new(registry(None));
    add(&registry, parent_manifest());
    add(&registry, yandex());
    registry
        .select_extension(PARENT_ID, POINT_ID, YANDEX)
        .expect("выбор");

    registry.track_call("call-cancel", PARENT_ID);

    let worker = registry.clone();
    let handle = std::thread::spawn(move || {
        let mut host = TestHost::default();
        worker.invoke_extension(
            &parent_call("call-cancel"),
            &request("sleep", json!({"ms": 2_000})),
            None,
            &mut host,
        )
    });
    std::thread::sleep(std::time::Duration::from_millis(200));
    registry.cancel_call("call-cancel");
    let error = handle.join().expect("поток").expect_err("отмена");
    assert_eq!(error.code, error_codes::CANCELLED);
    registry.untrack_call("call-cancel");
}
