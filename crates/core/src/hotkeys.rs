//! Реестр глобальных сочетаний: нормализация, системные резервы, перенос
//! владельца и сохранение назначений.
//!
//! Крейт не зависит от Win32: платформа и хранилище подключаются трейтами.
//! Это позволяет воспроизводить системный отказ и ошибку записи в тестах.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Ошибка разбора сочетания.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HotkeyError {
    #[error("сочетание не заполнено")]
    Empty,
    #[error("неизвестная клавиша: {0}")]
    UnknownKey(String),
    #[error("модификатор {0} указан дважды")]
    DuplicateModifier(String),
    #[error("в сочетании несколько клавиш: {0}")]
    MultipleKeys(String),
    #[error("слишком длинное сочетание")]
    TooLong,
}

/// Нормализованное сочетание. Порядок модификаторов при вводе не важен.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub key: String,
}

impl Hotkey {
    /// Разбирает запись вида `Ctrl+Shift+U` и нормализует клавиши.
    pub fn parse(input: &str) -> Result<Self, HotkeyError> {
        let input = input.trim();
        if input.is_empty() {
            return Err(HotkeyError::Empty);
        }
        if input.chars().count() > 64 {
            return Err(HotkeyError::TooLong);
        }
        let mut result = Self {
            ctrl: false,
            alt: false,
            shift: false,
            win: false,
            key: String::new(),
        };
        for part in input.split('+') {
            let part = part.trim();
            if part.is_empty() {
                return Err(HotkeyError::UnknownKey(String::new()));
            }
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => {
                    if result.ctrl {
                        return Err(HotkeyError::DuplicateModifier("Ctrl".to_owned()));
                    }
                    result.ctrl = true;
                }
                "alt" => {
                    if result.alt {
                        return Err(HotkeyError::DuplicateModifier("Alt".to_owned()));
                    }
                    result.alt = true;
                }
                "shift" => {
                    if result.shift {
                        return Err(HotkeyError::DuplicateModifier("Shift".to_owned()));
                    }
                    result.shift = true;
                }
                "win" | "meta" | "super" | "cmd" => {
                    if result.win {
                        return Err(HotkeyError::DuplicateModifier("Win".to_owned()));
                    }
                    result.win = true;
                }
                _ => {
                    if !result.key.is_empty() {
                        return Err(HotkeyError::MultipleKeys(input.to_owned()));
                    }
                    result.key = normalize_key(part)?;
                }
            }
        }
        if result.key.is_empty() {
            return Err(HotkeyError::UnknownKey(String::new()));
        }
        Ok(result)
    }

    /// Каноническая запись сочетания для хранения и сравнения.
    pub fn canonical(&self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("Ctrl".to_owned());
        }
        if self.alt {
            parts.push("Alt".to_owned());
        }
        if self.shift {
            parts.push("Shift".to_owned());
        }
        if self.win {
            parts.push("Win".to_owned());
        }
        parts.push(self.key.clone());
        parts.join("+")
    }
}

impl std::fmt::Display for Hotkey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.canonical())
    }
}

fn normalize_key(part: &str) -> Result<String, HotkeyError> {
    let lower = part.to_ascii_lowercase();
    let named = match lower.as_str() {
        "esc" | "escape" => "Escape",
        "del" | "delete" => "Delete",
        "enter" | "return" => "Enter",
        "space" => "Space",
        "tab" => "Tab",
        "backspace" => "Backspace",
        "up" => "ArrowUp",
        "down" => "ArrowDown",
        "left" => "ArrowLeft",
        "right" => "ArrowRight",
        "home" => "Home",
        "end" => "End",
        "pageup" | "pgup" => "PageUp",
        "pagedown" | "pgdn" => "PageDown",
        "insert" | "ins" => "Insert",
        "printscreen" | "prtsc" => "PrintScreen",
        "pause" => "Pause",
        "capslock" => "CapsLock",
        "numlock" => "NumLock",
        "scrolllock" => "ScrollLock",
        _ => {
            if lower.len() >= 2 && lower.starts_with('f') && lower[1..].parse::<u8>().is_ok() {
                let number = lower[1..].parse::<u8>().unwrap_or(0);
                if (1..=24).contains(&number) {
                    return Ok(format!("F{number}"));
                }
            }
            let mut characters = part.chars();
            if let (Some(character), None) = (characters.next(), characters.next()) {
                if character.is_ascii_alphanumeric() {
                    return Ok(character.to_ascii_uppercase().to_string());
                }
            }
            return Err(HotkeyError::UnknownKey(part.to_owned()));
        }
    };
    Ok(named.to_owned())
}

/// Известное системное сочетание и его функция.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemHotkey {
    pub combo: &'static str,
    pub function: &'static str,
}

