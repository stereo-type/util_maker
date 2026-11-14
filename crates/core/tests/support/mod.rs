//! Поиск собранных бинарников workspace для интеграционных тестов.
//!
//! Раскладка Cargo отличается между версиями, поэтому бинарник ищется по всему
//! каталогу `target`, а выбирается самый свежий экземпляр.

use std::fs;
use std::path::{Path, PathBuf};

pub fn binary(name: &str) -> PathBuf {
    let variable = format!(
        "UTIL_MAKER_{}",
        name.to_uppercase().replace(['-', '.'], "_")
    );
    if let Some(path) = std::env::var_os(variable) {
        return PathBuf::from(path);
    }
    let executable = std::env::current_exe().expect("путь к тестовому исполняемому файлу");
    let target_root = executable
        .ancestors()
        .find(|path| path.file_name().is_some_and(|value| value == "target"))
        .map(PathBuf::from)
        .expect("каталог target в пути тестового файла");
    let file_name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    let mut found = Vec::new();
    collect_binaries(&target_root, &file_name, 0, &mut found);
    found
        .into_iter()
        .max_by_key(|path| fs::metadata(path).and_then(|meta| meta.modified()).ok())
        .unwrap_or_else(|| target_root.join(file_name))
}

fn collect_binaries(directory: &Path, name: &str, depth: usize, found: &mut Vec<PathBuf>) {
    if depth > 8 {
        return;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if entry.file_name().to_string_lossy() == "incremental" {
                continue;
            }
            collect_binaries(&entry.path(), name, depth + 1, found);
        } else if file_type.is_file() && entry.file_name().to_string_lossy() == name {
            found.push(entry.path());
        }
    }
}
