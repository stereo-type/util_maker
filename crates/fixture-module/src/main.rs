//! Тестовый модуль с управляемыми сбоями.
//!
//! Запускается с режимом в первом аргументе. Используется интеграционными
//! тестами ядра для проверки рукопожатия, таймаута и повреждённого протокола.
//! В поставку не входит (`publish = false`).

use std::io::{self, BufRead, Write};
use std::thread::sleep;
use std::time::Duration;

use serde_json::{json, Value};

const MODULE_ID: &str = "org.utilmaker.fixture";

fn main() {
    let spec = std::env::args().nth(1).unwrap_or_else(|| "ok".to_owned());
    let mut parts = spec.splitn(2, '@');
    let mode = parts.next().unwrap_or("ok");
    let module_id = parts.next().unwrap_or(MODULE_ID).to_owned();
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    let mut first_initialize = true;
    let mut lines = stdin.lock().lines();

    while let Some(Ok(line)) = lines.next() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();

        match method {
            "initialize" => {
                if mode == "crash-init" {
                    std::process::exit(3);
                }
                if mode == "stderr-secret-init" {
                    eprintln!("Authorization: Bearer secret-key-123");
                    std::process::exit(4);
                }
                match mode {
                    "silent" => {
                        // Процесс жив, но не подтверждает готовность.
                        continue;
                    }
                    "garbage" => {
                        writeln!(stdout, "это не JSON-RPC").unwrap();
                        stdout.flush().unwrap();
                        continue;
                    }
                    "late" if first_initialize => {
                        sleep(Duration::from_secs(4));
                    }
                    "stderr" => {
                        eprintln!("[fixture-module] диагностика в stderr до ответа");
                    }
                    _ => {}
                }
                first_initialize = false;
                if mode == "module-incompatible" {
                    write_response(
                        &mut stdout,
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": {
                                "code": 1004,
                                "message": "модуль собран для API 2.x"
                            }
                        }),
                    );
                } else {
                    let api_version = if mode == "incompatible" {
                        "2.0.0"
                    } else {
                        util_maker_sdk::HOST_API_VERSION
                    };
                    write_response(
                        &mut stdout,
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "apiVersion": api_version,
                                "moduleId": module_id.clone(),
                                "capabilities": []
                            }
                        }),
                    );
                }
            }
            "invoke" => {
                if mode == "crash" {
                    std::process::exit(2);
                }
                if mode == "rpc-error" {
                    write_response(
                        &mut stdout,
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": {
                                "code": 2001,
                                "message": "сбой перевода Authorization: Bearer secret-key-123 https://example.test/v1?text=секрет"
                            }
                        }),
                    );
                    continue;
                }
                if mode == "unexpected-secret" {
                    write_response(
                        &mut stdout,
                        json!({
                            "jsonrpc": "2.0",
                            "id": 999,
                            "result": {
                                "message": "Authorization: Bearer secret-key-123"
                            }
                        }),
                    );
                    std::process::exit(6);
                }
                if mode == "hang" {
                    sleep(Duration::from_secs(30));
                }
                if mode == "late-invoke" {
                    sleep(Duration::from_millis(200));
                }
                let call_id = request
                    .get("params")
                    .and_then(|params| params.get("callId"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if mode == "host-call"
                    || mode == "host-context"
                    || mode == "host-secret"
                    || mode == "host-hang"
                {
                    let requested_call = if mode == "host-context" {
                        "stale-call"
                    } else {
                        call_id
                    };
                    let (capability, method, params) = if mode == "host-secret" {
                        ("secret.read", "secret.read", json!({"key": "token"}))
                    } else {
                        ("screen.frame", "frame.capture", json!({}))
                    };
                    write_response(
                        &mut stdout,
                        json!({
                            "jsonrpc": "2.0",
                            "id": 9001,
                            "method": "host.call",
                            "params": {
                                "callId": requested_call,
                                "capability": capability,
                                "method": method,
                                "params": params
                            }
                        }),
                    );
                    let Some(Ok(host_line)) = lines.next() else {
                        return;
                    };
                    let host_response: Value =
                        serde_json::from_str(&host_line).unwrap_or(Value::Null);
                    if mode == "host-hang" {
                        // Кадр уже получен; держим вызов активным, пока хост
                        // не отменит его и не освободит буфер.
                        sleep(Duration::from_secs(30));
                    }
                    if let Some(error) = host_response.get("error") {
                        write_response(
                            &mut stdout,
                            json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "error": error
                            }),
                        );
                    } else {
                        write_response(
                            &mut stdout,
                            json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": {
                                    "status": "ok",
                                    "message": "fixture-module: кадр получен",
                                    "data": host_response.get("result").cloned().unwrap_or(Value::Null)
                                }
                            }),
                        );
                    }
                    continue;
                }
                write_response(
                    &mut stdout,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "status": "ok",
                            "message": format!("fixture-module: вызов {call_id} выполнен")
                        }
                    }),
                );
            }
            "shutdown" => {
                write_response(
                    &mut stdout,
                    json!({"jsonrpc": "2.0", "id": id, "result": {"stopped": true}}),
                );
                break;
            }
            other => {
                write_response(
                    &mut stdout,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": -32601, "message": format!("неизвестный метод: {other}")}
                    }),
                );
            }
        }
    }
}

fn write_response(stdout: &mut impl Write, response: Value) {
    writeln!(stdout, "{}", serde_json::to_string(&response).unwrap()).unwrap();
    stdout.flush().unwrap();
}
