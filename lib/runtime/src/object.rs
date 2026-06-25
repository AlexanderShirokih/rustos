//! Типизированные обёртки над объектами ядра поверх `OwnedHandle`.

mod memory;
mod port;
mod process;
mod signal;
mod spawn;
mod thread;

pub use memory::{AnonymousMapping, Mapping, MemoryRegion, RegionInfo, RegionKind, Resource};
pub use port::{Port, Reply};
pub use process::Process;
pub use signal::Signal;
pub use spawn::{JoinHandle, spawn};
pub use thread::{Priority, Thread, ThreadEntry};
