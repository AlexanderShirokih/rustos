//! Формат устройства virtio-blk: признаки, конфигурация, заголовок запроса и байт статуса.
//!
//! Запрос - цепочка из заголовка [`HEADER_BYTES`] байт, буфера данных и байта статуса.
//! Устройство читает заголовок и данные записи, пишет данные чтения и статус. Сектор
//! заголовка и ёмкость считаются в секторах по [`SECTOR_BYTES`] байт.

use crate::{
    access::{AccessWidth, Registers},
    transport::Transport,
};

const CAPACITY_OFFSET: usize = 0;
const SIZE_MAX_OFFSET: usize = 8;
const BLK_SIZE_OFFSET: usize = 20;

const STATUS_OK: u8 = 0;
const STATUS_IOERR: u8 = 1;
const STATUS_UNSUPP: u8 = 2;

/// Размер сектора, в котором устройство считает ёмкость и адрес запроса.
pub const SECTOR_BYTES: u32 = 512;

/// Длина заголовка запроса.
pub const HEADER_BYTES: usize = 16;

/// Номер очереди запросов устройства.
pub const REQUEST_QUEUE: u16 = 0;

/// Признаки устройства.
pub mod feature {
    /// Поле `size_max` ограничивает длину одного буфера запроса.
    pub const SIZE_MAX: u64 = 1 << 1;
    /// Поле `blk_size` задаёт размер блока устройства.
    pub const BLK_SIZE: u64 = 1 << 6;
}

/// Тип запроса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum RequestKind {
    /// Чтение секторов в буфер данных.
    Read = 0,
    /// Запись секторов из буфера данных.
    Write = 1,
}

/// Исход запроса, записанный устройством в байт статуса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Запрос выполнен.
    Ok,
    /// Устройство не выполнило запрос из-за ошибки ввода-вывода.
    IoError,
    /// Устройство не поддерживает запрос.
    Unsupported,
}

/// Байт статуса вне значений формата.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownStatus(pub u8);

/// Поля конфигурации устройства.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Ёмкость в секторах.
    pub capacity: u64,
    /// Наибольшая длина одного буфера запроса в байтах. Значимо при признаке
    /// [`feature::SIZE_MAX`].
    pub size_max: u32,
    /// Размер блока в байтах. Значим при признаке [`feature::BLK_SIZE`].
    pub blk_size: u32,
}

impl Status {
    /// Разбирает байт статуса.
    pub fn parse(byte: u8) -> Result<Self, UnknownStatus> {
        match byte {
            STATUS_OK => Ok(Self::Ok),
            STATUS_IOERR => Ok(Self::IoError),
            STATUS_UNSUPP => Ok(Self::Unsupported),
            other => Err(UnknownStatus(other)),
        }
    }
}

impl Config {
    /// Возвращает поля конфигурации устройства за транспортом `transport`.
    pub fn read<R: Registers>(transport: &Transport<R>) -> Result<Self, R::Error> {
        let low = transport.read_config(CAPACITY_OFFSET, AccessWidth::Word)?;
        let high = transport.read_config(CAPACITY_OFFSET + 4, AccessWidth::Word)?;

        Ok(Self {
            capacity: u64::from(high) << 32 | u64::from(low),
            size_max: transport.read_config(SIZE_MAX_OFFSET, AccessWidth::Word)?,
            blk_size: transport.read_config(BLK_SIZE_OFFSET, AccessWidth::Word)?,
        })
    }
}

/// Возвращает заголовок запроса `kind` к сектору `sector`.
pub fn encode_header(kind: RequestKind, sector: u64) -> [u8; HEADER_BYTES] {
    let mut header = [0; HEADER_BYTES];
    let fields = (kind as u32)
        .to_le_bytes()
        .into_iter()
        .chain(0u32.to_le_bytes())
        .chain(sector.to_le_bytes());

    for (byte, field) in header.iter_mut().zip(fields) {
        *byte = field;
    }

    header
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        testing::{FakeRegisters, Log},
        transport::DeviceType,
    };

    #[test]
    fn a_read_header_of_sector_24_encodes_little_endian_fields() {
        assert_eq!(
            encode_header(RequestKind::Read, 24),
            [0, 0, 0, 0, 0, 0, 0, 0, 0x18, 0, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn status_bytes_of_the_format_parse_and_any_other_names_its_value() {
        assert_eq!(Status::parse(0), Ok(Status::Ok));
        assert_eq!(Status::parse(1), Ok(Status::IoError));
        assert_eq!(Status::parse(2), Ok(Status::Unsupported));
        assert_eq!(Status::parse(7), Err(UnknownStatus(7)));
    }

    #[test]
    fn config_fields_are_read_at_their_offsets() {
        let log = Log::default();
        let mut registers = FakeRegisters::new(DeviceType::Block, &log);
        registers.set_config(0, &0x1_0000_8000u64.to_le_bytes());
        registers.set_config(8, &65536u32.to_le_bytes());
        registers.set_config(20, &4096u32.to_le_bytes());
        let transport = Transport::probe(registers, DeviceType::Block).unwrap();

        assert_eq!(
            Config::read(&transport),
            Ok(Config {
                capacity: 0x1_0000_8000,
                size_max: 65536,
                blk_size: 4096,
            })
        );
    }
}