/// Версионированный перечень системных сочетаний Windows.
pub const SYSTEM_HOTKEYS: &[SystemHotkey] = &[
    SystemHotkey {
        combo: "Win+L",
        function: "блокировка компьютера",
    },
    SystemHotkey {
        combo: "Win+D",
        function: "показ рабочего стола",
    },
    SystemHotkey {
        combo: "Win+E",
        function: "Проводник",
    },
    SystemHotkey {
        combo: "Win+I",
        function: "Параметры Windows",
    },
    SystemHotkey {
        combo: "Win+R",
        function: "Выполнить",
    },
    SystemHotkey {
        combo: "Win+S",
        function: "поиск Windows",
    },
    SystemHotkey {
        combo: "Win+Tab",
        function: "представление задач",
    },
    SystemHotkey {
        combo: "Win+V",
        function: "журнал буфера обмена",
    },
    SystemHotkey {
        combo: "Win+PrintScreen",
        function: "снимок экрана",
    },
    SystemHotkey {
        combo: "Win+Shift+S",
        function: "фрагмент экрана",
    },
    SystemHotkey {
        combo: "Win+Ctrl+D",
        function: "новый виртуальный рабочий стол",
    },
    SystemHotkey {
        combo: "Alt+Tab",
        function: "переключение между окнами",
    },
    SystemHotkey {
        combo: "Alt+F4",
        function: "закрытие активного окна",
    },
    SystemHotkey {
        combo: "Alt+PrintScreen",
        function: "снимок активного окна",
    },
    SystemHotkey {
        combo: "Ctrl+Alt+Delete",
        function: "безопасность Windows",
    },
    SystemHotkey {
        combo: "Ctrl+Shift+Escape",
        function: "Диспетчер задач",
    },
    SystemHotkey {
        combo: "Ctrl+Escape",
        function: "меню «Пуск»",
    },
];

/// Известная функция Windows или безымянный резерв для семейства Win+клавиша.
pub fn system_function(hotkey: &Hotkey) -> Option<String> {
    let canonical = hotkey.canonical();
    if let Some(entry) = SYSTEM_HOTKEYS.iter().find(|entry| entry.combo == canonical) {
        return Some(entry.function.to_owned());
    }
    if hotkey.win && !hotkey.ctrl && !hotkey.alt && !hotkey.shift {
        return Some("Зарезервировано Windows".to_owned());
    }
    None
}

/// Владелец назначения. Отключённый модуль остаётся владельцем в реестре.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owner {
    pub module_id: String,
    pub module_name: String,
    pub action_id: String,
    pub action_name: String,
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration_error: Option<String>,
}

/// Кандидат на назначение сочетания.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub combo: String,
    pub module_id: String,
    pub module_name: String,
    pub action_id: String,
    pub action_name: String,
    pub enabled: bool,
}

impl Candidate {
    pub fn new(
        combo: impl Into<String>,
        module_id: impl Into<String>,
        module_name: impl Into<String>,
        action_id: impl Into<String>,
        action_name: impl Into<String>,
        enabled: bool,
    ) -> Self {
        Self {
            combo: combo.into(),
            module_id: module_id.into(),
            module_name: module_name.into(),
            action_id: action_id.into(),
            action_name: action_name.into(),
            enabled,
        }
    }

    fn owner(&self) -> Owner {
        Owner {
            module_id: self.module_id.clone(),
            module_name: self.module_name.clone(),
            action_id: self.action_id.clone(),
            action_name: self.action_name.clone(),
            enabled: self.enabled,
            available: true,
            registration_error: None,
        }
    }
}

/// Предварительная проверка сочетания до сохранения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckResult {
    Available,
    SameAssignment,
    InternalConflict { previous: Owner, proposed: Box<Owner> },
    SystemReserved { combo: String, function: String },
    RegistrationRejected { combo: String, occupied: bool },
}

/// Подтверждение переноса, показанное пользователю на предыдущем шаге.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirmation {
    pub combo: String,
    pub expected_owner: Owner,
}

/// Результат успешного сохранения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignmentOutcome {
    pub combo: String,
    pub previous: Option<Owner>,
    pub current: Owner,
    pub transferred: bool,
}

/// Ошибка назначения, которую интерфейс показывает рядом с полем.
#[derive(Debug, thiserror::Error)]
pub enum AssignError {
    #[error(transparent)]
    Hotkey(#[from] HotkeyError),
    #[error("системное сочетание {combo}: {function}")]
    SystemReserved { combo: String, function: String },
    #[error("сочетание {combo} недоступно для глобального назначения")]
    RegistrationRejected { combo: String, occupied: bool },
    #[error("сочетание {combo} уже принадлежит «{previous}». Требуется подтверждение переноса")]
    NeedsConfirmation { combo: String, previous: String },
    #[error("условия назначения изменились, проверьте сочетание заново")]
    StaleConfirmation,
    #[error("не удалось сохранить назначения: {0}")]
    Store(String),
}

/// Ошибка хранилища.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct StoreError(pub String);

/// Хранилище назначений. Сохранение выполняется целиком, чтобы не оставить
/// частично перенесённую привязку.
pub trait HotkeyStore {
    fn load(&self) -> Result<Vec<AssignmentRecord>, StoreError>;
    fn save(&mut self, records: &[AssignmentRecord]) -> Result<(), StoreError>;
}

/// Отказ платформенной регистрации.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationError {
    /// Сочетание занято внешней программой.
    Occupied,
    /// Иная причина, включая недоступность операции.
    Unavailable(String),
}

