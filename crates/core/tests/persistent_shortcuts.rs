//! Постоянный реестр сочетаний: старый формат, отключение и возврат действия.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use util_maker_core::hotkeys::{
    ActionAvailability, Candidate, Hotkey, HotkeyPlatform, HotkeyRegistry, JsonHotkeyStore,
    RegistrationError,
};

#[derive(Default)]
struct TestPlatform {
    registered: BTreeSet<String>,
    occupied: BTreeSet<String>,
    fail: bool,
}

impl HotkeyPlatform for TestPlatform {
    fn register(&mut self, hotkey: &Hotkey) -> Result<(), RegistrationError> {
        if self.fail || self.occupied.contains(&hotkey.canonical()) {
            return Err(RegistrationError::Occupied);
        }
        self.registered.insert(hotkey.canonical());
        Ok(())
    }

    fn unregister(&mut self, hotkey: &Hotkey) {
        self.registered.remove(&hotkey.canonical());
    }
}

fn temp_file() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let directory = std::env::temp_dir().join(format!(
        "util-maker-hotkeys-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("каталог");
    directory.join("settings.json")
}

fn candidate(combo: &str, module: &str, action: &str) -> Candidate {
    Candidate::new(
        combo,
        module,
        format!("Модуль {module}"),
        action,
        format!("Действие {action}"),
        true,
    )
}

#[test]
fn legacy_shortcut_is_saved_and_restored_with_extra_bindings() {
    let path = temp_file();
    std::fs::write(&path, r#"{"shortcut":"Ctrl+Shift+U"}"#).expect("старый формат");
    let mut registry =
        HotkeyRegistry::open(JsonHotkeyStore::new(&path), TestPlatform::default()).expect("реестр");
    let legacy = registry
        .owner("Ctrl+Shift+U")
        .expect("разбор")
        .expect("старая привязка");
    assert_eq!(legacy.module_id, "org.utilmaker.control");
    assert_eq!(legacy.action_id, "ping");
    assert!(registry.platform_mut().registered.contains("Ctrl+Shift+U"));

    registry
        .assign(
            &candidate("Ctrl+Alt+U", "org.utilmaker.control", "ping"),
            None,
        )
        .expect("второе сочетание");
    let (store, platform) = registry.into_parts();
    let reopened = HotkeyRegistry::open(store, platform).expect("повторное открытие");
    let records: Vec<_> = reopened.records().collect();
    assert_eq!(records.len(), 2);
    let combos: BTreeSet<_> = records.iter().map(|record| record.combo.as_str()).collect();
    assert!(combos.contains("Ctrl+Shift+U"));
    assert!(combos.contains("Ctrl+Alt+U"));
    let contents = std::fs::read_to_string(&path).expect("файл");
    assert!(
        contents.starts_with('['),
        "старый формат должен быть перезаписан"
    );
    let _ = std::fs::remove_dir_all(path.parent().expect("каталог"));
}

#[test]
fn disabling_and_removing_action_keep_binding_for_return() {
    let path = temp_file();
    let mut registry =
        HotkeyRegistry::open(JsonHotkeyStore::new(&path), TestPlatform::default()).expect("реестр");
    registry
        .assign(&candidate("Ctrl+Shift+U", "a", "one"), None)
        .expect("привязка");
    let availability = [
        ActionAvailability::new("a", "Модуль a", "one", "Первое", true),
        ActionAvailability::new("b", "Модуль b", "two", "Второе", true),
    ];
    registry.sync_actions(&availability).expect("синхронизация");
    assert!(registry.platform_mut().registered.contains("Ctrl+Shift+U"));

    registry.set_module_enabled("a", false).expect("отключение");
    assert!(!registry.platform_mut().registered.contains("Ctrl+Shift+U"));
    let owner = registry
        .owner("Ctrl+Shift+U")
        .expect("разбор")
        .expect("привязка сохранена");
    assert!(!owner.enabled);
    assert!(owner.available);

    registry.set_module_enabled("a", true).expect("включение");
    assert!(registry.platform_mut().registered.contains("Ctrl+Shift+U"));

    let without_a = [ActionAvailability::new(
        "b",
        "Модуль b",
        "two",
        "Второе",
        true,
    )];
    registry
        .sync_actions(&without_a)
        .expect("удаление действия");
    let owner = registry
        .owner("Ctrl+Shift+U")
        .expect("разбор")
        .expect("недоступная привязка");
    assert!(!owner.available);
    assert!(!registry.platform_mut().registered.contains("Ctrl+Shift+U"));

    registry
        .sync_actions(&availability)
        .expect("возврат действия");
    let owner = registry
        .owner("Ctrl+Shift+U")
        .expect("разбор")
        .expect("привязка вернулась");
    assert!(owner.available);
    assert!(registry.platform_mut().registered.contains("Ctrl+Shift+U"));
    let _ = std::fs::remove_dir_all(path.parent().expect("каталог"));
}

#[test]
fn registration_failure_is_visible_and_retryable() {
    let path = temp_file();
    std::fs::write(&path, r#"{"shortcut":"Ctrl+Shift+U"}"#).expect("старый формат");
    let mut platform = TestPlatform::default();
    platform.occupied.insert("Ctrl+Shift+U".to_owned());
    let mut registry = HotkeyRegistry::open(JsonHotkeyStore::new(&path), platform).expect("реестр");
    let owner = registry
        .owner("Ctrl+Shift+U")
        .expect("разбор")
        .expect("запись");
    assert_eq!(
        owner.registration_error.as_deref(),
        Some("сочетание занято")
    );
    registry.platform_mut().occupied.clear();
    registry.restore().expect("повторная регистрация");
    let owner = registry
        .owner("Ctrl+Shift+U")
        .expect("разбор")
        .expect("запись");
    assert!(owner.registration_error.is_none());
    assert!(registry.platform_mut().registered.contains("Ctrl+Shift+U"));
    let _ = std::fs::remove_dir_all(path.parent().expect("каталог"));
}

#[test]
fn recommendation_does_not_take_existing_binding() {
    let path = temp_file();
    let mut registry =
        HotkeyRegistry::open(JsonHotkeyStore::new(&path), TestPlatform::default()).expect("реестр");
    registry
        .assign(
            &candidate("Ctrl+Shift+U", "org.utilmaker.control", "ping"),
            None,
        )
        .expect("контрольный модуль");
    let result = registry
        .assign(&candidate("Ctrl+Shift+U", "org.example.new", "run"), None)
        .expect_err("конфликт");
    assert!(matches!(
        result,
        util_maker_core::hotkeys::AssignError::NeedsConfirmation { .. }
    ));
    assert_eq!(
        registry
            .owner("Ctrl+Shift+U")
            .expect("разбор")
            .expect("владелец")
            .module_id,
        "org.utilmaker.control"
    );
    assert_eq!(
        registry
            .records()
            .map(|record| record.combo.clone())
            .collect::<Vec<_>>(),
        vec!["Ctrl+Shift+U".to_owned()]
    );
    let _ = std::fs::remove_dir_all(path.parent().expect("каталог"));
}
