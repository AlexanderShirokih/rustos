use syscall::{Handle, Timeout, WaitItem};

use crate::{
    error::{Result, value},
    svc,
};

mod irq;
mod memory;
mod port;
mod process;
mod signal;
mod spawn;
mod thread;

pub use irq::{IrqControl, IrqLine};
pub use memory::{
    AnonymousMapping, Mapping, MemoryRegion, ReadOnlyMapping, RegionInfo, RegionKind, Resource,
};
pub use port::{Port, Reply, ServicePort};
pub use process::Process;
pub use signal::Signal;
pub use spawn::{JoinHandle, spawn, spawn_owned};
pub use thread::{Priority, Thread, ThreadEntry};

pub(super) fn wait_signals(handle: Handle, mask: u32, timeout: Timeout) -> Result<u32> {
    value(svc::signal_wait_one(handle, mask, timeout.raw()))
        .map(|observed| (observed & 0xFFFF_FFFF) as u32)
}

/// Ждёт сигнал из маски на любой из записей `items` до `timeout`. Возвращает
/// индекс сработавшей записи. Длина `items` - от 1 до
/// [`syscall::WAIT_MANY_MAX_COUNT`].
pub fn wait_any(items: &[WaitItem], timeout: Timeout) -> Result<usize> {
    let (observed, index) = svc::signal_wait_many(items, timeout.raw());
    value(observed)?;
    Ok(usize::try_from(index).expect("wait index is an index into items"))
}
