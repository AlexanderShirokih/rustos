use alloc::vec::Vec;

use syscall::Timeout;

use crate::plan::{BOOT_PLAN_MAGIC, BOOT_PLAN_VERSION, PLAN_COMPONENT_COUNT, PLAN_HEADER_SIZE};

/// Компонент композиции, укладываемый в план.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentSpec<'a> {
    pub id: &'a str,
    pub program: &'a str,
    pub config: &'a str,
    pub budget_pages: u64,
    pub startup_timeout: Timeout,
    pub restart_limit: u32,
    pub critical: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanEncodeError {
    EmptyString,
    StringTooLong,
    ZeroBudget,
    ZeroRestartLimit,
    UnboundedStartupTimeout,
}

/// Кодирует план загрузочной композиции из одного компонента.
pub fn encode_plan(component: &ComponentSpec<'_>) -> Result<Vec<u8>, PlanEncodeError> {
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
    let config_len = string_len(component.config)?;

    let mut bytes = Vec::with_capacity(PLAN_HEADER_SIZE);
    bytes.extend_from_slice(&BOOT_PLAN_MAGIC);
    bytes.extend_from_slice(&BOOT_PLAN_VERSION.to_le_bytes());
    bytes.extend_from_slice(&PLAN_COMPONENT_COUNT.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());

    bytes.extend_from_slice(&component.restart_limit.to_le_bytes());
    bytes.push(u8::from(component.critical));
    bytes.push(0);
    bytes.extend_from_slice(&id_len.to_le_bytes());
    bytes.extend_from_slice(&program_len.to_le_bytes());
    bytes.extend_from_slice(&config_len.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&component.budget_pages.to_le_bytes());
    bytes.extend_from_slice(&component.startup_timeout.raw().to_le_bytes());
    bytes.extend_from_slice(component.id.as_bytes());
    bytes.extend_from_slice(component.program.as_bytes());
    bytes.extend_from_slice(component.config.as_bytes());

    Ok(bytes)
}

fn string_len(text: &str) -> Result<u16, PlanEncodeError> {
    if text.is_empty() {
        return Err(PlanEncodeError::EmptyString);
    }
    u16::try_from(text.len()).map_err(|_| PlanEncodeError::StringTooLong)
}
