//! Однонаправленный поток кадров поверх SPSC-кольца ([`SpscRing`]) в разделяемой памяти и
//! `Signal` для park/wake.
//!
//! [`RingProducer`] кладёт кадры в кольцо и будит потребителя, [`RingConsumer`] извлекает их и
//! спит на том же "data"-сигнале, пока кольцо пусто. Запись не блокируется, и на полном кольце
//! возвращает [`RingError::Full`]. Capability через кольцо не передаются.
//!
//! Конец кольца владеет отображением региона вместе с регионом и сигналом. Отображение
//! действует до уничтожения конца либо до отзыва handle региона выдавшей его стороной.

use io::{PopOutcome, PushOutcome, RingError, SpscRing};
use ipc::wire::IpcError;
use syscall::{SIGNALED, SYSCALL_RETURN_TIMEOUT, Timeout, UserMemFlags, WaitItem, WakeCount};

use crate::{
    object::{Mapping, MemoryRegion, Signal},
    svc::{signal_set, signal_wait_one},
};

/// Пишущий конец кольца.
pub struct RingProducer(RingEnd);

/// Читающий конец кольца.
pub struct RingConsumer(RingEnd);

struct RingEnd {
    ring: SpscRing,
    _mapping: Mapping,
    data: Signal,
}

impl RingProducer {
    /// Пишущий конец над кольцом в регионе `region` со слотами по `slot_payload` байт,
    /// будящий потребителя сигналом `data`. Регион отображается целиком на чтение и запись.
    /// `None` - регион не отображается либо не вмещает заголовок кольца и один слот.
    #[must_use]
    pub fn new(region: MemoryRegion, slot_payload: usize, data: Signal) -> Option<Self> {
        RingEnd::over(region, slot_payload, data).map(Self)
    }

    /// Публикует кадр. ResourceExhausted означает полное кольцо, Truncated - длинный кадр.
    /// PeerClosed означает отказ уведомления после публикации: повторять этот кадр нельзя.
    pub fn push(&self, frame: &[u8]) -> Result<(), IpcError> {
        let PushOutcome { wake_consumer } =
            self.0.ring.try_push(frame).map_err(|error| match error {
                RingError::Full => IpcError::ResourceExhausted,
                RingError::TooLarge | RingError::Empty => IpcError::Truncated,
            })?;
        if wake_consumer && signal_set(self.0.data.as_raw(), SIGNALED, 0, WakeCount::One) < 0 {
            return Err(IpcError::PeerClosed);
        }

        Ok(())
    }
}

impl RingConsumer {
    /// Читающий конец над кольцом в регионе `region` со слотами по `slot_payload` байт,
    /// спящий на сигнале `data`. Регион отображается целиком на чтение и запись, сброс сигнала
    /// требует `Rights::WRITE`. `None` - регион не отображается либо не вмещает заголовок
    /// кольца и один слот.
    #[must_use]
    pub fn new(region: MemoryRegion, slot_payload: usize, data: Signal) -> Option<Self> {
        RingEnd::over(region, slot_payload, data).map(Self)
    }

    /// Извлекает кадр либо готовит сигнал к ожиданию и повторно проверяет кольцо.
    /// None разрешает ждать wait_item: публикация между сбросом и проверкой не теряет wakeup.
    pub fn try_pop(&self, into: &mut [u8]) -> Result<Option<usize>, IpcError> {
        match self.0.ring.try_pop(into) {
            Ok(PopOutcome { len, .. }) => return Ok(Some(len)),
            Err(RingError::Empty) => {}
            Err(RingError::TooLarge | RingError::Full) => return Err(IpcError::Truncated),
        }

        if signal_set(self.0.data.as_raw(), 0, SIGNALED, WakeCount::None) < 0 {
            return Err(IpcError::PeerClosed);
        }

        match self.0.ring.try_pop(into) {
            Ok(PopOutcome { len, .. }) => Ok(Some(len)),
            Err(RingError::Empty) => Ok(None),
            Err(RingError::TooLarge | RingError::Full) => Err(IpcError::Truncated),
        }
    }

    /// Условие ожидания, которое используется после try_pop без кадра.
    pub fn wait_item(&self) -> WaitItem {
        WaitItem::new(self.0.data.as_raw(), SIGNALED)
    }

    /// Извлекает кадр в into, ожидая его не дольше timeout. Ошибка сигнала - PeerClosed.
    pub fn pop(&self, into: &mut [u8], timeout: Timeout) -> Result<usize, IpcError> {
        loop {
            if let Some(len) = self.try_pop(into)? {
                return Ok(len);
            }

            match signal_wait_one(self.0.data.as_raw(), SIGNALED, timeout) {
                SYSCALL_RETURN_TIMEOUT => return Err(IpcError::Timeout),
                r if r < 0 => return Err(IpcError::PeerClosed),
                _ => {}
            }
        }
    }
}

impl RingEnd {
    fn over(region: MemoryRegion, slot_payload: usize, data: Signal) -> Option<Self> {
        let mapping = Mapping::full(region, UserMemFlags::ReadWrite).ok()?;
        let len = usize::try_from(mapping.size_bytes()).ok()?;
        // SAFETY: ядро отобразило `[va, va + size_bytes)` через handle `region`, и конец держит
        // отображение вместе с регионом, пока живо кольцо. Страничное отображение выровнено под
        // `AtomicU32`. `SpscRing` обращается к региону атомиками в его границах.
        let ring = unsafe { SpscRing::from_raw(mapping.va() as *mut u8, len, slot_payload) }?;
        Some(Self {
            ring,
            _mapping: mapping,
            data,
        })
    }
}
