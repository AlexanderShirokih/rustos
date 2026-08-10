use alloc::vec::Vec;

use syscall::Timeout;

use crate::plan::{
    BOOT_PLAN_MAGIC, BOOT_PLAN_VERSION, EXHAUSTED_DETACH, EXHAUSTED_HALT, OnExhausted,
    PLAN_HEADER_SIZE, SLOT_KIND_DIRECTORY, SLOT_KIND_IRQ, SLOT_KIND_MMIO, SLOT_KIND_SERVICE,
    SLOT_KIND_SUPERVISOR, SLOT_KIND_USERLAND,
};

/// Устройство, названное слотом композиции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceSpec<'a> {
    /// Строки совместимости драйвера: подходит любая из них.
    pub compatible: &'a [&'a str],
    /// Номер экземпляра среди устройств той же совместимости.
    pub instance: u16,
    /// Номер ресурса устройства: линии либо окна.
    pub resource: u16,
}

/// Источник capability слота, укладываемый в план.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotSpec<'a> {
    /// Read-only регион userland-образа.
    Userland,
    /// Линия прерывания устройства.
    Irq(DeviceSpec<'a>),
    /// MMIO-окно устройства.
    Mmio(DeviceSpec<'a>),
    /// Сессия service-порта компонента с номером `provider` в плане.
    Service { provider: u16 },
    /// Сессия файловой системы с каталогом компонента в роли корня, выданная
    /// маршрутизатором имён с номером `provider` в плане.
    Directory { provider: u16 },
    /// Клиентская capability supervisor, поднявшего компонент.
    Supervisor,
}

/// Компонент композиции, укладываемый в план.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentSpec<'a> {
    pub executable: &'a str,
    pub name: &'a str,
    pub slots: &'a [SlotSpec<'a>],
    pub requires: &'a [u16],
    pub after: &'a [u16],
    pub memory_pages: u64,
    pub startup_timeout: Timeout,
    pub launches: u32,
    pub on_exhausted: OnExhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanEncodeError {
    EmptyComposition,
    EmptyString,
    StringTooLong,
    TooManyEntries,
    IndexOutOfRange,
    ZeroMemory,
    ZeroLaunches,
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
    if component.memory_pages == 0 {
        return Err(PlanEncodeError::ZeroMemory);
    }
    if component.launches == 0 {
        return Err(PlanEncodeError::ZeroLaunches);
    }
    if component.startup_timeout == Timeout::POLL || component.startup_timeout == Timeout::INFINITE
    {
        return Err(PlanEncodeError::UnboundedStartupTimeout);
    }
    let executable_len = string_len(component.executable)?;
    let name_len = string_len(component.name)?;
    let slot_count = entry_count(component.slots.len())?;
    let requires_count = entry_count(component.requires.len())?;
    let after_count = entry_count(component.after.len())?;

    bytes.extend_from_slice(&component.launches.to_le_bytes());
    bytes.push(match component.on_exhausted {
        OnExhausted::Halt => EXHAUSTED_HALT,
        OnExhausted::Detach => EXHAUSTED_DETACH,
    });
    bytes.push(0);
    bytes.extend_from_slice(&executable_len.to_le_bytes());
    bytes.extend_from_slice(&name_len.to_le_bytes());
    bytes.extend_from_slice(&slot_count.to_le_bytes());
    bytes.extend_from_slice(&requires_count.to_le_bytes());
    bytes.extend_from_slice(&after_count.to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&component.memory_pages.to_le_bytes());
    bytes.extend_from_slice(&component.startup_timeout.raw().to_le_bytes());
    bytes.extend_from_slice(component.executable.as_bytes());
    bytes.extend_from_slice(component.name.as_bytes());

    for slot in component.slots {
        encode_slot(bytes, slot, count)?;
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
    slot: &SlotSpec<'_>,
    count: u16,
) -> Result<(), PlanEncodeError> {
    let mut header = [0u8; 12];
    let device = match slot {
        SlotSpec::Userland => {
            header[0] = SLOT_KIND_USERLAND;
            None
        }
        SlotSpec::Service { provider } => {
            if *provider >= count {
                return Err(PlanEncodeError::IndexOutOfRange);
            }
            header[0] = SLOT_KIND_SERVICE;
            header[2..4].copy_from_slice(&provider.to_le_bytes());
            None
        }
        SlotSpec::Directory { provider } => {
            if *provider >= count {
                return Err(PlanEncodeError::IndexOutOfRange);
            }
            header[0] = SLOT_KIND_DIRECTORY;
            header[2..4].copy_from_slice(&provider.to_le_bytes());
            None
        }
        SlotSpec::Supervisor => {
            header[0] = SLOT_KIND_SUPERVISOR;
            None
        }
        SlotSpec::Irq(device) => {
            header[0] = SLOT_KIND_IRQ;
            Some(device)
        }
        SlotSpec::Mmio(device) => {
            header[0] = SLOT_KIND_MMIO;
            Some(device)
        }
    };
    if let Some(device) = device {
        // Драйвер работает с семейством устройств: подходит любая из строк, и
        // выбор между ними принадлежит описанию платформы.
        if device.compatible.is_empty() {
            return Err(PlanEncodeError::EmptyString);
        }
        let selector_count = entry_count(device.compatible.len())?;
        header[4..6].copy_from_slice(&device.instance.to_le_bytes());
        header[6..8].copy_from_slice(&device.resource.to_le_bytes());
        header[8..10].copy_from_slice(&selector_count.to_le_bytes());
    }

    bytes.extend_from_slice(&header);
    if let Some(device) = device {
        for compatible in device.compatible {
            let len = string_len(compatible)?;
            bytes.extend_from_slice(&len.to_le_bytes());
            bytes.extend_from_slice(compatible.as_bytes());
        }
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
