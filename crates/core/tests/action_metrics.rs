//! Разбор журнала холодного отклика: отдельные p95, размер выборки и секреты.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use util_maker_core::metrics::read_jsonl;

fn temp_file() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let directory = std::env::temp_dir().join(format!(
        "util-maker-metrics-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("каталог");
    directory.join("util-maker.jsonl")
}

#[test]
fn summary_separates_actions_and_reports_incomplete_samples() {
    let path = temp_file();
    let contents = [
        r#"{"timeMs":1,"appVersion":"0.4.1","moduleId":"a","actionId":"one","durationMs":9000,"outcome":"ok","firstFrameMs":120,"cold":true}"#,
        r#"{"timeMs":2,"appVersion":"0.4.1","moduleId":"a","actionId":"one","durationMs":9000,"outcome":"ok","firstFrameMs":240,"cold":true}"#,
        r#"{"timeMs":3,"appVersion":"0.4.1","moduleId":"a","actionId":"one","durationMs":9000,"outcome":"ok","firstFrameMs":600,"cold":false}"#,
        r#"{"timeMs":4,"appVersion":"0.4.1","moduleId":"b","actionId":"two","durationMs":9000,"outcome":"ok","firstFrameMs":90,"cold":true}"#,
        r#"{"timeMs":5,"appVersion":"0.4.1","moduleId":"a","actionId":"one","durationMs":9000,"outcome":"error","message":"Authorization: Bearer secret-key-123"}"#,
        "не JSON",
    ]
    .join("\n");
    std::fs::write(&path, contents).expect("журнал");

    let summary = read_jsonl(&path).expect("разбор");
    assert_eq!(summary.total_records, 5);
    assert_eq!(summary.malformed_records, 1);
    assert_eq!(summary.frame_samples, 4);
    let one = summary.action("a", "one").expect("a/one");
    assert_eq!(one.all.sample_count, 3);
    assert_eq!(one.cold.sample_count, 2);
    assert_eq!(one.cold.p95_ms, Some(240));
    assert_eq!(one.warm.p95_ms, Some(600));
    let two = summary.action("b", "two").expect("b/two");
    assert_eq!(two.cold.p95_ms, Some(90));
    assert_ne!(
        one.cold.p95_ms, two.cold.p95_ms,
        "p95 разных действий не смешивается"
    );

    let serialized = serde_json::to_string(&summary).expect("JSON");
    assert!(!serialized.contains("secret-key-123"));
    let _ = std::fs::remove_dir_all(path.parent().expect("каталог"));
}

#[test]
fn completion_without_first_frame_is_not_counted() {
    let path = temp_file();
    std::fs::write(
        &path,
        r#"{"timeMs":1,"appVersion":"0.4.1","moduleId":"a","actionId":"one","durationMs":5000,"outcome":"ok"}
{"timeMs":2,"appVersion":"0.4.1","moduleId":"a","actionId":"one","durationMs":5000,"outcome":"ok"}"#,
    )
    .expect("журнал");
    let summary = read_jsonl(&path).expect("разбор");
    assert_eq!(summary.total_records, 2);
    assert_eq!(summary.frame_samples, 0);
    assert!(summary.actions.is_empty());
    let _ = std::fs::remove_dir_all(path.parent().expect("каталог"));
}
