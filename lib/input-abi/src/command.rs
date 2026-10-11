//! Построитель команды обновления устройства.

use crate::record::{MESSAGE_RECORDS_MAX, NO_CONTACT, Record, RecordBuf, RecordsError};

/// Наибольшее число записей команды. Кадр команды с записью `Frame` помещается в сообщение.
pub const COMMAND_RECORDS_MAX: usize = MESSAGE_RECORDS_MAX - 1;

/// Команда обновления одного устройства: записи `Key`, `Axis`, `ContactDown` и `ContactUp` с
/// нулевым полем устройства.
///
/// Каждая операция дописывает одну запись. Отказы операции:
/// - `Full` - в команде [`COMMAND_RECORDS_MAX`] записей.
#[derive(Clone, Default)]
pub struct Command {
    records: RecordBuf,
}

impl Command {
    /// Пустая команда.
    pub const fn new() -> Self {
        Self {
            records: RecordBuf::new(),
        }
    }

    /// Нажатие клавиши с индексом `key`.
    pub fn press(&mut self, key: u16) -> Result<&mut Self, RecordsError> {
        self.push(Record::key(None, key, true))
    }

    /// Отпускание клавиши с индексом `key`.
    pub fn release(&mut self, key: u16) -> Result<&mut Self, RecordsError> {
        self.push(Record::key(None, key, false))
    }

    /// Значение либо приращение оси устройства с индексом `axis`.
    pub fn axis(&mut self, axis: u16, value: i64) -> Result<&mut Self, RecordsError> {
        self.push(Record::axis(None, axis, NO_CONTACT, value))
    }

    /// Начало касания контактом `contact`.
    pub fn down(&mut self, contact: u16) -> Result<&mut Self, RecordsError> {
        self.push(Record::contact_down(None, contact))
    }

    /// Значение оси с индексом `axis` у контакта `contact`.
    pub fn contact_axis(
        &mut self,
        contact: u16,
        axis: u16,
        value: i64,
    ) -> Result<&mut Self, RecordsError> {
        self.push(Record::axis(None, axis, contact, value))
    }

    /// Конец касания контактом `contact`.
    pub fn up(&mut self, contact: u16) -> Result<&mut Self, RecordsError> {
        self.push(Record::contact_up(None, contact, false))
    }

    /// Отмена касания контактом `contact` вместе с жестом.
    pub fn cancel(&mut self, contact: u16) -> Result<&mut Self, RecordsError> {
        self.push(Record::contact_up(None, contact, true))
    }

    /// Раскладка записей команды.
    pub fn as_bytes(&self) -> &[u8] {
        self.records.as_bytes()
    }

    fn push(&mut self, record: Record) -> Result<&mut Self, RecordsError> {
        if self.records.len() >= COMMAND_RECORDS_MAX {
            return Err(RecordsError::Full);
        }

        self.records.push(record)?;
        Ok(self)
    }
}
