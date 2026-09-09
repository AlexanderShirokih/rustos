//! Объявляет объекты ядра типами и держит владение их capability: порт, сигнал, регион
//! памяти, отображение, процесс, поток, ресурс, линию прерывания.
//!
//! Вызывающий зовёт операции объекта его же методами. [`OwnedHandle`] владеет handle и
//! закрывает его на выходе из области, [`BorrowedHandle`] заимствует чужой handle. Отказ
//! операции назван [`Error`].
//!
//! [`ReceivedFrame`] держит capability доставленного кадра, пока вызывающий не заберёт их
//! методом `claim`. [`RingTransport`] ведёт транспорт `ipc` поверх кольца в разделяемой
//! памяти, capability через это кольцо не идут.
//!
//! Крейт берёт прямой зависимостью и программа на стандартной библиотеке, и программа без неё.

#![cfg_attr(not(test), no_std)]
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
pub use received::{ReceivedFrame, settle, vend};
pub use ring_transport::RingTransport;
pub use syscall::{ObjectKind, Priority, WaitableKind, kind};
pub(crate) use syscall_user as svc;
pub use syscall_user::{PortTransport, ReceivedHandle};
