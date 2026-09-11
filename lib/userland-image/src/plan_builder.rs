use alloc::vec::Vec;

use component_abi::SESSION_SCOPE_MAX;

use crate::plan::{
    BOOT_PLAN_MAGIC, BOOT_PLAN_VERSION, PLAN_HEADER_SIZE, REQUIREMENT_KIND_EXPORT,
    REQUIREMENT_KIND_IRQ, REQUIREMENT_KIND_MMIO, REQUIREMENT_KIND_REGION,
};

/// Устройство требования `irq` или `mmio`, укладываемое в план.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceSpec<'a> {
    /// Драйверу подходит любая из перечисленных строк совместимости.
    pub compatible: &'a [&'a str],
    /// Номер экземпляра среди устройств той же совместимости.
    pub instance: u16,
    /// Номер ресурса устройства: линии либо окна.
    pub resource: u16,
}

/// Источник capability требования, укладываемый в план.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequirementSourceSpec<'a> {
    /// Сессия экспорта `protocol` в области `scope`. `provider` - номер провайдера в плане.
    /// Пустая область называет весь экспорт.
    Export {
        provider: u16,
        protocol: &'a str,
        scope: &'a str,
    },
    /// Линия прерывания устройства.
    Irq(DeviceSpec<'a>),
    /// MMIO-окно устройства.
    Mmio(DeviceSpec<'a>),
    /// Read-only регион userland-образа.
    Region,
}

/// Требование компонента с именем из манифеста, укладываемое в план.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequirementSpec<'a> {
    pub name: &'a str,
    pub source: RequirementSourceSpec<'a>,
}

/// Компонент композиции, укладываемый в план.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentSpec<'a> {
    pub executable: &'a str,
    pub name: &'a str,
    /// Требования компонента в порядке манифеста.
    pub requirements: &'a [RequirementSpec<'a>],
    /// Протоколы экспортов компонента в порядке объявления.
    pub exports: &'a [&'a str],
    pub memory_limit: u64,
    /// Число перезапусков после первого запуска.
    pub restarts: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanEncodeError {
    /// В плане нет ни одного компонента.
    EmptyComposition,
    /// Строка поля пуста, либо у устройства нет ни одной строки совместимости.
    EmptyString,
    /// Область длиннее `SESSION_SCOPE_MAX`.
    ScopeTooLong,
    /// Строка длиннее `u16::MAX` байт.
    StringTooLong,
    /// Число компонентов, требований, экспортов или строк совместимости превышает `u16::MAX`.
    TooManyEntries,
    /// Предел памяти компонента равен нулю.
    ZeroMemory,
}

/// Кодирует план загрузочной композиции без сверки требований с экспортами, которую делает
/// [`decode_plan`].
///
/// [`decode_plan`]: crate::decode_plan
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
    if component.memory_limit == 0 {
        return Err(PlanEncodeError::ZeroMemory);
    }
    let executable_len = string_len(component.executable)?;
    let name_len = string_len(component.name)?;
    let requirement_count = entry_count(component.requirements.len())?;
    let export_count = entry_count(component.exports.len())?;

    // Поля заголовка идут подряд в порядке смещений `COMPONENT_*` из `plan.rs`.
    bytes.extend_from_slice(&component.restarts.to_le_bytes());
    bytes.extend_from_slice(&executable_len.to_le_bytes());
    bytes.extend_from_slice(&name_len.to_le_bytes());
    bytes.extend_from_slice(&requirement_count.to_le_bytes());
    bytes.extend_from_slice(&export_count.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&component.memory_limit.to_le_bytes());
    bytes.extend_from_slice(component.executable.as_bytes());
    bytes.extend_from_slice(component.name.as_bytes());

    for requirement in component.requirements {
        encode_requirement(bytes, requirement)?;
    }
    for export in component.exports {
        encode_name(bytes, export)?;
    }
    Ok(())
}

fn encode_requirement(
    bytes: &mut Vec<u8>,
    requirement: &RequirementSpec<'_>,
) -> Result<(), PlanEncodeError> {
    let name_len = string_len(requirement.name)?;
    let mut provider = 0;
    let mut protocol_len = 0;
    let mut scope_len_field = 0;
    let mut session = None;
    let mut device = None;
    let kind = match requirement.source {
        RequirementSourceSpec::Export {
            provider: index,
            protocol,
            scope,
        } => {
            provider = index;
            protocol_len = string_len(protocol)?;
            scope_len_field = scope_len(scope)?;
            session = Some((protocol, scope));
            REQUIREMENT_KIND_EXPORT
        }
        RequirementSourceSpec::Irq(spec) => {
            device = Some(spec);
            REQUIREMENT_KIND_IRQ
        }
        RequirementSourceSpec::Mmio(spec) => {
            device = Some(spec);
            REQUIREMENT_KIND_MMIO
        }
        RequirementSourceSpec::Region => REQUIREMENT_KIND_REGION,
    };
    let (instance, resource, selector_count) = match device {
        // Драйвер работает с семейством устройств. Драйверу подходит любая из строк, а
        // выбор между ними принадлежит описанию платформы.
        Some(device) if device.compatible.is_empty() => return Err(PlanEncodeError::EmptyString),
        Some(device) => (
            device.instance,
            device.resource,
            entry_count(device.compatible.len())?,
        ),
        None => (0, 0, 0),
    };

    // Поля заголовка идут подряд в порядке смещений `REQUIREMENT_*` из `plan.rs`.
    bytes.push(kind);
    bytes.push(0);
    bytes.extend_from_slice(&name_len.to_le_bytes());
    bytes.extend_from_slice(&provider.to_le_bytes());
    bytes.extend_from_slice(&protocol_len.to_le_bytes());
    bytes.extend_from_slice(&scope_len_field.to_le_bytes());
    bytes.extend_from_slice(&instance.to_le_bytes());
    bytes.extend_from_slice(&resource.to_le_bytes());
    bytes.extend_from_slice(&selector_count.to_le_bytes());
    bytes.extend_from_slice(requirement.name.as_bytes());
    if let Some((protocol, scope)) = session {
        bytes.extend_from_slice(protocol.as_bytes());
        bytes.extend_from_slice(scope.as_bytes());
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

fn scope_len(scope: &str) -> Result<u16, PlanEncodeError> {
    u16::try_from(scope.len())
        .ok()
        .filter(|len| usize::from(*len) <= SESSION_SCOPE_MAX)
        .ok_or(PlanEncodeError::ScopeTooLong)
}

fn string_len(text: &str) -> Result<u16, PlanEncodeError> {
    if text.is_empty() {
        return Err(PlanEncodeError::EmptyString);
    }
    u16::try_from(text.len()).map_err(|_| PlanEncodeError::StringTooLong)
}
