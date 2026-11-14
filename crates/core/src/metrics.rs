//! Холодный отклик действий до первого кадра.
//!
//! Считаются только записи, где явно зафиксирован `firstFrameMs`. Длительность
//! всего вызова, включая облачный запрос плагина, не подменяет этот момент.
//! Сводка не содержит сообщений, текста, снимков и ключей.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use serde::Serialize;

use crate::diagnostics::LogEntry;

/// Процентили одной группы выборки.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Percentiles {
    pub sample_count: usize,
    pub p50_ms: Option<u64>,
    pub p95_ms: Option<u64>,
    pub max_ms: Option<u64>,
}

impl Percentiles {
    pub fn from_values(mut values: Vec<u64>) -> Self {
        if values.is_empty() {
            return Self::default();
        }
        values.sort_unstable();
        Self {
            sample_count: values.len(),
            p50_ms: Some(percentile(&values, 0.50)),
            p95_ms: Some(percentile(&values, 0.95)),
            max_ms: values.last().copied(),
        }
    }
}

/// Отклик одного `moduleId + actionId`, отдельно для холодных и тёплых вызовов.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionMetrics {
    pub module_id: String,
    pub action_id: String,
    pub all: Percentiles,
    pub cold: Percentiles,
    pub warm: Percentiles,
}

/// Итог разбора журнала. `total_records` и `frame_samples` показывают, была ли
/// выборка неполной: не каждый вызов доходит до первого кадра.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricsSummary {
    pub total_records: usize,
    pub malformed_records: usize,
    pub frame_samples: usize,
    pub actions: Vec<ActionMetrics>,
}

impl MetricsSummary {
    pub fn action(&self, module_id: &str, action_id: &str) -> Option<&ActionMetrics> {
        self.actions
            .iter()
            .find(|entry| entry.module_id == module_id && entry.action_id == action_id)
    }
}

/// Строит сводку по уже разобранным записям журнала.
#[derive(Default)]
struct SampleSet {
    all: Vec<u64>,
    cold: Vec<u64>,
    warm: Vec<u64>,
}

pub fn summarize<'a>(entries: impl IntoIterator<Item = &'a LogEntry>) -> MetricsSummary {
    let mut groups: BTreeMap<(String, String), SampleSet> = BTreeMap::new();
    let mut total_records = 0;
    let mut frame_samples = 0;
    for entry in entries {
        total_records += 1;
        let (Some(module_id), Some(action_id), Some(first_frame_ms)) = (
            entry.module_id.as_ref(),
            entry.action_id.as_ref(),
            entry.first_frame_ms,
        ) else {
            continue;
        };
        frame_samples += 1;
        let values = groups
            .entry((module_id.clone(), action_id.clone()))
            .or_default();
        values.all.push(first_frame_ms);
        if entry.cold.unwrap_or(false) {
            values.cold.push(first_frame_ms);
        } else {
            values.warm.push(first_frame_ms);
        }
    }
    MetricsSummary {
        total_records,
        malformed_records: 0,
        frame_samples,
        actions: groups
            .into_iter()
            .map(
                |((module_id, action_id), values)| ActionMetrics {
                    module_id,
                    action_id,
                    all: Percentiles::from_values(values.all),
                    cold: Percentiles::from_values(values.cold),
                    warm: Percentiles::from_values(values.warm),
                },
            )
            .collect(),
    }
}

/// Читает JSONL-журнал. Повреждённая строка не скрывает остальные записи и
/// увеличивает `malformedRecords`.
pub fn read_jsonl(path: &Path) -> io::Result<MetricsSummary> {
    let contents = fs::read_to_string(path)?;
    let mut entries = Vec::new();
    let mut malformed_records = 0;
    for line in contents.lines().filter(|line| !line.trim().is_empty()) {
        match serde_json::from_str::<LogEntry>(line) {
            Ok(entry) => entries.push(entry),
            Err(_) => malformed_records += 1,
        }
    }
    let mut summary = summarize(&entries);
    summary.malformed_records = malformed_records;
    Ok(summary)
}

/// Метод ближайшего ранга: p95 не требует интерполяции и воспроизводим.
fn percentile(sorted: &[u64], fraction: f64) -> u64 {
    let rank = ((sorted.len() as f64) * fraction).ceil() as usize;
    let index = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[index]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(module: &str, action: &str, first_frame_ms: Option<u64>, cold: bool) -> LogEntry {
        let mut entry = LogEntry::invocation("0.4.1", module, action, 9_000, "ok", None);
        entry.first_frame_ms = first_frame_ms;
        entry.cold = first_frame_ms.map(|_| cold);
        entry
    }

    #[test]
    fn p95_is_separate_for_module_action_and_cold_calls() {
        let entries = vec![
            entry("a", "one", Some(100), true),
            entry("a", "one", Some(200), true),
            entry("a", "one", Some(900), false),
            entry("a", "two", Some(50), true),
            entry("b", "one", Some(300), true),
        ];
        let summary = summarize(&entries);
        assert_eq!(summary.frame_samples, 5);
        let one = summary.action("a", "one").expect("действие a/one");
        assert_eq!(one.all.sample_count, 3);
        assert_eq!(one.cold.sample_count, 2);
        assert_eq!(one.cold.p95_ms, Some(200));
        assert_eq!(one.warm.p95_ms, Some(900));
        let two = summary.action("a", "two").expect("второе действие a");
        assert_eq!(two.cold.p95_ms, Some(50));
        assert_ne!(
            one.cold.p95_ms, two.cold.p95_ms,
            "действия не должны смешиваться"
        );
    }

    #[test]
    fn missing_frames_show_incomplete_sample_size() {
        let entries = vec![
            entry("a", "one", Some(100), true),
            entry("a", "one", None, true),
            entry("a", "one", None, false),
        ];
        let summary = summarize(&entries);
        assert_eq!(summary.total_records, 3);
        assert_eq!(summary.frame_samples, 1);
        assert_eq!(
            summary
                .action("a", "one")
                .expect("действие")
                .all
                .sample_count,
            1
        );
    }
}
