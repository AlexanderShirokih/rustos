//! Объявляет объекты ядра типами и держит владение их capability: порт, сигнал, регион
//! памяти, отображение, процесс, поток, ресурс, линию прерывания.
//!
//! Вызывающий зовёт операции объекта его же методами. [`OwnedHandle`] владеет handle и
//! закрывает его на выходе из области, [`BorrowedHandle`] заимствует чужой handle. Отказ
//! операции назван [`Error`].
//!
//! [`SessionTransport`] передаёт кадры контракта через порт и владеет capability этого
//! порта. [`Session`] владеет портом сессии протокола и строит поверх него клиента этого
//! протокола. [`RingProducer`] и [`RingConsumer`] передают поток кадров через кольцо в
//! разделяемой памяти, и capability такое кольцо не переносит.
//!
//! Крейт берёт прямой зависимостью и программа на стандартной библиотеке, и программа без неё.

#![cfg_attr(not(test), no_std)]
#![allow(unsafe_code)]

mod error;
mod handle;
mod object;
mod ring_transport;
mod session_transport;

pub use error::{Error, Result};
pub use handle::{BorrowedHandle, OwnedHandle};
pub use object::{
    AnonymousMapping, IrqControl, IrqLine, Mapping, MemoryRegion, MmioMapping, MmioWindow, Port,
    Process, ReadOnlyMapping, RegionInfo, RegionKind, Reply, Resource, ServicePort, Signal, Thread,
    ThreadEntry, WriteCombiningMapping, wait_any,
};
pub use ring_transport::{RingConsumer, RingProducer};
pub use session_transport::SessionTransport;
pub use syscall::{ObjectKind, Priority, WaitableKind, kind};
pub(crate) use syscall_user as svc;
pub use syscall_user::{Delivery, IssuedHandle, PortTransport};

/// Сессия протокола `P`, владеющая портом. Клиент строится `client`, а передача владеющим
/// значением переносит порт получателю.
pub type Session<P> = ipc::wire::Session<P, Port>;
