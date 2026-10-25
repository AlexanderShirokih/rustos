//! Конфигурация и события virtio-input.
//!
//! Запись `select` и `subsel` выбирает раздел конфигурации, `size` задаёт число его байт в
//! объединении. Событие занимает 8 байт: le16 тип, le16 код и le32 значение.

use crate::{
    access::{AccessWidth, Registers},
    transport::Transport,
};

const SELECT_OFFSET: usize = 0;
const SUBSEL_OFFSET: usize = 1;
const SIZE_OFFSET: usize = 2;
const UNION_OFFSET: usize = 8;

const ABS_INFO: u8 = 0x12;
const ABS_RANGE_BYTES: u32 = 8;

/// Длина объединения конфигурации.
pub const SECTION_BYTES: usize = 128;

/// Длина события.
pub const EVENT_BYTES: usize = 8;

/// Номер очереди событий устройства.
pub const EVENT_QUEUE: u16 = 0;

/// Значения `select`: разделы конфигурации.
pub mod select {
    /// Имя устройства.
    pub const ID_NAME: u8 = 0x01;
    /// Битовая карта кодов событий типа `subsel`: бит n - код n.
    pub const EV_BITS: u8 = 0x11;
}

/// Типы событий.
pub mod kind {
    /// Синхронизация.
    pub const SYN: u16 = 0;
    /// Клавиша или кнопка.
    pub const KEY: u16 = 1;
    /// Относительная ось.
    pub const REL: u16 = 2;
    /// Абсолютная ось.
    pub const ABS: u16 = 3;
}

/// Коды событий типа [`kind::SYN`].
pub mod sync {
    /// Конец кадра событий.
    pub const REPORT: u16 = 0;
    /// Устройство потеряло события текущего кадра.
    pub const DROPPED: u16 = 3;
}

/// Коды событий типа [`kind::ABS`] протокола касаний по слотам.
pub mod abs {
    /// Выбор слота, к которому относятся следующие события касания.
    pub const MT_SLOT: u16 = 0x2f;
    /// Координата X касания слота.
    pub const MT_POSITION_X: u16 = 0x35;
    /// Координата Y касания слота.
    pub const MT_POSITION_Y: u16 = 0x36;
    /// Номер касания слота, -1 - конец касания.
    pub const MT_TRACKING_ID: u16 = 0x39;
}

/// Событие устройства.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    /// Тип события из [`kind`].
    pub kind: u16,
    /// Код клавиши, оси или синхронизации.
    pub code: u16,
    /// Значение.
    pub value: i32,
}

/// Диапазон абсолютной оси.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbsInfo {
    /// Наименьшее значение.
    pub min: i32,
    /// Наибольшее значение.
    pub max: i32,
}

/// Буфер события короче [`EVENT_BYTES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Truncated;

impl Event {
    /// Разбирает событие из первых [`EVENT_BYTES`] байт `bytes`.
    pub fn parse(bytes: &[u8]) -> Result<Self, Truncated> {
        let Some(&[k0, k1, c0, c1, v0, v1, v2, v3]) = bytes.first_chunk::<EVENT_BYTES>() else {
            return Err(Truncated);
        };
        Ok(Self {
            kind: u16::from_le_bytes([k0, k1]),
            code: u16::from_le_bytes([c0, c1]),
            value: i32::from_le_bytes([v0, v1, v2, v3]),
        })
    }
}

/// Выбирает раздел `select` с `subsel` и копирует его байты в `destination`.
///
/// Возвращает прочитанную часть `destination`. Байты читаются обращениями в 1 байт.
pub async fn read_section<'a, R: Registers>(
    transport: &mut Transport<R>,
    select: u8,
    subsel: u8,
    destination: &'a mut [u8; SECTION_BYTES],
) -> Result<&'a [u8], R::Error> {
    let size = choose(transport, select, subsel).await?;
    let length = usize::try_from(size).map_or(SECTION_BYTES, |size| size.min(SECTION_BYTES));
    let (section, _) = destination.split_at_mut(length);
    for (offset, byte) in (UNION_OFFSET..).zip(section.iter_mut()) {
        let value = transport.read_config(offset, AccessWidth::Byte).await?;
        *byte = value.to_le_bytes()[0];
    }

    Ok(section)
}

/// Читает диапазон абсолютной оси `axis` 32-битными обращениями.
///
/// Возвращает `None`, если раздел оси короче 8 байт.
pub async fn read_abs_info<R: Registers>(
    transport: &mut Transport<R>,
    axis: u8,
) -> Result<Option<AbsInfo>, R::Error> {
    if choose(transport, ABS_INFO, axis).await? < ABS_RANGE_BYTES {
        return Ok(None);
    }

    let min = transport
        .read_config(UNION_OFFSET, AccessWidth::Word)
        .await?;
    let max = transport
        .read_config(UNION_OFFSET + 4, AccessWidth::Word)
        .await?;
    Ok(Some(AbsInfo {
        min: min.cast_signed(),
        max: max.cast_signed(),
    }))
}

async fn choose<R: Registers>(
    transport: &mut Transport<R>,
    select: u8,
    subsel: u8,
) -> Result<u32, R::Error> {
    transport
        .write_config(SELECT_OFFSET, AccessWidth::Byte, u32::from(select))
        .await?;
    transport
        .write_config(SUBSEL_OFFSET, AccessWidth::Byte, u32::from(subsel))
        .await?;
    transport.read_config(SIZE_OFFSET, AccessWidth::Byte).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        testing::{Event as Access, FakeRegisters, Log, complete},
        transport::DeviceType,
    };

    fn input_transport(log: &Log, config: &[(usize, &[u8])]) -> Transport<FakeRegisters> {
        let mut registers = FakeRegisters::new(DeviceType::Input, log);
        for (offset, bytes) in config {
            registers.set_config(*offset, bytes);
        }

        let transport = complete(Transport::probe(registers, DeviceType::Input)).unwrap();
        log.clear();
        transport
    }

    #[test]
    fn section_reads_its_size_with_one_byte_access_and_returns_size_bytes() {
        let log = Log::default();
        let mut transport =
            input_transport(&log, &[(SIZE_OFFSET, &[5]), (UNION_OFFSET, b"mouse!")]);
        let mut destination = [0; SECTION_BYTES];

        let name = complete(read_section(
            &mut transport,
            select::ID_NAME,
            0,
            &mut destination,
        ))
        .unwrap();

        assert_eq!(name, b"mouse");
        assert_eq!(
            log.events()[..3],
            [
                Access::ConfigWrite(SELECT_OFFSET, AccessWidth::Byte, 1),
                Access::ConfigWrite(SUBSEL_OFFSET, AccessWidth::Byte, 0),
                Access::ConfigRead(SIZE_OFFSET, AccessWidth::Byte),
            ]
        );
    }

    #[test]
    fn key_event_is_parsed_from_little_endian_fields() {
        let event = Event::parse(&[0x01, 0x00, 0x1e, 0x00, 0x01, 0x00, 0x00, 0x00]);

        assert_eq!(
            event,
            Ok(Event {
                kind: kind::KEY,
                code: 30,
                value: 1,
            })
        );
    }

    #[test]
    fn event_shorter_than_eight_bytes_is_truncated() {
        assert_eq!(Event::parse(&[0; EVENT_BYTES - 1]), Err(Truncated));
    }
}
