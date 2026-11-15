//! Общий порядок микроприложений для настроек и меню трея.

use std::path::PathBuf;

use util_maker_core::{Manifest, ModuleEntry, Registry};
use util_maker_sdk::Action;

fn manifest(id: &str, name: &str, actions: &[(&str, &str)]) -> Manifest {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/control/manifest.json");
    let mut manifest = Manifest::load(&path).expect("манифест");
    manifest.id = id.to_owned();
    manifest.name = name.to_owned();
    manifest.actions = actions
        .iter()
        .map(|(id, name)| Action {
            id: (*id).to_owned(),
            name: (*name).to_owned(),
            description: None,
            recommended_hotkeys: Vec::new(),
        })
        .collect();
    manifest
}

fn entry(id: &str, name: &str, actions: &[(&str, &str)]) -> ModuleEntry {
    ModuleEntry::new(
        manifest(id, name, actions),
        PathBuf::from("fixture-module"),
        true,
    )
}

#[test]
fn modules_are_sorted_by_russian_name_case_insensitively_then_by_id() {
    let registry = Registry::new();
    registry
        .replace_packages(vec![
            entry("org.test.yabloko", "Яблоко", &[("run", "Запустить")]),
            entry("org.test.el", "Ель", &[("run", "Запустить")]),
            entry("org.test.elka", "ёлка", &[("run", "Запустить")]),
            entry("org.test.zhuk", "Жук", &[("run", "Запустить")]),
            entry("org.test.dup-b", "Дуб", &[("run", "Запустить")]),
            entry("org.test.dup-a", "дуб", &[("run", "Запустить")]),
            entry("org.test.beta", "Бета", &[("run", "Запустить")]),
            entry("org.test.alpha", "альфа", &[("run", "Запустить")]),
        ])
        .expect("каталог");

    let names: Vec<_> = registry
        .modules()
        .into_iter()
        .map(|entry| entry.manifest.name)
        .collect();
    assert_eq!(
        names,
        vec![
            "альфа",
            "Бета",
            "дуб",
            "Дуб",
            "Ель",
            "ёлка",
            "Жук",
            "Яблоко"
        ]
    );
}

#[test]
fn actions_follow_module_order_and_keep_manifest_order_inside_module() {
    let registry = Registry::new();
    registry
        .replace_packages(vec![
            entry(
                "org.test.beta",
                "Бета",
                &[("first", "Первый"), ("second", "Второй")],
            ),
            entry(
                "org.test.alpha",
                "Альфа",
                &[("second", "Второй"), ("first", "Первый")],
            ),
        ])
        .expect("каталог");

    let actions: Vec<_> = registry
        .actions()
        .into_iter()
        .map(|action| (action.module_name, action.action_name))
        .collect();
    assert_eq!(
        actions,
        vec![
            ("Альфа".to_owned(), "Второй".to_owned()),
            ("Альфа".to_owned(), "Первый".to_owned()),
            ("Бета".to_owned(), "Первый".to_owned()),
            ("Бета".to_owned(), "Второй".to_owned()),
        ]
    );
}

#[test]
fn disabling_a_module_keeps_its_settings_position_and_removes_only_its_actions() {
    let registry = Registry::new();
    registry
        .replace_packages(vec![
            entry("org.test.alpha", "Альфа", &[("run", "Запустить")]),
            entry("org.test.beta", "Бета", &[("run", "Запустить")]),
        ])
        .expect("каталог");

    registry
        .set_enabled("org.test.beta", false)
        .expect("отключение");

    let modules: Vec<_> = registry
        .modules()
        .into_iter()
        .map(|entry| entry.manifest.name)
        .collect();
    assert_eq!(modules, vec!["Альфа", "Бета"]);
    let actions: Vec<_> = registry
        .actions()
        .into_iter()
        .map(|action| action.module_name)
        .collect();
    assert_eq!(actions, vec!["Альфа"]);

    registry
        .set_enabled("org.test.beta", true)
        .expect("включение");
    let modules: Vec<_> = registry
        .modules()
        .into_iter()
        .map(|entry| entry.manifest.name)
        .collect();
    assert_eq!(modules, vec!["Альфа", "Бета"]);
}

#[test]
fn current_package_set_has_same_alphabetical_order_in_settings_and_tray() {
    fn packaged(relative: &str) -> ModuleEntry {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(relative);
        ModuleEntry::new(
            Manifest::load(&path).expect("манифест поставки"),
            PathBuf::from("fixture-module"),
            true,
        )
    }

    let entries = vec![
        packaged("plugins/screenshot/manifest.json"),
        packaged("plugins/control/manifest.json"),
        packaged("plugins/color-picker/manifest.json"),
        packaged("plugins/translator/manifest.json"),
    ];
    let registry = Registry::new();
    registry
        .replace_packages(entries.clone())
        .expect("первичный каталог");

    let expected = [
        "Контрольный модуль",
        "Переводчик",
        "Пипетка цвета",
        "Скриншоты",
    ];
    let names: Vec<_> = registry
        .modules()
        .into_iter()
        .map(|entry| entry.manifest.name)
        .collect();
    assert_eq!(names, expected);

    let mut action_modules = Vec::new();
    for action in registry.actions() {
        if action_modules.last() != Some(&action.module_name) {
            action_modules.push(action.module_name);
        }
    }
    assert_eq!(action_modules, expected);

    registry
        .replace_packages(entries.iter().cloned().rev().collect())
        .expect("обновление каталога");
    let names_after_update: Vec<_> = registry
        .modules()
        .into_iter()
        .map(|entry| entry.manifest.name)
        .collect();
    assert_eq!(names_after_update, expected);

    let reopened = Registry::new();
    reopened
        .replace_packages(entries)
        .expect("каталог после перезапуска");
    let names_after_reopen: Vec<_> = reopened
        .modules()
        .into_iter()
        .map(|entry| entry.manifest.name)
        .collect();
    assert_eq!(names_after_reopen, expected);
}
