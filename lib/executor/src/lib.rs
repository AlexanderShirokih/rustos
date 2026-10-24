//! Однопоточный executor задач [`Future`] и reactor ожидания объектов ядра и сроков.
//!
//! [`Executor`] выполняет задачи на потоке, который зовёт [`Executor::block_on`]. Задача ждёт
//! сигнал объекта ядра через [`Reactor::wait`] и срок через [`Reactor::sleep`] либо
//! [`Reactor::timeout`]. Ожидания всех задач потока идут одним `SignalWaitMany`, которое длится
//! не дольше ближайшего срока. Любой поток может разбудить задачу через её
//! [`Waker`](core::task::Waker).
//!
//! [`PortChannel`] даёт клиенту ipc-контракта вызывать операции из задачи. Пока операция ждёт
//! исхода, поток выполняет другие задачи.

#![no_std]

extern crate alloc;

mod executor;
mod port_channel;
mod reactor;
mod yield_now;

pub use executor::Executor;
pub use port_channel::PortChannel;
pub use reactor::{Reactor, Sleep, Timed, Wait};
pub use yield_now::{YieldNow, yield_now};
