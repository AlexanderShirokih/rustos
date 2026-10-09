//! Запись события и последовательность записей.

use core::slice::Iter;

use ipc::{
    IntoWireValue, ReceivedResources, WireTyped, WireValue,
    schema::WireType,
    wire::{FIELD_DATA_MAX, IpcError, MessageBuf},
};

use crate::packed::Packed;

/// Размер записи события в байтах.
pub const RECORD_SIZE: usize = 16;

/// Наибольшее число записей в одном поле сообщения.
pub const MESSAGE_RECORDS_MAX: usize = FIELD_DATA_MAX / RECORD_SIZE;

/// Номер контакта в записи оси устройства.
pub const NO_CONTACT: u16 = 0xFFFF;

/// Флаг записи `ContactUp`: касание отменено вместе с жестом.
pub const CANCEL: u8 = 1;

/// Вид записи события.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Нажатие (значение 1) или отпускание (значение 0) клавиши. Код - индекс клавиши в списке
    /// клавиш дескриптора.
    Key,
    /// Значение оси контакта с его номером либо оси устройства с номером [`NO_CONTACT`]. Код -
    /// индекс оси в списке осей контакта либо устройства.
    Axis,
    /// Начало касания.
    ContactDown,
    /// Конец касания, с флагом [`CANCEL`] - отмена жеста.
    ContactUp,
    /// Конец кадра, значение - время в наносекундах.
    Frame,
    /// Появление устройства.
    DeviceAdded,
    /// Уход устройства.
    DeviceRemoved,
    /// Сброс состояния всех устройств.
    Resync,
    /// Вид вне модели. Читатель пропускает такую запись.
    Unknown(u8),
}

/// Запись события.
///
/// Раскладка little-endian: вид `u8`, флаги `u8`, устройство `u16`, код `u16`, номер контакта
/// `u16`, значение `i64`. Время записи `Frame` хранится в значении как `u64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record {
    pub kind: Kind,
    pub flags: u8,
    pub device: u16,
    pub code: u16,
    pub contact: u16,
    pub value: i64,
}

/// Последовательность записей ёмкостью [`MESSAGE_RECORDS_MAX`].
///
/// Передаётся полем сообщения в раскладке [`RecordBuf::as_bytes`]. Поле с длиной, не кратной
/// [`RECORD_SIZE`], читается как `InvalidValue`.
#[derive(Clone)]
pub struct RecordBuf {
    records: Packed<RECORD_SIZE, MESSAGE_RECORDS_MAX>,
}

/// Записи последовательности по порядку.
pub struct Records<'a> {
    chunks: Iter<'a, [u8; RECORD_SIZE]>,
}

/// Отказ операции над последовательностью записей.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordsError {
    /// Длина последовательности не кратна [`RECORD_SIZE`].
    Misaligned,
    /// Буфер заполнен.
    Full,
}

impl Kind {
    const KNOWN: [Self; 8] = [
        Self::Key,
        Self::Axis,
        Self::ContactDown,
        Self::ContactUp,
        Self::Frame,
        Self::DeviceAdded,
        Self::DeviceRemoved,
        Self::Resync,
    ];

    fn from_raw(raw: u8) -> Self {
        // Номер известного вида на единицу больше его позиции в `KNOWN`.
        let known = usize::from(raw)
            .checked_sub(1)
            .and_then(|index| Self::KNOWN.get(index));
        known.copied().unwrap_or(Self::Unknown(raw))
    }

    fn raw(self) -> u8 {
        match self {
            Self::Key => 1,
            Self::Axis => 2,
            Self::ContactDown => 3,
            Self::ContactUp => 4,
            Self::Frame => 5,
            Self::DeviceAdded => 6,
            Self::DeviceRemoved => 7,
            Self::Resync => 8,
            Self::Unknown(raw) => raw,
        }
    }
}

impl Record {
    const fn new(kind: Kind, device: u16) -> Self {
        Self {
            kind,
            flags: 0,
            device,
            code: 0,
            contact: NO_CONTACT,
            value: 0,
        }
    }

    /// Нажатие либо отпускание клавиши с индексом `key`.
    pub fn key(device: u16, key: u16, pressed: bool) -> Self {
        Self {
            code: key,
            value: i64::from(pressed),
            ..Self::new(Kind::Key, device)
        }
    }

    /// Значение оси с индексом `axis` у контакта `contact` либо у устройства при [`NO_CONTACT`].
    pub fn axis(device: u16, axis: u16, contact: u16, value: i64) -> Self {
        Self {
            code: axis,
            contact,
            value,
            ..Self::new(Kind::Axis, device)
        }
    }

    /// Начало касания контактом `contact`.
    pub fn contact_down(device: u16, contact: u16) -> Self {
        Self {
            contact,
            ..Self::new(Kind::ContactDown, device)
        }
    }

    /// Конец касания контактом `contact`, при `cancel` - с флагом [`CANCEL`].
    pub fn contact_up(device: u16, contact: u16, cancel: bool) -> Self {
        Self {
            flags: if cancel { CANCEL } else { 0 },
            contact,
            ..Self::new(Kind::ContactUp, device)
        }
    }

