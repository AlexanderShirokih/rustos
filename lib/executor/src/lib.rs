//! Однопоточный executor задач [`Future`] и reactor ожидания объектов ядра.
//!
//! [`Executor`] выполняет задачи на потоке, который зовёт [`Executor::block_on`]. Задача ждёт
//! сигнал объекта ядра через [`Reactor::wait`], и ожидания всех задач потока идут одним
//! `SignalWaitMany`. Любой поток может разбудить задачу через её
//! [`Waker`](core::task::Waker), а [`channel`] передаёт задаче значения от любого потока.

#![no_std]

extern crate alloc;

mod channel;
mod executor;
mod reactor;
mod yield_now;

pub use channel::{Channel, Receiver, Sender, channel};
pub use executor::Executor;
pub use reactor::{Reactor, Wait};
pub use yield_now::{YieldNow, yield_now};
