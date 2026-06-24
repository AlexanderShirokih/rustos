//! Потоковый транспорт ipc-контрактов поверх SPSC-кольца ([`SpscRing`]) в
//! разделяемой памяти и `Signal` для park/wake.
//!
//! `RingTransport` - однонаправленное соединение producer->consumer над одним
//! общим кольцом и одним "data"-сигналом, которым producer будит уснувшего
//! consumer'а. Роль задаётся при конструировании
//! ([`RingTransport::producer`] / [`RingTransport::consumer`]).
//!
//! Запись не блокируется: при полном кольце возвращается `WouldBlock`.
//! Capability через кольцо не передаются - `write_message` с непустым
//! handle-вектором отвергается, `read_message` отдаёт ноль хэндлов.

use collections::{PopOutcome, PushOutcome, RingError, SpscRing};
use ipc::{MessageLen, Transport, wire::IpcError};
use syscall::{Handle, SIGNALED, SYSCALL_RETURN_TIMEOUT, WakeCount};

use crate::svc::{signal_set, signal_wait_one};

/// Роль конца ring-транспорта над общим кольцом и "data"-сигналом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Producer,
    Consumer,
}

/// Потоковый транспорт ipc-контракта поверх SPSC-кольца в shared memory.
pub struct RingTransport {
    ring: SpscRing,
    /// "data"-сигнал: producer делает `set(SIGNALED)`, consumer спит на нём.
    data: Handle,
    role: Role,
}

impl RingTransport {
    /// Producer-конец: пишет кадры в кольцо, будит consumer'а.
    #[must_use]
    pub fn producer(ring: SpscRing, data: Handle) -> Self {
        Self {
            ring,
            data,
            role: Role::Producer,
        }
    }

    /// Consumer-конец: читает кадры из кольца, спит на "data"-сигнале.
    #[must_use]
    pub fn consumer(ring: SpscRing, data: Handle) -> Self {
        Self {
            ring,
            data,
            role: Role::Consumer,
        }
    }
}

impl Transport for RingTransport {
    fn write_message(&self, bytes: &[u8], handles: &[u32]) -> Result<(), IpcError> {
        if !handles.is_empty() {
            return Err(IpcError::FrameOverflow);
        }

        match self.role {
            Role::Producer => match self.ring.try_push(bytes) {
                Ok(PushOutcome { was_empty }) => {
                    // Будим consumer'а только на переходе empty->non-empty: под
                    // нагрузкой кольцо непусто и syscall'ов нет.
                    if was_empty {
                        signal_set(self.data, SIGNALED, 0, WakeCount::One);
                    }
                    Ok(())
                }
                Err(RingError::TooLarge) => Err(IpcError::FrameOverflow),
                // Полное кольцо - backpressure вызывающему; `Empty` недостижим.
                Err(RingError::Full | RingError::Empty) => Err(IpcError::WouldBlock),
            },
            // Однонаправленное кольцо: consumer-конец не пишет.
            Role::Consumer => Err(IpcError::PeerClosed),
        }
    }

    fn read_message(&self, bytes: &mut [u8], handles: &mut [u32]) -> Result<MessageLen, IpcError> {
        let _ = handles;
        match self.role {
            Role::Consumer => match self.ring.try_pop(bytes) {
                Ok(PopOutcome { len, .. }) => Ok(MessageLen::new(len, 0)),
                Err(RingError::Empty) => Err(IpcError::WouldBlock),
                // Буфер короче кадра; `Full` недостижим.
                Err(RingError::TooLarge | RingError::Full) => Err(IpcError::Truncated),
            },
            Role::Producer => Err(IpcError::WouldBlock),
        }
    }

    fn wait_readable(&self, timeout_ns: u64) -> Result<(), IpcError> {
        match self.role {
            Role::Consumer => loop {
                if !self.ring.is_empty() {
                    return Ok(());
                }
                // Чистим "data" (нужны права WRITE) и перечитываем кольцо перед
                // сном: producer публикует кадр ДО set, поэтому recheck-after-clear
                // не даёт проспать кадр, запушенный между check и clear. Провал
                // clear пробрасываем, иначе оставшийся SIGNALED крутил бы busy-spin.
                if signal_set(self.data, 0, SIGNALED, WakeCount::None) < 0 {
                    return Err(IpcError::PeerClosed);
                }
                if !self.ring.is_empty() {
                    return Ok(());
                }
                let r = signal_wait_one(self.data, SIGNALED, timeout_ns);
                if r == SYSCALL_RETURN_TIMEOUT {
                    return Err(IpcError::Timeout);
                }
                if r < 0 {
                    return Err(IpcError::PeerClosed);
                }
            },
            Role::Producer => Ok(()),
        }
    }
}
