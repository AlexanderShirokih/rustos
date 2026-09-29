//! Имена источников журнала зарегистрированных компонентов.

use alloc::{string::String, sync::Arc, vec::Vec};
use core::num::NonZeroU64;

use log_abi::{LOG_SOURCE_MAX, LogBuffer};
use log_relay::SourceNames;
use runtime::Mutex;
use userland_image::BootPlan;

/// Общий список имён источников. Relay использует его при обработке сообщения.
#[derive(Clone)]
pub struct ComponentNames(Arc<Mutex<Vec<String>>>);

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

    pub fn add(&self, name: &str) -> Option<NonZeroU64> {
        let mut names = self.0.lock();
        let record = u16::try_from(names.len()).ok()?;
        names.push(String::from(name));
        Some(log_badge(record))
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
