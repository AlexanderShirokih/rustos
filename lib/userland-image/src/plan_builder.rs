use alloc::vec::Vec;

use crate::plan::{BOOT_PLAN_MAGIC, BOOT_PLAN_VERSION, PLAN_HEADER_SIZE};

/// Компонент композиции, укладываемый в план.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentSpec<'a> {
    pub executable: &'a str,
    pub name: &'a str,
    pub memory_limit: u64,
    /// Число перезапусков после первого запуска.
    pub restarts: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanEncodeError {
    /// В плане нет записи 0 либо у неё ненулевые предел памяти или перезапуски.
    BootRecord,
    /// Строка поля пуста.
    EmptyString,
    /// Строка длиннее `u16::MAX` байт.
    StringTooLong,
    /// Число компонентов превышает `u16::MAX`.
    TooManyEntries,
    /// Предел памяти компонента равен нулю.
    ZeroMemory,
}

/// Кодирует план загрузочной композиции. Первым элементом `components` идёт запись 0 -
/// загрузочная программа без предела памяти и перезапусков.
pub fn encode_plan(components: &[ComponentSpec<'_>]) -> Result<Vec<u8>, PlanEncodeError> {
    let count = u16::try_from(components.len()).map_err(|_| PlanEncodeError::TooManyEntries)?;
    let (boot, composition) = components
        .split_first()
        .ok_or(PlanEncodeError::BootRecord)?;
    if boot.memory_limit != 0 || boot.restarts != 0 {
        return Err(PlanEncodeError::BootRecord);
    }
    if composition
        .iter()
        .any(|component| component.memory_limit == 0)
    {
        return Err(PlanEncodeError::ZeroMemory);
    }

    let mut bytes = Vec::with_capacity(PLAN_HEADER_SIZE);
    bytes.extend_from_slice(&BOOT_PLAN_MAGIC);
    bytes.extend_from_slice(&BOOT_PLAN_VERSION.to_le_bytes());
    bytes.extend_from_slice(&count.to_le_bytes());

    for component in components {
        encode_component(&mut bytes, component)?;
    }
    Ok(bytes)
}

fn encode_component(
    bytes: &mut Vec<u8>,
    component: &ComponentSpec<'_>,
) -> Result<(), PlanEncodeError> {
    let executable_len = string_len(component.executable)?;
    let name_len = string_len(component.name)?;

    // Поля заголовка идут подряд в порядке смещений `COMPONENT_*` из `plan.rs`.
    bytes.extend_from_slice(&component.restarts.to_le_bytes());
    bytes.extend_from_slice(&executable_len.to_le_bytes());
    bytes.extend_from_slice(&name_len.to_le_bytes());
    bytes.extend_from_slice(&component.memory_limit.to_le_bytes());
    bytes.extend_from_slice(component.executable.as_bytes());
    bytes.extend_from_slice(component.name.as_bytes());
    Ok(())
}

fn string_len(text: &str) -> Result<u16, PlanEncodeError> {
    if text.is_empty() {
        return Err(PlanEncodeError::EmptyString);
    }
    u16::try_from(text.len()).map_err(|_| PlanEncodeError::StringTooLong)
}
