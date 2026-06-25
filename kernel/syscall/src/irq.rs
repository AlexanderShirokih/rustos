//! IRQ-операции syscall-слоя: минт `IrqLine` по `IrqControl` и подтверждение.
//!
//! Тонкие обёртки над `capability::irq_*`. Ожидание срабатывания идёт через
//! общий `SignalWaitOne`/`SignalWaitMany` прямо по `IrqLine`-handle.

use syscall::SyscallError;

use super::{bridge::parse_handle_id, error::map_ipc_error};

/// `IrqMint`: `arg0=irq_control_handle`, `arg1=irq` (номер линии в нижних 16
/// битах). Возвращает handle на свежий `IrqLine`.
pub fn sys_irq_mint(control_handle: u64, irq: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(control_handle)?;
    let irq = u16::try_from(irq).map_err(|_| SyscallError::InvalidArgument)?;
    let line = capability::irq_mint(id, irq).map_err(map_ipc_error)?;
    Ok(u64::from(line.raw().get()))
}

/// `IrqAck`: `arg0=irq_line_handle`. Снимает latch и размаскирует линию.
pub fn sys_irq_ack(line_handle: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(line_handle)?;
    capability::irq_ack(id).map_err(map_ipc_error)?;
    Ok(0)
}
