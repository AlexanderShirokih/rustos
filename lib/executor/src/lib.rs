//! Однопоточный executor задач [`Future`] и reactor ожидания объектов ядра и сроков.
//!
//! [`Executor`] выполняет задачи на потоке, который зовёт [`Executor::block_on`]. Задача ждёт
//! сигнал объекта ядра через [`wait`] и срок через [`sleep`] либо [`timeout`]. Ожидание
//! регистрируется в executor, который опрашивает задачу. Каждый executor объединяет ожидания
//! своих задач в вызов `SignalWaitMany`, который длится не дольше ближайшего срока. Любой поток
//! может разбудить задачу через её [`Waker`](core::task::Waker).
//!
//! [`PortChannel`] даёт клиенту ipc-контракта вызывать операции из задачи. Пока операция ждёт
//! исхода, поток выполняет другие задачи.

#![no_std]
#![feature(thread_local)]

extern crate alloc;

mod either;
mod executor;
mod mutex;
mod port_channel;
mod reactor;
mod task;
mod yield_now;

pub use either::Either;
pub use executor::Executor;
pub use mutex::{Mutex, MutexGuard};
pub use port_channel::PortChannel;
pub use reactor::{Sleep, Timed, Wait, sleep, timeout, wait, wait_any};
pub use task::{JoinHandle, TaskHandle};
pub use yield_now::{YieldNow, yield_now};
