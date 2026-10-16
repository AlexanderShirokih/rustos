//! Выполнение запросов диска и состояние его отказа.

use block_abi::{BlockError, BlockInfo};
use virtio::block::{RequestKind, SECTOR_BYTES, Status};

/// Обмен запросами с устройством.
pub trait Channel {
    /// Регион памяти, доступный устройству.
    type Region;

    /// Отправляет устройству запрос `kind` к сектору `sector` с данными в первых `bytes` байтах
    /// `region` и возвращает байт статуса, который записало устройство, либо `None`, если
    /// устройство не ответило в срок или отказал транспорт.
    fn transfer(
        &mut self,
        kind: RequestKind,
        sector: u64,
        region: &Self::Region,
        bytes: u32,
    ) -> Option<u8>;

    /// Сбрасывает устройство и возвращает `true` при успехе. После успеха устройство не
    /// обращается к регионам запросов.
    fn reset(&mut self) -> bool;
}

/// Буфер данных сессии: регион и размер, с которым его запросил клиент.
pub struct SessionBuffer<R> {
    /// Регион буфера.
    pub region: R,
    /// Размер в байтах.
    pub size: u32,
}

/// Диск: геометрия, обмен с устройством и состояние его отказа.
pub struct Disk<C: Channel> {
    channel: C,
    info: BlockInfo,
    health: Health,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Health {
    Working,
    Failed,
    Unreset,
}

impl<C: Channel> Disk<C> {
    /// Обслуживает диск геометрии `info` через `channel`. `info` должна быть геометрией, которую
    /// [`geometry`](crate::geometry) вернула для устройства за `channel`.
    pub fn new(channel: C, info: BlockInfo) -> Self {
        Self {
            channel,
            info,
            health: Health::Working,
        }
    }

    /// Сведения о диске.
    pub fn info(&self) -> BlockInfo {
        self.info
    }

    /// Выполняет запрос `kind` к `count` блокам с блока `lba` через буфер сессии `buffer`.
    ///
    /// Отказ обмена сбрасывает устройство и переводит диск в отказ до конца процесса.
    ///
    /// Отказы:
    /// - `NoBuffer` - у сессии нет буфера;
    /// - `BufferTooSmall` - блоки длиннее буфера;
    /// - `OutOfRange` - `lba + count` больше числа блоков либо не помещается в `u64`;
    /// - `Device` - устройство ответило ошибкой, не ответило либо диск в отказе.
    pub fn serve(
        &mut self,
        kind: RequestKind,
        lba: u64,
        count: u32,
        buffer: Option<&SessionBuffer<C::Region>>,
    ) -> Result<(), BlockError> {
        let SessionBuffer { region, size } = buffer.ok_or(BlockError::NoBuffer)?;
        let bytes = u32::try_from(u64::from(count) * u64::from(self.info.block_size))
            .ok()
            .filter(|bytes| bytes <= size)
            .ok_or(BlockError::BufferTooSmall)?;
        let end = lba.checked_add(u64::from(count));
        if end.is_none_or(|end| end > self.info.blocks) {
            return Err(BlockError::OutOfRange);
        }

        if count == 0 {
            return Ok(());
        }

        if self.health != Health::Working {
            return Err(BlockError::Device);
        }

        // Блок `lba` лежит на диске, поэтому его сектор не превышает ёмкость устройства.
        let sector = lba * u64::from(self.info.block_size / SECTOR_BYTES);
        match self.channel.transfer(kind, sector, region, bytes) {
            Some(byte) if Status::parse(byte) == Ok(Status::Ok) => Ok(()),
            Some(_) => Err(BlockError::Device),
            None => {
                self.health = if self.channel.reset() {
                    Health::Failed
                } else {
                    Health::Unreset
                };
                Err(BlockError::Device)
            }
        }
    }

    /// Освобождает регион буфера ушедшей сессии. Если устройство не удалось сбросить после
    /// отказа, регион остаётся занятым до конца процесса.
    pub fn release(&self, region: C::Region) {
        if self.health == Health::Unreset {
            core::mem::forget(region);
        }
    }
}
