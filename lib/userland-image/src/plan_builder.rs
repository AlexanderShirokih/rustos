use alloc::vec::Vec;

use syscall::Timeout;

use crate::plan::{
    BOOT_PLAN_MAGIC, BOOT_PLAN_VERSION, PLAN_HEADER_SIZE, PlanDevice, SLOT_KIND_IMAGE,
    SLOT_KIND_IRQ, SLOT_KIND_MMIO, SLOT_KIND_SERVICE, SlotSource,
};

/// Компонент композиции, укладываемый в план.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentSpec<'a> {
    pub id: &'a str,
    pub program: &'a str,
    pub config: Option<&'a str>,
    pub slots: &'a [SlotSource<'a>],
    pub requires: &'a [u16],
    pub after: &'a [u16],
    pub budget_pages: u64,
    pub startup_timeout: Timeout,
    pub restart_limit: u32,
    pub critical: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanEncodeError {
    EmptyComposition,
    EmptyString,
    StringTooLong,
    TooManyEntries,
    IndexOutOfRange,
    ZeroBudget,
    ZeroRestartLimit,
    UnboundedStartupTimeout,
}

/// Кодирует план загрузочной композиции.
pub fn encode_plan(components: &[ComponentSpec<'_>]) -> Result<Vec<u8>, PlanEncodeError> {
    let count = u16::try_from(components.len()).map_err(|_| PlanEncodeError::TooManyEntries)?;
    if count == 0 {
        return Err(PlanEncodeError::EmptyComposition);
    }

    let mut bytes = Vec::with_capacity(PLAN_HEADER_SIZE);
    bytes.extend_from_slice(&BOOT_PLAN_MAGIC);
    bytes.extend_from_slice(&BOOT_PLAN_VERSION.to_le_bytes());
    bytes.extend_from_slice(&count.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());

    for component in components {
        encode_component(&mut bytes, component, count)?;
    }
    Ok(bytes)
}

fn encode_component(
    bytes: &mut Vec<u8>,
    component: &ComponentSpec<'_>,
    count: u16,
) -> Result<(), PlanEncodeError> {
    if component.budget_pages == 0 {
        return Err(PlanEncodeError::ZeroBudget);
    }
    if component.restart_limit == 0 {
        return Err(PlanEncodeError::ZeroRestartLimit);
    }
    if component.startup_timeout == Timeout::POLL || component.startup_timeout == Timeout::INFINITE
    {
        return Err(PlanEncodeError::UnboundedStartupTimeout);
    }
    let id_len = string_len(component.id)?;
    let program_len = string_len(component.program)?;
    let config_len = match component.config {
        Some(config) => string_len(config)?,
        None => 0,
    };
    let slot_count = entry_count(component.slots.len())?;
    let requires_count = entry_count(component.requires.len())?;
    let after_count = entry_count(component.after.len())?;

    bytes.extend_from_slice(&component.restart_limit.to_le_bytes());
    bytes.push(u8::from(component.critical));
    bytes.push(0);
    bytes.extend_from_slice(&id_len.to_le_bytes());
    bytes.extend_from_slice(&program_len.to_le_bytes());
    bytes.extend_from_slice(&config_len.to_le_bytes());
    bytes.extend_from_slice(&slot_count.to_le_bytes());
    bytes.extend_from_slice(&requires_count.to_le_bytes());
    bytes.extend_from_slice(&after_count.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&component.budget_pages.to_le_bytes());
    bytes.extend_from_slice(&component.startup_timeout.raw().to_le_bytes());
    bytes.extend_from_slice(component.id.as_bytes());
    bytes.extend_from_slice(component.program.as_bytes());
    if let Some(config) = component.config {
        bytes.extend_from_slice(config.as_bytes());
    }

    for slot in component.slots {
        encode_slot(bytes, *slot, count)?;
    }
    for edges in [component.requires, component.after] {
        for index in edges {
            if *index >= count {
                return Err(PlanEncodeError::IndexOutOfRange);
            }
            bytes.extend_from_slice(&index.to_le_bytes());
        }
    }
    Ok(())
}

fn encode_slot(
    bytes: &mut Vec<u8>,
    slot: SlotSource<'_>,
    count: u16,
) -> Result<(), PlanEncodeError> {
    let mut header = [0u8; 12];
    let selector = match slot {
        SlotSource::Image => {
            header[0] = SLOT_KIND_IMAGE;
            None
        }
        SlotSource::Service { provider } => {
            if provider >= count {
                return Err(PlanEncodeError::IndexOutOfRange);
            }
            header[0] = SLOT_KIND_SERVICE;
            header[2..4].copy_from_slice(&provider.to_le_bytes());
            None
        }
        SlotSource::Irq(device) | SlotSource::Mmio(device) => {
            header[0] = if matches!(slot, SlotSource::Irq(_)) {
                SLOT_KIND_IRQ
            } else {
                SLOT_KIND_MMIO
            };
            Some(device)
        }
    };
    if let Some(PlanDevice {
        compatible,
        instance,
        resource,
    }) = selector
    {
        let selector_len = string_len(compatible)?;
        header[4..6].copy_from_slice(&instance.to_le_bytes());
        header[6..8].copy_from_slice(&resource.to_le_bytes());
        header[8..10].copy_from_slice(&selector_len.to_le_bytes());
    }

    bytes.extend_from_slice(&header);
    if let Some(device) = selector {
        bytes.extend_from_slice(device.compatible.as_bytes());
    }
    Ok(())
}

fn entry_count(len: usize) -> Result<u16, PlanEncodeError> {
    u16::try_from(len).map_err(|_| PlanEncodeError::TooManyEntries)
}

fn string_len(text: &str) -> Result<u16, PlanEncodeError> {
    if text.is_empty() {
        return Err(PlanEncodeError::EmptyString);
    }
    u16::try_from(text.len()).map_err(|_| PlanEncodeError::StringTooLong)
}
