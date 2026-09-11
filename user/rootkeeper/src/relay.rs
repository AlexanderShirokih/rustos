//! Источники журнала записей плана: имена записей и значки их сессий журнала.

use alloc::{string::String, vec::Vec};
use core::num::NonZeroU64;

use log_abi::{LOG_SOURCE_MAX, LogBuffer};
use log_relay::SourceNames;
use userland_image::BootPlan;

/// Имена записей плана в его порядке.
pub struct ComponentNames(Vec<String>);

/// Значок сессии журнала записи плана с номером `record`.
#[must_use]
pub fn log_badge(record: u16) -> NonZeroU64 {
    NonZeroU64::MIN.saturating_add(u64::from(record))
}

impl ComponentNames {
    #[must_use]
    pub fn of(plan: &BootPlan<'_>) -> Self {
        Self(
            plan.components()
                .map(|component| String::from(component.name()))
                .collect(),
        )
    }
}

impl SourceNames for ComponentNames {
    fn name(&self, badge: u64) -> Option<LogBuffer<LOG_SOURCE_MAX>> {
        let record = usize::try_from(badge.checked_sub(1)?).ok()?;
        let name = self.0.get(record)?;
        let mut source = LogBuffer::new();
        source.push_str(name);
        Some(source)
    }
}
