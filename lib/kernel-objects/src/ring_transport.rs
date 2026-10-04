//! Однонаправленный поток кадров поверх SPSC-кольца ([`SpscRing`]) в разделяемой памяти и
//! `Signal` для park/wake.
//!
//! [`RingProducer`] кладёт кадры в кольцо и будит потребителя, [`RingConsumer`] извлекает их и
//! спит на том же "data"-сигнале, пока кольцо пусто. Запись не блокируется, и на полном кольце
//! возвращает [`RingError::Full`]. Capability через кольцо не передаются.

use io::{PopOutcome, PushOutcome, RingError, SpscRing};
use ipc::wire::IpcError;
use syscall::{Handle, SIGNALED, SYSCALL_RETURN_TIMEOUT, Timeout, WakeCount, kind};

use crate::{
    handle::BorrowedHandle,
    svc::{signal_set, signal_wait_one},
};

/// Пишущий конец кольца.
pub struct RingProducer {
    ring: SpscRing,
    data: Handle<kind::Signal>,
}

/// Читающий конец кольца.
pub struct RingConsumer {
    ring: SpscRing,
    data: Handle<kind::Signal>,
}

impl RingProducer {
    /// Пишущий конец над кольцом `ring`, будящий потребителя сигналом `data`.
    #[must_use]
    pub fn new(ring: SpscRing, data: BorrowedHandle<'_, kind::Signal>) -> Self {
        Self {
            ring,
            data: data.as_raw(),
        }
    }

    /// Кладёт кадр в кольцо. `Full` - в кольце нет свободного слота, `TooLarge` - кадр длиннее
    /// слота.
    pub fn push(&self, frame: &[u8]) -> Result<(), RingError> {
        let PushOutcome { wake_consumer } = self.ring.try_push(frame)?;
        // Потребителя будит только кадр, опубликованный в пустое кольцо. Под нагрузкой кольцо
        // непусто, и syscall не нужен.
        if wake_consumer {
            signal_set(self.data, SIGNALED, 0, WakeCount::One);
        }
        Ok(())
    }
}

impl RingConsumer {
    /// Читающий конец над кольцом `ring`, спящий на сигнале `data`. Сброс сигнала требует
    /// `Rights::WRITE`.
    #[must_use]
    pub fn new(ring: SpscRing, data: Handle<kind::Signal>) -> Self {
        Self { ring, data }
    }

    /// Извлекает кадр в `into`, ожидая его не дольше `timeout`, и отдаёт его длину.
    /// `Truncated` - буфер короче кадра, `Timeout` - кадр не пришёл в срок, `PeerClosed` -
    /// отказ операции над сигналом.
    pub fn pop(&self, into: &mut [u8], timeout: Timeout) -> Result<usize, IpcError> {
        loop {
            match self.ring.try_pop(into) {
                Ok(PopOutcome { len, .. }) => return Ok(len),
                Err(RingError::Empty) => self.park(timeout)?,
                Err(RingError::TooLarge | RingError::Full) => return Err(IpcError::Truncated),
            }
        }
    }

    fn park(&self, timeout: Timeout) -> Result<(), IpcError> {
        // Сигнал сбрасывается до повторной проверки кольца. Производитель публикует кадр до
        // установки сигнала, поэтому кадр между проверкой и сбросом не теряется. Отказ сброса
        // возвращается вызывающему, иначе оставшийся SIGNALED крутил бы цикл без сна.
        if signal_set(self.data, 0, SIGNALED, WakeCount::None) < 0 {
            return Err(IpcError::PeerClosed);
        }
        if !self.ring.is_empty_before_park() {
            return Ok(());
        }
        match signal_wait_one(self.data, SIGNALED, timeout) {
            SYSCALL_RETURN_TIMEOUT => Err(IpcError::Timeout),
            r if r < 0 => Err(IpcError::PeerClosed),
            _ => Ok(()),
        }
    }
}
