//! Заглушка платформенного адаптера для Linux и macOS.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use util_maker_core::hotkeys::Hotkey;

use crate::{dormant_events, HotkeyEvent, RegisterFailure};

pub struct Service {
    events: Receiver<HotkeyEvent>,
}

impl Service {
    pub fn start() -> Result<Self, String> {
        Ok(Self {
            events: dormant_events(),
        })
    }

    pub fn register(&self, _hotkey: &Hotkey, _id: u32) -> Result<(), RegisterFailure> {
        Err(RegisterFailure::Unavailable(
            "глобальная регистрация поддерживается только в Windows".to_owned(),
        ))
    }

    pub fn unregister(&self, _id: u32) {}

    pub fn recv_timeout(&self, timeout: Duration) -> Result<HotkeyEvent, RecvTimeoutError> {
        self.events.recv_timeout(timeout)
    }
}
