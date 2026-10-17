//! Преобразует отчёты NT36672A в команды хаба ввода и завершает касания контактов,
//! отсутствующих в очередном отчёте, как `INPUT_MT_DROP_UNUSED` в Linux.

use input_abi::{Command, ContactTable, RecordsError};

use crate::device::Device;

/// Адрес начала отчёта касаний.
pub const REPORT_ADDRESS: u8 = 0x00;
const RECORD_SIZE: u16 = 6;
/// Индекс оси `X` контакта в дескрипторе.
pub const AXIS_X: u16 = 0;
/// Индекс оси `Y` контакта в дескрипторе.
pub const AXIS_Y: u16 = 1;

const RECORD_INVALID: u8 = 0xff;
const SLOT_SHIFT: u8 = 3;
const STATE_MASK: u8 = 0x07;
const STATE_NEW: u8 = 1;
const STATE_UPDATE: u8 = 2;
const STATE_RELEASE: u8 = 3;

/// Состояние контактов тачскрина, которое обновляется при преобразовании отчётов в команды.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contacts {
    limits: Limits,
    table: ContactTable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Limits {
    contacts: u16,
    x: i32,
    y: i32,
}

#[derive(Clone, Copy)]
struct Touch {
    contact: u16,
    active: bool,
    x: i32,
    y: i32,
}

impl Contacts {
    /// Создаёт состояние устройства `device` без активных касаний.
    pub fn new(device: &Device) -> Self {
        let contacts = device.max_contacts();
        Self {
            limits: Limits {
                contacts,
                x: device.max_x(),
                y: device.max_y(),
            },
            table: ContactTable::new(contacts, 2),
        }
    }

    /// Длина полного отчёта в байтах, по 6 байт на каждый контакт устройства.
    pub fn report_length(&self) -> u16 {
        self.limits.contacts.saturating_mul(RECORD_SIZE)
    }

    /// Преобразует отчёт `report` в команду хаба и обновляет состояние контактов.
    ///
    /// Для чтения полного отчёта используйте длину [`Self::report_length`]. Метод
    /// обрабатывает целые записи по 6 байт в пределах числа контактов устройства. Неполная
    /// последняя запись и записи сверх этого предела пропускаются. Длина не проверяется.
    ///
    /// Запись с байтом 0 равным 0xff, слотом вне `1..=число контактов` либо состоянием кроме
    /// 1, 2 и 3 пропускается. Первая допустимая запись слота определяет его состояние,
    /// последующие записи того же слота пропускаются. Состояния 1 и 2 начинают касание
    /// свободного контакта и передают координаты, зажатые в диапазон осей. Для занятого
    /// контакта они передают только изменившиеся координаты. Состояние 3 завершает касание занятого
    /// контакта и пропускается для свободного.
    ///
    /// Занятый контакт, отсутствующий среди обработанных записей, завершает касание, в том
    /// числе при коротком или пустом входе. Пустая команда означает, что контакты не изменились.
    ///
    /// Отказы:
    /// - `Full` - записи не помещаются в команду.
    pub fn translate(&mut self, report: &[u8]) -> Result<Command, RecordsError> {
        let mut seen = 0u16;
        let limits = self.limits;

        for touch in report
            .chunks_exact(usize::from(RECORD_SIZE))
            .take(usize::from(limits.contacts))
            .filter_map(|record| limits.touch(record))
        {
            let bit = bit(touch.contact);
            if seen & bit != 0 {
                continue;
            }
            seen |= bit;

            if touch.active {
                self.table.axis(touch.contact, AXIS_X, touch.x);
                self.table.axis(touch.contact, AXIS_Y, touch.y);
                self.table.touch(touch.contact);
            } else {
                self.table.lift(touch.contact);
            }
        }

        for contact in 0..limits.contacts {
            if seen & bit(contact) == 0 {
                self.table.lift(contact);
            }
        }

        let mut command = Command::new();
        // Отчёт не перезапускает касания, и второй команды таблица не даёт.
        self.table.finish(&mut command)?;
        Ok(command)
    }
}

impl Limits {
    fn touch(&self, record: &[u8]) -> Option<Touch> {
        let &[head, high_x, high_y, low, ..] = record else {
            return None;
        };
        if head == RECORD_INVALID {
            return None;
        }

        let slot = u16::from(head >> SLOT_SHIFT);
        if slot == 0 || slot > self.contacts {
            return None;
        }
        let active = match head & STATE_MASK {
            STATE_NEW | STATE_UPDATE => true,
            STATE_RELEASE => false,
            _ => return None,
        };

        let x = i32::from(high_x) << 4 | i32::from(low >> 4);
        let y = i32::from(high_y) << 4 | i32::from(low & 0x0f);
        Some(Touch {
            contact: slot - 1,
            active,
            x: x.clamp(0, self.x),
            y: y.clamp(0, self.y),
        })
    }
}

fn bit(contact: u16) -> u16 {
    1u16.checked_shl(u32::from(contact)).unwrap_or_default()
}
