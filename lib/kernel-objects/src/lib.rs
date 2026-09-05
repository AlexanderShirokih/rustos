//! Типизированные объекты ядра и владение их capability.
//!
//! Крейт берёт прямой зависимостью и программа на стандартной библиотеке, и программа без неё.

#![no_std]
#![allow(unsafe_code)]

mod error;
mod handle;
mod object;
mod received;
mod ring_transport;

pub use error::{Error, Result};
pub use handle::{BorrowedHandle, OwnedHandle};
pub use object::{
    AnonymousMapping, IrqControl, IrqLine, Mapping, MemoryRegion, Port, Process, ReadOnlyMapping,
    RegionInfo, RegionKind, Reply, Resource, ServicePort, Signal, Thread, ThreadEntry, wait_any,
};
pub use received::ReceivedFrame;
pub use ring_transport::RingTransport;
pub use syscall::Priority;
pub(crate) use syscall_user as svc;
pub use syscall_user::{PortTransport, ReceivedHandle};
