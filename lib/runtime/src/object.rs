use syscall::{Handle, Timeout};

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
pub use memory::{AnonymousMapping, Mapping, MemoryRegion, RegionInfo, RegionKind, Resource};
pub use port::{Port, Reply};
pub use process::Process;
pub use signal::Signal;
pub use spawn::{JoinHandle, spawn};
pub use thread::{Priority, Thread, ThreadEntry};

pub(super) fn wait_signals(handle: Handle, mask: u32, timeout: Timeout) -> Result<u32> {
    value(svc::signal_wait_one(handle, mask, timeout.raw()))
        .map(|observed| (observed & 0xFFFF_FFFF) as u32)
}
