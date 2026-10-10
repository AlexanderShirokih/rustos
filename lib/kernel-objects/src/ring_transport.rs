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
use syscall::{SIGNALED, SYSCALL_RETURN_TIMEOUT, Timeout, UserMemFlags, WakeCount};

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

    /// Число кадров, которые поместятся в кольцо сейчас. Не превышает ёмкости кольца при любом
    /// содержимом региона.
    #[must_use]
    pub fn free_slots(&self) -> usize {
        self.0.ring.free_slots()
    }

    /// Кладёт кадр в кольцо. `Full` - в кольце нет свободного слота, `TooLarge` - кадр длиннее
    /// слота.
    pub fn push(&self, frame: &[u8]) -> Result<(), RingError> {
        let PushOutcome { wake_consumer } = self.0.ring.try_push(frame)?;
        // Потребителя будит только кадр, опубликованный в пустое кольцо. Под нагрузкой кольцо
        // непусто, и syscall не нужен.
        if wake_consumer {
            signal_set(self.0.data.as_raw(), SIGNALED, 0, WakeCount::One);
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

    /// Извлекает кадр в `into`, ожидая его не дольше `timeout`, и отдаёт его длину.
    /// `Truncated` - буфер короче кадра, `Timeout` - кадр не пришёл в срок, `PeerClosed` -
    /// отказ операции над сигналом.
    pub fn pop(&self, into: &mut [u8], timeout: Timeout) -> Result<usize, IpcError> {
        loop {
            match self.0.ring.try_pop(into) {
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
        if signal_set(self.0.data.as_raw(), 0, SIGNALED, WakeCount::None) < 0 {
            return Err(IpcError::PeerClosed);
        }
        if !self.0.ring.is_empty_before_park() {
            return Ok(());
        }
        match signal_wait_one(self.0.data.as_raw(), SIGNALED, timeout) {
            SYSCALL_RETURN_TIMEOUT => Err(IpcError::Timeout),
            r if r < 0 => Err(IpcError::PeerClosed),
            _ => Ok(()),
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