/// Платформенный адаптер глобальных сочетаний.
pub trait HotkeyPlatform {
    fn register(&mut self, hotkey: &Hotkey) -> Result<(), RegistrationError>;
    fn unregister(&mut self, hotkey: &Hotkey);

    /// Проверка свободной комбинации без удержания регистрации.
    fn probe(&mut self, hotkey: &Hotkey) -> Result<(), RegistrationError> {
        self.register(hotkey)?;
        self.unregister(hotkey);
        Ok(())
    }
}

/// Запись реестра, которая сохраняется на диск.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssignmentRecord {
    pub combo: String,
    pub module_id: String,
    pub module_name: String,
    pub action_id: String,
    pub action_name: String,
    pub enabled: bool,
    /// Действие и модуль ещё присутствуют в активном каталоге.
    #[serde(default = "default_true")]
    pub available: bool,
    /// Причина, по которой регистрация не удалась, без пользовательских данных.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration_error: Option<String>,
}

impl AssignmentRecord {
    fn owner(&self) -> Owner {
        Owner {
            module_id: self.module_id.clone(),
            module_name: self.module_name.clone(),
            action_id: self.action_id.clone(),
            action_name: self.action_name.clone(),
            enabled: self.enabled,
            available: self.available,
            registration_error: self.registration_error.clone(),
        }
    }
}

/// Актуальное состояние действия в каталоге пакетов.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionAvailability {
    pub module_id: String,
    pub module_name: String,
    pub action_id: String,
    pub action_name: String,
    pub module_enabled: bool,
}

impl ActionAvailability {
    pub fn new(
        module_id: impl Into<String>,
        module_name: impl Into<String>,
        action_id: impl Into<String>,
        action_name: impl Into<String>,
        module_enabled: bool,
    ) -> Self {
        Self {
            module_id: module_id.into(),
            module_name: module_name.into(),
            action_id: action_id.into(),
            action_name: action_name.into(),
            module_enabled,
        }
    }
}

/// Реестр сочетаний поверх хранилища и платформенного адаптера.
pub struct HotkeyRegistry<S: HotkeyStore, P: HotkeyPlatform> {
    records: BTreeMap<String, AssignmentRecord>,
    store: S,
    platform: P,
}

impl<S: HotkeyStore, P: HotkeyPlatform> HotkeyRegistry<S, P> {
    /// Загружает целостный реестр; некорректная запись не теряется молча.
    pub fn open(store: S, platform: P) -> Result<Self, StoreError> {
        let mut records = BTreeMap::new();
        for mut record in store.load()? {
            let hotkey =
                Hotkey::parse(&record.combo).map_err(|error| StoreError(error.to_string()))?;
            let canonical = hotkey.canonical();
            record.combo = canonical.clone();
            if let Some(existing) = records.insert(canonical.clone(), record) {
                return Err(StoreError(format!(
                    "сочетание {canonical} принадлежит двум действиям: {} и {}",
                    existing.action_id, "повтор"
                )));
            }
        }
        let mut registry = Self {
            records,
            store,
            platform,
        };
        registry.restore()?;
        Ok(registry)
    }

    /// Регистрирует доступные привязки после запуска и запоминает причину
    /// отказа. Сами назначения при этом не теряются.
    pub fn restore(&mut self) -> Result<(), StoreError> {
        self.resync_platform();
        self.persist_store()
    }

    /// Обновляет сведения о действиях после установки, обновления или
    /// отключения пакета. Возвращает восстановленные и ставшие недоступными
    /// привязки.
    pub fn sync_actions(&mut self, actions: &[ActionAvailability]) -> Result<(), AssignError> {
        let previous = self.records.clone();
        let available: BTreeMap<(String, String), &ActionAvailability> = actions
            .iter()
            .map(|action| ((action.module_id.clone(), action.action_id.clone()), action))
            .collect();
        for record in self.records.values_mut() {
            match available.get(&(record.module_id.clone(), record.action_id.clone())) {
                Some(action) => {
                    record.module_name = action.module_name.clone();
                    record.action_name = action.action_name.clone();
                    record.available = true;
                    record.enabled = action.module_enabled;
                }
                None => {
                    record.available = false;
                    record.enabled = false;
                }
            }
        }
        self.resync_platform();
        if let Err(error) = self.persist() {
            self.records = previous;
            self.resync_platform();
            return Err(error);
        }
        Ok(())
    }

    /// Включает или отключает все привязки модуля, сохраняя их для возврата.
    pub fn set_module_enabled(
        &mut self,
        module_id: &str,
        enabled: bool,
    ) -> Result<(), AssignError> {
        let previous = self.records.clone();
        for record in self.records.values_mut() {
            if record.module_id == module_id {
                record.enabled = enabled;
            }
        }
        self.resync_platform();
        if let Err(error) = self.persist() {
            self.records = previous;
            self.resync_platform();
            return Err(error);
        }
        Ok(())
    }

