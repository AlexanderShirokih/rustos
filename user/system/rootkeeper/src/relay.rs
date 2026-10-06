//! Имена источников журнала зарегистрированных компонентов.

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use core::num::NonZeroU64;

use kernel_objects::Session;
use log_abi::{LOG_SOURCE_MAX, LogBuffer, log};
use log_relay::{LogRelayThread, SourceNames};
use runtime::Mutex;
use userland_image::BootPlan;

/// Общий список имён источников. Relay использует его при обработке сообщения.
#[derive(Clone)]
pub struct ComponentNames(Arc<Mutex<Vec<String>>>);

/// Источник сессий журнала записей. Имена источников общие с relay, который принимает
/// строки сессий.
pub struct LogSources {
    names: ComponentNames,
    sessions: Box<dyn LogSessions>,
}

/// Выдача сессий журнала по значку источника.
pub trait LogSessions: Send {
    /// Сессия журнала со значком `badge`. `None`, если relay сессию не выдал.
    fn session(&self, badge: NonZeroU64) -> Option<Session<log::Protocol>>;
}

/// Значок сессии журнала записи с номером `record`.
#[must_use]
pub fn log_badge(record: u16) -> NonZeroU64 {
    NonZeroU64::MIN.saturating_add(u64::from(record))
}

impl ComponentNames {
    #[must_use]
    pub fn of(plan: &BootPlan<'_>) -> Self {
        Self(Arc::new(Mutex::new(
            plan.components()
                .map(|component| String::from(component.name()))
                .collect(),
        )))
    }

    /// Добавляет имя источника следующей записи и возвращает значок её сессии журнала. `None`,
    /// если имя не помещается в источник журнала либо номера записей исчерпаны.
    pub fn add(&self, name: &str) -> Option<NonZeroU64> {
        if name.len() > LOG_SOURCE_MAX {
            return None;
        }
        let mut names = self.0.lock();
        let record = u16::try_from(names.len()).ok()?;
        names.push(String::from(name));
        Some(log_badge(record))
    }

    fn retract(&self, badge: NonZeroU64) {
        let mut names = self.0.lock();
        if u64::try_from(names.len()).is_ok_and(|count| count == badge.get()) {
            names.pop();
        }
    }
}

impl LogSources {
    /// Источник с именами `names`, сессию со значком выдаёт `sessions`.
    pub fn new(names: ComponentNames, sessions: impl LogSessions + 'static) -> Self {
        Self {
            names,
            sessions: Box::new(sessions),
        }
    }

    pub(crate) fn session(&self, record: u16) -> Option<Session<log::Protocol>> {
        self.sessions.session(log_badge(record))
    }

    pub(crate) fn register(&self, record: u16, name: &str) -> Option<Session<log::Protocol>> {
        let badge = self.names.add(name)?;
        let session = (badge == log_badge(record))
            .then(|| self.session(record))
            .flatten();
        if session.is_none() {
            self.names.retract(badge);
        }
        session
    }

    pub(crate) fn retract(&self, record: u16) {
        self.names.retract(log_badge(record));
    }
}

impl LogSessions for LogRelayThread {
    fn session(&self, badge: NonZeroU64) -> Option<Session<log::Protocol>> {
        Self::session(self, badge).ok()
    }
}

impl SourceNames for ComponentNames {
    fn name(&self, badge: u64) -> Option<LogBuffer<LOG_SOURCE_MAX>> {
        let record = usize::try_from(badge.checked_sub(1)?).ok()?;
        let names = self.0.lock();
        let name = names.get(record)?;
        let mut source = LogBuffer::new();
        source.push_str(name);
        Some(source)
    }
}
