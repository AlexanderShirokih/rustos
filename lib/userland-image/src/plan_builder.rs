use alloc::vec::Vec;

use syscall::Timeout;

use crate::plan::{
    BOOT_PLAN_MAGIC, BOOT_PLAN_VERSION, EXHAUSTED_DETACH, EXHAUSTED_HALT,
    FLAG_ESTABLISHES_SESSIONS, OnExhausted, PLAN_HEADER_SIZE, SLOT_KIND_CONTROL,
    SLOT_KIND_DIRECTORY, SLOT_KIND_IRQ, SLOT_KIND_MMIO, SLOT_KIND_SERVICE, SLOT_KIND_SUPERVISOR,
    SLOT_KIND_USERLAND,
};

/// Устройство, названное слотом композиции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceSpec<'a> {
    /// Драйверу подходит любая из перечисленных строк совместимости.
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
    /// Сессия service-порта экземпляра `provider`.
    Service { provider: &'a str },
    /// Сессия файловой системы с каталогом компонента в роли корня, выданная
    /// экземпляром `provider` маршрутизатора имён.
    Directory { provider: &'a str },
    /// Клиентская capability supervisor, поднявшего компонент.
    Supervisor,
    /// Клиентская capability control-порта экземпляра `provider`.
    Control { provider: &'a str },
}

/// Компонент композиции, укладываемый в план.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentSpec<'a> {
    pub executable: &'a str,
    pub name: &'a str,
    pub slots: &'a [SlotSpec<'a>],
    /// Имена экземпляров, которые поднимаются раньше этого без обмена с ним.
    pub after: &'a [&'a str],
    pub memory_pages: u64,
    pub startup_timeout: Timeout,
    pub launches: u32,
    pub on_exhausted: OnExhausted,
    /// Компонент сам создаёт сессии своего service-порта.
    pub establishes_sessions: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanEncodeError {
    EmptyComposition,
    EmptyString,
    StringTooLong,
    TooManyEntries,
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
        encode_component(&mut bytes, component)?;
    }
    Ok(bytes)
}

fn encode_component(
    bytes: &mut Vec<u8>,
    component: &ComponentSpec<'_>,
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
    let after_count = entry_count(component.after.len())?;

    bytes.extend_from_slice(&component.launches.to_le_bytes());
    bytes.push(match component.on_exhausted {
        OnExhausted::Halt => EXHAUSTED_HALT,
        OnExhausted::Detach => EXHAUSTED_DETACH,
    });
    bytes.push(if component.establishes_sessions {
        FLAG_ESTABLISHES_SESSIONS
    } else {
        0
    });
    bytes.extend_from_slice(&executable_len.to_le_bytes());
    bytes.extend_from_slice(&name_len.to_le_bytes());
    bytes.extend_from_slice(&slot_count.to_le_bytes());
    bytes.extend_from_slice(&after_count.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&component.memory_pages.to_le_bytes());
    bytes.extend_from_slice(&component.startup_timeout.raw().to_le_bytes());
    bytes.extend_from_slice(component.executable.as_bytes());
    bytes.extend_from_slice(component.name.as_bytes());

    for slot in component.slots {
        encode_slot(bytes, slot)?;
    }
    for edge in component.after {
        encode_name(bytes, edge)?;
    }
    Ok(())
}

fn encode_slot(bytes: &mut Vec<u8>, slot: &SlotSpec<'_>) -> Result<(), PlanEncodeError> {
    let mut header = [0u8; 12];
    let mut instance = None;
    let mut device = None;
    match slot {
        SlotSpec::Userland => header[0] = SLOT_KIND_USERLAND,
        SlotSpec::Service { provider } => {
            header[0] = SLOT_KIND_SERVICE;
            instance = Some(*provider);
        }
        SlotSpec::Directory { provider } => {
            header[0] = SLOT_KIND_DIRECTORY;
            instance = Some(*provider);
        }
        SlotSpec::Control { provider } => {
            header[0] = SLOT_KIND_CONTROL;
            instance = Some(*provider);
        }
        SlotSpec::Supervisor => header[0] = SLOT_KIND_SUPERVISOR,
        SlotSpec::Irq(spec) => {
            header[0] = SLOT_KIND_IRQ;
            device = Some(spec);
        }
        SlotSpec::Mmio(spec) => {
            header[0] = SLOT_KIND_MMIO;
            device = Some(spec);
        }
    }
    if let Some(instance) = instance {
        header[2..4].copy_from_slice(&string_len(instance)?.to_le_bytes());
    }
    if let Some(device) = device {
        // Драйвер работает с семейством устройств. Драйверу подходит любая из строк, а
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
    if let Some(instance) = instance {
        bytes.extend_from_slice(instance.as_bytes());
    }
    if let Some(device) = device {
        for compatible in device.compatible {
            encode_name(bytes, compatible)?;
        }
    }
    Ok(())
}

fn encode_name(bytes: &mut Vec<u8>, name: &str) -> Result<(), PlanEncodeError> {
    let len = string_len(name)?;
    bytes.extend_from_slice(&len.to_le_bytes());
    bytes.extend_from_slice(name.as_bytes());
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