    fn persist_store(&mut self) -> Result<(), StoreError> {
        let records: Vec<_> = self.records.values().cloned().collect();
        self.store.save(&records)
    }

    fn resync_platform(&mut self) {
        let keys: Vec<_> = self.records.keys().cloned().collect();
        for key in &keys {
            if let Ok(hotkey) = Hotkey::parse(key) {
                self.platform.unregister(&hotkey);
            }
        }
        for key in keys {
            let Some(record) = self.records.get_mut(&key) else {
                continue;
            };
            let Ok(hotkey) = Hotkey::parse(&record.combo) else {
                record.registration_error = Some("некорректное сочетание".to_owned());
                continue;
            };
            if !record.enabled || !record.available {
                record.registration_error = None;
                continue;
            }
            match self.platform.register(&hotkey) {
                Ok(()) => record.registration_error = None,
                Err(RegistrationError::Occupied) => {
                    record.registration_error = Some("сочетание занято".to_owned());
                }
                Err(RegistrationError::Unavailable(reason)) => {
                    record.registration_error = Some(reason);
                }
            }
        }
    }

    pub fn records(&self) -> impl Iterator<Item = &AssignmentRecord> {
        self.records.values()
    }

    pub fn store_mut(&mut self) -> &mut S {
        &mut self.store
    }

    /// Отдаёт хранилище и адаптер, например чтобы открыть реестр заново.
    pub fn into_parts(self) -> (S, P) {
        (self.store, self.platform)
    }

    pub fn platform_mut(&mut self) -> &mut P {
        &mut self.platform
    }

    pub fn owner(&self, combo: &str) -> Result<Option<Owner>, HotkeyError> {
        let hotkey = Hotkey::parse(combo)?;
        Ok(self
            .records
            .get(&hotkey.canonical())
            .map(AssignmentRecord::owner))
    }

    /// Предварительная проверка без изменения реестра.
    pub fn check(&mut self, candidate: &Candidate) -> Result<CheckResult, HotkeyError> {
        let hotkey = Hotkey::parse(&candidate.combo)?;
        let canonical = hotkey.canonical();
        if let Some(function) = system_function(&hotkey) {
            return Ok(CheckResult::SystemReserved {
                combo: canonical,
                function,
            });
        }
        if let Some(previous) = self.records.get(&canonical) {
            if previous.module_id == candidate.module_id
                && previous.action_id == candidate.action_id
            {
                return Ok(CheckResult::SameAssignment);
            }
            return Ok(CheckResult::InternalConflict {
                previous: previous.owner(),
                proposed: Box::new(candidate.owner()),
            });
        }
        match self.platform.probe(&hotkey) {
            Ok(()) => Ok(CheckResult::Available),
            Err(RegistrationError::Occupied) => Ok(CheckResult::RegistrationRejected {
                combo: canonical,
                occupied: true,
            }),
            Err(RegistrationError::Unavailable(_)) => Ok(CheckResult::RegistrationRejected {
                combo: canonical,
                occupied: false,
            }),
        }
    }

    /// Сохраняет назначение. Для переноса нужен `Confirmation` с прежним
    /// владельцем; устаревшее подтверждение отклоняется.
    pub fn assign(
        &mut self,
        candidate: &Candidate,
        confirmation: Option<&Confirmation>,
    ) -> Result<AssignmentOutcome, AssignError> {
        let hotkey = Hotkey::parse(&candidate.combo)?;
        let canonical = hotkey.canonical();
        let check = self.check(candidate)?;
        match check {
            CheckResult::SystemReserved { combo, function } => {
                Err(AssignError::SystemReserved { combo, function })
            }
            CheckResult::RegistrationRejected { combo, occupied } => {
                Err(AssignError::RegistrationRejected { combo, occupied })
            }
            CheckResult::SameAssignment => {
                let owner = self
                    .records
                    .get(&canonical)
                    .expect("назначение существует")
                    .owner();
                Ok(AssignmentOutcome {
                    combo: canonical,
                    previous: Some(owner.clone()),
                    current: owner,
                    transferred: false,
                })
            }
            CheckResult::Available => self.assign_new(candidate, hotkey, canonical),
            CheckResult::InternalConflict { previous, .. } => {
                self.assign_transfer(candidate, hotkey, canonical, previous, confirmation)
            }
        }
    }

