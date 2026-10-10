//! Модель событий ввода: идентичность клавиш и осей, дескриптор устройства, записи событий, команда
//! обновления устройства и трекер его состояния.
//!
//! Событие передаётся записью [`Record`] размером [`RECORD_SIZE`] байт. Кадр - записи одного
//! устройства, закрытые записью `Frame`. Последовательность записей передаётся одним полем
//! сообщения, и запись неизвестного вида читается как пропускаемая.
//!
//! Устройство объявляет [`DeviceDescriptor`]: клавиши, оси устройства и наибольшее число
//! контактов с их осями. Клавиша и ось задаются [`Usage`] из HID Usage Tables, а запись события
//! ссылается на них индексом в списке дескриптора. Абсолютная ось хранит значение в диапазоне,
//! относительная передаёт приращение. [`Tracker`] держит состояние устройства, применяет к нему
//! [`Command`] целиком либо отклоняет её без последствий, строит снимок состояния и отменяет его.
//!
//! Протокол [`input_inject`] принимает от источников дескрипторы устройств и их команды, протокол
//! [`input`] отдаёт потребителю очередь кадров всех устройств и дескриптор устройства по номеру.
//! Протокол [`input_source`] передаёт источнику сессию `InputInject`.
#![cfg_attr(not(test), no_std)]

mod command;
mod descriptor;
mod linux;
mod packed;
mod protocol;
mod record;
mod tracker;
mod usage;

pub use command::{COMMAND_RECORDS_MAX, Command};
pub use descriptor::{
    AXES_MAX, AXIS_DECL_SIZE, AxisDecl, AxisList, AxisMode, Binding, CONTACTS_MAX, DEVICE_NAME_MAX,
    DescriptorError, DeviceDescriptor, DeviceLayout, KEYS_MAX, KeyList,
};
pub use protocol::{
    AttachError, DeclareError, DescribeError, NextError, UpdateError, input, input_inject,
    input_source,
};
pub use record::{
    CANCEL, Kind, MESSAGE_RECORDS_MAX, NO_CONTACT, RECORD_SIZE, Record, RecordBuf, Records,
    RecordsError,
};
pub use tracker::{CommandError, Tracker};
pub use usage::Usage;