    /// Конец кадра со временем `nanos`.
    pub fn frame(device: u16, nanos: u64) -> Self {
        Self {
            value: i64::from_le_bytes(nanos.to_le_bytes()),
            ..Self::new(Kind::Frame, device)
        }
    }

    /// Появление устройства.
    pub fn device_added(device: u16) -> Self {
        Self::new(Kind::DeviceAdded, device)
    }

    /// Уход устройства.
    pub fn device_removed(device: u16) -> Self {
        Self::new(Kind::DeviceRemoved, device)
    }

    /// Сброс состояния всех устройств.
    pub fn resync() -> Self {
        Self::new(Kind::Resync, 0)
    }

    /// Время записи `Frame` в наносекундах.
    pub fn nanos(&self) -> u64 {
        u64::from_le_bytes(self.value.to_le_bytes())
    }

    /// Раскладка записи.
    pub fn to_bytes(&self) -> [u8; RECORD_SIZE] {
        let fields = [self.kind.raw(), self.flags]
            .into_iter()
            .chain(self.device.to_le_bytes())
            .chain(self.code.to_le_bytes())
            .chain(self.contact.to_le_bytes())
            .chain(self.value.to_le_bytes());
        let mut bytes = [0; RECORD_SIZE];
        for (slot, byte) in bytes.iter_mut().zip(fields) {
            *slot = byte;
        }
        bytes
    }

    /// Запись из раскладки.
    pub fn from_bytes(bytes: &[u8; RECORD_SIZE]) -> Self {
        let [
            kind,
            flags,
            d0,
            d1,
            k0,
            k1,
            c0,
            c1,
            v0,
            v1,
            v2,
            v3,
            v4,
            v5,
            v6,
            v7,
        ] = *bytes;
        Self {
            kind: Kind::from_raw(kind),
            flags,
            device: u16::from_le_bytes([d0, d1]),
            code: u16::from_le_bytes([k0, k1]),
            contact: u16::from_le_bytes([c0, c1]),
            value: i64::from_le_bytes([v0, v1, v2, v3, v4, v5, v6, v7]),
        }
    }
}

impl RecordBuf {
    /// Пустая последовательность.
    pub const fn new() -> Self {
        Self {
            records: Packed::new(),
        }
    }

    /// Дописывает запись в конец последовательности.
    ///
    /// Отказы:
    /// - `Full` - в буфере [`MESSAGE_RECORDS_MAX`] записей.
    pub fn push(&mut self, record: Record) -> Result<(), RecordsError> {
        self.push_bytes(&record.to_bytes())
    }

    /// Дописывает запись в раскладке `record` в конец последовательности.
    ///
    /// Отказы:
    /// - `Full` - в буфере [`MESSAGE_RECORDS_MAX`] записей.
    pub fn push_bytes(&mut self, record: &[u8; RECORD_SIZE]) -> Result<(), RecordsError> {
        if self.records.push(*record) {
            Ok(())
        } else {
            Err(RecordsError::Full)
        }
    }

    /// Число записей.
    pub const fn len(&self) -> usize {
        self.records.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.records.len() == 0
    }

    /// Удаляет все записи.
    pub fn clear(&mut self) {
        self.records.clear();
    }

    /// Раскладка последовательности.
    pub fn as_bytes(&self) -> &[u8] {
        self.records.as_bytes()
    }

    /// Записи последовательности по порядку.
    pub fn records(&self) -> Records<'_> {
        Records {
            chunks: self.records.items().iter(),
        }
    }
}

impl Default for RecordBuf {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a, R: ReceivedResources> WireValue<'a, R> for RecordBuf {
    type Owned = Self;

    fn from_field(data: &'a [u8], _frame: &mut R) -> Result<Self, IpcError> {
        let records = Packed::from_bytes(data).ok_or(IpcError::InvalidValue)?;
        Ok(Self { records })
    }
}

impl<R: ReceivedResources> IntoWireValue<R> for RecordBuf {
    type Read = Self;

    fn write_into(self, frame: &mut MessageBuf<R>, field_id: u8) -> Result<(), IpcError> {
        frame.write_field(field_id, self.as_bytes())
    }
}

impl WireTyped for RecordBuf {
    const WIRE_TYPE: WireType = WireType::BoundedBytes(MESSAGE_RECORDS_MAX * RECORD_SIZE);
}

impl<'a> Records<'a> {
    /// Записи раскладки `bytes`.
    ///
    /// Отказы:
    /// - `Misaligned` - длина `bytes` не кратна [`RECORD_SIZE`].
    pub fn read(bytes: &'a [u8]) -> Result<Self, RecordsError> {
        let (records, rest) = bytes.as_chunks();
        if !rest.is_empty() {
            return Err(RecordsError::Misaligned);
        }
        Ok(Self {
            chunks: records.iter(),
        })
    }
}

impl Iterator for Records<'_> {
    type Item = Record;

    fn next(&mut self) -> Option<Record> {
        self.chunks.next().map(Record::from_bytes)
    }
}
