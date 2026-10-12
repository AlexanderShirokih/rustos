//! [`LogConsumer`] читает кольцо лога ядра из региона, отображённого на чтение, и ждёт сигнал
//! новых записей.

use ipc::wire::IpcError;
use log_abi::{Entry, LogRing, RecordBuf, RingReader};
use syscall::{SIGNALED, SYSCALL_RETURN_TIMEOUT, Timeout, UserMemFlags, WakeCount};

use crate::{
    Result,
    object::{Mapping, MemoryRegion, Signal},
    svc::{signal_set, signal_wait_one},
};

/// Курсор чтения лога ядра. Чтение начинается со старейшей записи, которую кольцо ещё хранит.
pub struct LogConsumer {
    mapping: Mapping,
    signal: Signal,
    reader: RingReader,
    buf: RecordBuf,
}

impl LogConsumer {
    /// Отображает регион `ring` на чтение. Сигналу `ring` нужно право `Rights::WRITE` для
    /// сброса в [`wait`](Self::wait). Отказ отображения возвращается ошибкой.
    pub fn new(ring: LogRing<MemoryRegion, Signal>) -> Result<Self> {
        let mapping = Mapping::full(ring.region, UserMemFlags::ReadOnly)?;
        Ok(Self {
            mapping,
            signal: ring.signal,
            reader: RingReader::new(),
            buf: RecordBuf::new(),
        })
    }

    /// Следующая запись лога либо разрыв перед ней. `None`, если все записи прочитаны.
    pub fn try_next(&mut self) -> Option<Entry<'_>> {
        self.reader.next(&self.mapping.load_words(), &mut self.buf)
    }

    /// Возвращается, когда в кольце есть непрочитанная запись, но не позже `timeout`. Истёкший
    /// срок даёт `Timeout`, отказ операции над сигналом даёт `PeerClosed`.
    pub fn wait(&self, timeout: Timeout) -> core::result::Result<(), IpcError> {
        // Ядро поднимает сигнал после записи. Сброс до проверки кольца оставляет поднятым
        // сигнал записи, пришедшей после проверки. При отказе сброса ожидание вернулось бы
        // сразу, поэтому отказ уходит вызывающему.
        if signal_set(self.signal.as_raw(), 0, SIGNALED, WakeCount::None) < 0 {
            return Err(IpcError::PeerClosed);
        }
        if !self.reader.caught_up(&self.mapping.load_words()) {
            return Ok(());
        }
        match signal_wait_one(self.signal.as_raw(), SIGNALED, timeout) {
            SYSCALL_RETURN_TIMEOUT => Err(IpcError::Timeout),
            r if r < 0 => Err(IpcError::PeerClosed),
            _ => Ok(()),
        }
    }
}