    fn assign_new(
        &mut self,
        candidate: &Candidate,
        hotkey: Hotkey,
        canonical: String,
    ) -> Result<AssignmentOutcome, AssignError> {
        if candidate.enabled {
            self.platform
                .register(&hotkey)
                .map_err(|error| AssignError::RegistrationRejected {
                    combo: canonical.clone(),
                    occupied: error == RegistrationError::Occupied,
                })?;
        }
        let record = record_for(candidate, canonical.clone());
        let previous = self.records.insert(canonical.clone(), record.clone());
        if let Err(error) = self.persist() {
            self.records.remove(&canonical);
            if let Some(previous) = previous {
                self.records.insert(canonical.clone(), previous);
            }
            if candidate.enabled {
                self.platform.unregister(&hotkey);
            }
            return Err(error);
        }
        Ok(AssignmentOutcome {
            combo: canonical,
            previous: None,
            current: record.owner(),
            transferred: false,
        })
    }

    fn assign_transfer(
        &mut self,
        candidate: &Candidate,
        hotkey: Hotkey,
        canonical: String,
        previous: Owner,
        confirmation: Option<&Confirmation>,
    ) -> Result<AssignmentOutcome, AssignError> {
        let confirmation = confirmation.ok_or_else(|| AssignError::NeedsConfirmation {
            combo: canonical.clone(),
            previous: format!("{} — {}", previous.module_name, previous.action_name),
        })?;
        if confirmation.combo != canonical || confirmation.expected_owner != previous {
            return Err(AssignError::StaleConfirmation);
        }

        // Регистрация уже принадлежит ядру; меняем только маршрут. Если
        // прежний владелец был отключён или недоступен, сочетание нужно
        // зарегистрировать заново.
        let previous_active =
            previous.enabled && previous.available && previous.registration_error.is_none();
        if candidate.enabled && !previous_active {
            self.platform
                .register(&hotkey)
                .map_err(|_| AssignError::RegistrationRejected {
                    combo: canonical.clone(),
                    occupied: false,
                })?;
        }
        if !candidate.enabled && previous_active {
            self.platform.unregister(&hotkey);
        }

        let record = record_for(candidate, canonical.clone());
        self.records.insert(canonical.clone(), record.clone());
        if let Err(error) = self.persist() {
            self.records.insert(
                canonical.clone(),
                AssignmentRecord {
                    combo: canonical.clone(),
                    module_id: previous.module_id.clone(),
                    module_name: previous.module_name.clone(),
                    action_id: previous.action_id.clone(),
                    action_name: previous.action_name.clone(),
                    enabled: previous.enabled,
                    available: previous.available,
                    registration_error: previous.registration_error.clone(),
                },
            );
            if candidate.enabled && !previous_active {
                self.platform.unregister(&hotkey);
            }
            if !candidate.enabled && previous_active {
                let _ = self.platform.register(&hotkey);
            }
            return Err(error);
        }
        Ok(AssignmentOutcome {
            combo: canonical,
            previous: Some(previous),
            current: record.owner(),
            transferred: true,
        })
    }

    /// Удаляет одно сочетание действия. Остальные привязки не меняются.
    pub fn unassign(
        &mut self,
        module_id: &str,
        action_id: &str,
        combo: &str,
    ) -> Result<bool, AssignError> {
        let hotkey = Hotkey::parse(combo)?;
        let canonical = hotkey.canonical();
        let Some(record) = self.records.get(&canonical) else {
            return Ok(false);
        };
        if record.module_id != module_id || record.action_id != action_id {
            return Ok(false);
        }
        let removed = self.records.remove(&canonical).expect("запись существует");
        if removed.enabled && removed.available && removed.registration_error.is_none() {
            self.platform.unregister(&hotkey);
        }
        if let Err(error) = self.persist() {
            self.records.insert(canonical, removed);
            return Err(error);
        }
        Ok(true)
    }

    fn persist(&mut self) -> Result<(), AssignError> {
        let records: Vec<_> = self.records.values().cloned().collect();
        self.store
            .save(&records)
            .map_err(|error| AssignError::Store(error.to_string()))
    }
}

/// Файловое хранилище назначений. Поддерживает новый массив записей и
/// старый прототипный объект с одним сочетанием.
#[derive(Debug, Clone)]
pub struct JsonHotkeyStore {
    path: PathBuf,
}

impl JsonHotkeyStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read_records(&self, contents: &str) -> Result<Vec<AssignmentRecord>, StoreError> {
        if contents.trim().is_empty() {
            return Ok(Vec::new());
        }
        if let Ok(records) = serde_json::from_str::<Vec<AssignmentRecord>>(contents) {
            return Ok(records);
        }
        if let Ok(document) = serde_json::from_str::<StoredDocument>(contents) {
            let mut records = document.assignments;
            records.extend(document.hotkeys);
            if !records.is_empty() {
                return Ok(records);
            }
            if let Some(combo) = document.shortcut.or(document.hotkey).or(document.combo) {
                return Ok(vec![legacy_record(combo)]);
            }
        }
        if let Ok(bindings) = serde_json::from_str::<BTreeMap<String, LegacyBinding>>(contents) {
            if !bindings.is_empty() {
                return Ok(bindings
                    .into_iter()
                    .map(|(combo, binding)| legacy_record_with(combo, binding))
                    .collect());
            }
        }
        Err(StoreError(format!(
            "не удалось прочитать настройки сочетаний {}",
            self.path.display()
        )))
    }
}

