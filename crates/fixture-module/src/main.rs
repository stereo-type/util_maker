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
    let mode = std::env::args().nth(1).unwrap_or_else(|| "ok".to_owned());
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    let mut first_initialize = true;

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
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
                match mode.as_str() {
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
                        "1.0.0"
                    };
                    write_response(
                        &mut stdout,
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "apiVersion": api_version,
                                "moduleId": MODULE_ID,
                                "capabilities": []
                            }
                        }),
                    );
                }
            }
            "invoke" => {
                write_response(
                    &mut stdout,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "status": "ok",
                            "message": "fixture-module: вызов выполнен"
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