impl HotkeyStore for JsonHotkeyStore {
    fn load(&self) -> Result<Vec<AssignmentRecord>, StoreError> {
        match fs::read_to_string(&self.path) {
            Ok(contents) => self.read_records(&contents),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(StoreError(format!(
                "чтение {}: {error}",
                self.path.display()
            ))),
        }
    }

    fn save(&mut self, records: &[AssignmentRecord]) -> Result<(), StoreError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| StoreError(format!("каталог {}: {error}", parent.display())))?;
        }
        let contents = serde_json::to_vec_pretty(records)
            .map_err(|error| StoreError(format!("сериализация назначений: {error}")))?;
        let temporary = self.path.with_extension("json.tmp");
        {
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&temporary)
                .map_err(|error| StoreError(format!("запись {}: {error}", temporary.display())))?;
            file.write_all(&contents)
                .and_then(|()| file.sync_all())
                .map_err(|error| StoreError(format!("запись {}: {error}", temporary.display())))?;
        }
        if let Err(first) = fs::rename(&temporary, &self.path) {
            // Windows не заменяет существующий файл атомарно.
            fs::remove_file(&self.path)
                .map_err(|error| StoreError(format!("замена {}: {error}", self.path.display())))?;
            fs::rename(&temporary, &self.path).map_err(|_| {
                StoreError(format!(
                    "замена {}: {first}; {}",
                    self.path.display(),
                    "повторная попытка не удалась"
                ))
            })?;
        }
        Ok(())
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredDocument {
    #[serde(default)]
    assignments: Vec<AssignmentRecord>,
    #[serde(default)]
    hotkeys: Vec<AssignmentRecord>,
    #[serde(default)]
    shortcut: Option<String>,
    #[serde(default)]
    hotkey: Option<String>,
    #[serde(default)]
    combo: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyBinding {
    #[serde(default)]
    module_id: Option<String>,
    #[serde(default)]
    module_name: Option<String>,
    #[serde(default)]
    action_id: Option<String>,
    #[serde(default)]
    action_name: Option<String>,
    #[serde(default = "default_true")]
    enabled: bool,
}

impl Default for LegacyBinding {
    fn default() -> Self {
        Self {
            module_id: None,
            module_name: None,
            action_id: None,
            action_name: None,
            enabled: true,
        }
    }
}

fn legacy_record(combo: String) -> AssignmentRecord {
    legacy_record_with(combo, LegacyBinding::default())
}

fn legacy_record_with(combo: String, binding: LegacyBinding) -> AssignmentRecord {
    AssignmentRecord {
        combo,
        module_id: binding
            .module_id
            .unwrap_or_else(|| "org.utilmaker.control".to_owned()),
        module_name: binding
            .module_name
            .unwrap_or_else(|| "Контрольный модуль".to_owned()),
        action_id: binding.action_id.unwrap_or_else(|| "ping".to_owned()),
        action_name: binding
            .action_name
            .unwrap_or_else(|| "Проверить связь".to_owned()),
        enabled: binding.enabled,
        available: true,
        registration_error: None,
    }
}

fn default_true() -> bool {
    true
}

fn record_for(candidate: &Candidate, combo: String) -> AssignmentRecord {
    AssignmentRecord {
        combo,
        module_id: candidate.module_id.clone(),
        module_name: candidate.module_name.clone(),
        action_id: candidate.action_id.clone(),
        action_name: candidate.action_name.clone(),
        enabled: candidate.enabled,
        available: true,
        registration_error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[derive(Default, Clone)]
    struct TestStore {
        records: Vec<AssignmentRecord>,
        fail_next_save: bool,
    }

    impl HotkeyStore for TestStore {
        fn load(&self) -> Result<Vec<AssignmentRecord>, StoreError> {
            Ok(self.records.clone())
        }

        fn save(&mut self, records: &[AssignmentRecord]) -> Result<(), StoreError> {
            if self.fail_next_save {
                self.fail_next_save = false;
                return Err(StoreError("диск переполнен".to_owned()));
            }
            self.records = records.to_vec();
            Ok(())
        }
    }

    #[derive(Default)]
    struct TestPlatform {
        registered: BTreeMap<String, ()>,
        occupied: BTreeSet<String>,
    }

    impl HotkeyPlatform for TestPlatform {
        fn register(&mut self, hotkey: &Hotkey) -> Result<(), RegistrationError> {
            let canonical = hotkey.canonical();
            if self.occupied.contains(&canonical) {
                return Err(RegistrationError::Occupied);
            }
            self.registered.insert(canonical, ());
            Ok(())
        }

        fn unregister(&mut self, hotkey: &Hotkey) {
            self.registered.remove(&hotkey.canonical());
        }
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

    fn registry() -> HotkeyRegistry<TestStore, TestPlatform> {
        HotkeyRegistry::open(TestStore::default(), TestPlatform::default()).expect("реестр")
    }

    fn confirm(combo: &str, owner: Owner) -> Confirmation {
        Confirmation {
            combo: Hotkey::parse(combo).expect("сочетание").canonical(),
            expected_owner: owner,
        }
    }

    #[test]
    fn modifier_order_and_aliases_are_normalized() {
        let left = Hotkey::parse("shift+control+u").expect("левое");
        let right = Hotkey::parse("Ctrl+Shift+U").expect("правое");
        assert_eq!(left.canonical(), "Ctrl+Shift+U");
        assert_eq!(left, right);
        assert_eq!(
            Hotkey::parse("ctrl+alt+del").expect("delete").canonical(),
            "Ctrl+Alt+Delete"
        );
        assert!(Hotkey::parse("Ctrl+Ctrl+U").is_err());
        assert!(Hotkey::parse("Ctrl+Shift").is_err());
    }

    #[test]
    fn system_combinations_are_named_before_registration() {
        let win_l = Hotkey::parse("Win+L").expect("Win+L");
        assert_eq!(
            system_function(&win_l).as_deref(),
            Some("блокировка компьютера")
        );
        assert_eq!(
            system_function(&Hotkey::parse("Alt+Tab").expect("Alt+Tab")).as_deref(),
            Some("переключение между окнами")
        );
        assert_eq!(
            system_function(&Hotkey::parse("Win+Q").expect("Win+Q")).as_deref(),
            Some("Зарезервировано Windows")
        );

        let mut registry = registry();
        let error = registry
            .assign(&candidate("Win+L", "a", "one"), None)
            .expect_err("системное сочетание");
        assert!(matches!(error, AssignError::SystemReserved { .. }));
        assert!(registry.records().next().is_none());
    }

    #[test]
    fn cancellation_keeps_both_assignments() {
        let mut registry = registry();
        registry
            .assign(&candidate("Ctrl+Shift+U", "a", "one"), None)
            .expect("первое назначение");
        registry
            .assign(&candidate("Ctrl+Alt+U", "a", "one"), None)
            .expect("второе назначение");

        let conflict = registry
            .check(&candidate("Ctrl+Shift+U", "b", "two"))
            .expect("проверка");
        let CheckResult::InternalConflict { previous, proposed } = conflict else {
            panic!("ожидался внутренний конфликт: {conflict:?}");
        };
        assert_eq!(previous.module_id, "a");
        assert_eq!(previous.module_name, "Модуль a");
        assert_eq!(previous.action_id, "one");
        assert_eq!(proposed.module_id, "b");
        assert_eq!(proposed.action_name, "Действие two");

        // Отмена — это отсутствие assign: обе привязки владельца остаются.
        assert_eq!(
            registry
                .owner("Ctrl+Shift+U")
                .expect("owner")
                .unwrap()
                .module_id,
            "a"
        );
        assert_eq!(
            registry
                .owner("Ctrl+Alt+U")
                .expect("owner")
                .unwrap()
                .module_id,
            "a"
        );
    }

    #[test]
    fn transfer_requires_confirmation_and_keeps_other_bindings() {
        let store = TestStore::default();
        let mut registry =
            HotkeyRegistry::open(store.clone(), TestPlatform::default()).expect("реестр");
        registry
            .assign(&candidate("Ctrl+Shift+U", "a", "one"), None)
            .expect("первое");
        registry
            .assign(&candidate("Ctrl+Alt+U", "a", "one"), None)
            .expect("второе");

        let error = registry
            .assign(&candidate("Ctrl+Shift+U", "b", "two"), None)
            .expect_err("без подтверждения");
        assert!(matches!(error, AssignError::NeedsConfirmation { .. }));
        assert_eq!(
            registry
                .owner("Ctrl+Shift+U")
                .expect("owner")
                .unwrap()
                .module_id,
            "a"
        );

        let previous = registry.owner("Ctrl+Shift+U").expect("owner").unwrap();
        let outcome = registry
            .assign(
                &candidate("Ctrl+Shift+U", "b", "two"),
                Some(&confirm("Ctrl+Shift+U", previous)),
            )
            .expect("перенос");
        assert!(outcome.transferred);
        assert_eq!(
            registry
                .owner("Ctrl+Shift+U")
                .expect("owner")
                .unwrap()
                .module_id,
            "b"
        );
        assert_eq!(
            registry
                .owner("Ctrl+Alt+U")
                .expect("owner")
                .unwrap()
                .module_id,
            "a"
        );

        // Новый процесс видит то же состояние из сохранённого хранилища.
        let (store, _platform) = registry.into_parts();
        let reopened =
            HotkeyRegistry::open(store, TestPlatform::default()).expect("повторное открытие");
        assert_eq!(
            reopened
                .owner("Ctrl+Shift+U")
                .expect("owner")
                .unwrap()
                .action_id,
            "two"
        );
        assert_eq!(
            reopened
                .owner("Ctrl+Alt+U")
                .expect("owner")
                .unwrap()
                .action_id,
            "one"
        );
    }

    #[test]
    fn stale_confirmation_is_rejected() {
        let mut registry = registry();
        registry
            .assign(&candidate("Ctrl+Shift+U", "a", "one"), None)
            .expect("назначение");
        let stale = registry.owner("Ctrl+Shift+U").expect("owner").unwrap();
        registry
            .assign(
                &candidate("Ctrl+Shift+U", "b", "two"),
                Some(&confirm("Ctrl+Shift+U", stale.clone())),
            )
            .expect("перенос");

        let error = registry
            .assign(
                &candidate("Ctrl+Shift+U", "c", "three"),
                Some(&confirm("Ctrl+Shift+U", stale)),
            )
            .expect_err("подтверждение устарело");
        assert!(matches!(error, AssignError::StaleConfirmation));
        assert_eq!(
            registry
                .owner("Ctrl+Shift+U")
                .expect("owner")
                .unwrap()
                .module_id,
            "b"
        );
    }

    #[test]
    fn system_reserved_forbids_transfer_even_with_confirmation() {
        let mut registry = registry();
        registry
            .assign(&candidate("Ctrl+Shift+U", "a", "one"), None)
            .expect("назначение");
        let previous = registry.owner("Ctrl+Shift+U").expect("owner").unwrap();
        let error = registry
            .assign(
                &candidate("Win+L", "b", "two"),
                Some(&confirm("Win+L", previous)),
            )
            .expect_err("системное сочетание");
        assert!(matches!(error, AssignError::SystemReserved { .. }));
    }

    #[test]
    fn external_occupancy_blocks_then_allows_assignment() {
        let mut registry = registry();
        registry
            .platform_mut()
            .occupied
            .insert("Ctrl+Shift+U".to_owned());
        let candidate = candidate("Ctrl+Shift+U", "a", "one");
        assert!(matches!(
            registry.check(&candidate).expect("проверка"),
            CheckResult::RegistrationRejected { occupied: true, .. }
        ));
        assert!(matches!(
            registry.assign(&candidate, None),
            Err(AssignError::RegistrationRejected { occupied: true, .. })
        ));

        registry.platform_mut().occupied.clear();
        registry
            .assign(&candidate, None)
            .expect("после освобождения");
        assert!(registry.owner("Ctrl+Shift+U").expect("owner").is_some());
    }

    #[test]
    fn store_failure_keeps_previous_assignment() {
        let mut registry = registry();
        registry
            .assign(&candidate("Ctrl+Shift+U", "a", "one"), None)
            .expect("назначение");
        registry.store_mut().fail_next_save = true;
        let previous = registry.owner("Ctrl+Shift+U").expect("owner").unwrap();
        let error = registry
            .assign(
                &candidate("Ctrl+Shift+U", "b", "two"),
                Some(&confirm("Ctrl+Shift+U", previous)),
            )
            .expect_err("ошибка записи");
        assert!(matches!(error, AssignError::Store(_)));
        assert_eq!(
            registry
                .owner("Ctrl+Shift+U")
                .expect("owner")
                .unwrap()
                .module_id,
            "a"
        );

        registry.store_mut().fail_next_save = true;
        let error = registry
            .assign(&candidate("Ctrl+Alt+U", "b", "two"), None)
            .expect_err("ошибка записи новой привязки");
        assert!(matches!(error, AssignError::Store(_)));
        assert!(registry.owner("Ctrl+Alt+U").expect("owner").is_none());
    }

    #[test]
    fn disabled_owner_is_still_a_conflict() {
        let mut registry = registry();
        let mut disabled = candidate("Ctrl+Shift+U", "a", "one");
        disabled.enabled = false;
        registry
            .assign(&disabled, None)
            .expect("отключённый модуль");
        let conflict = registry
            .check(&candidate("Ctrl+Shift+U", "b", "two"))
            .expect("проверка");
        let CheckResult::InternalConflict { previous, .. } = conflict else {
            panic!("ожидался конфликт");
        };
        assert!(!previous.enabled);
        assert_eq!(
            registry
                .owner("Ctrl+Shift+U")
                .expect("owner")
                .unwrap()
                .module_id,
            "a"
        );

        let outcome = registry
            .assign(
                &candidate("Ctrl+Shift+U", "b", "two"),
                Some(&confirm("Ctrl+Shift+U", previous)),
            )
            .expect("перенос с отключённого модуля");
        assert!(outcome.transferred);
        assert!(registry
            .platform_mut()
            .registered
            .contains_key("Ctrl+Shift+U"));
    }

    #[test]
    fn unassign_removes_only_requested_binding() {
        let mut registry = registry();
        registry
            .assign(&candidate("Ctrl+Shift+U", "a", "one"), None)
            .expect("первое");
        registry
            .assign(&candidate("Ctrl+Alt+U", "a", "one"), None)
            .expect("второе");
        assert!(registry
            .unassign("a", "one", "Ctrl+Shift+U")
            .expect("удаление"));
        assert!(registry.owner("Ctrl+Shift+U").expect("owner").is_none());
        assert!(registry.owner("Ctrl+Alt+U").expect("owner").is_some());
    }
}
