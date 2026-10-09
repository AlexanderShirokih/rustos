//! Протоколы хаба ввода [`input_inject`] для источников и [`input`] для потребителей, протокол
//! источника [`input_source`].

use ipc::{WireTyped, schema::max_body_len, wire::BODY_MAX};

use crate::{
    descriptor::{DescriptorError, DeviceDescriptor, DeviceLayout},
    record::RecordBuf,
    tracker::CommandError,
};

ipc::wire_enum! {
    /// Отказ объявления устройства.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum DeclareError {
        /// Список клавиш или объявление оси неверно, см. [`DescriptorError::Malformed`].
        Malformed = 1,
        /// Части дескриптора противоречат друг другу, см. [`DescriptorError::Inconsistent`].
        Inconsistent = 2,
        /// Дескриптор больше пределов модели, см. [`DescriptorError::TooLarge`].
        TooLarge = 3,
        /// Хаб не вмещает устройство:
        /// - ёмкость очереди сессии `Input` не вмещает последовательность начала сессии вместе
        ///   с новым устройством;
        /// - номера устройств исчерпаны.
        NoSpace = 4,
    }
}

ipc::wire_enum! {
    /// Отказ команды устройства. Состояние устройства после отказа не меняется.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum UpdateError {
        /// Устройство с таким номером не объявлено этой сессией.
        UnknownDevice = 1,
        /// Команда закодирована неверно, см. [`CommandError::Malformed`].
        Malformed = 2,
        /// Запись выходит за дескриптор, см. [`CommandError::Undeclared`].
        Undeclared = 3,
        /// Запись противоречит состоянию устройства или другой записи команды, см.
        /// [`CommandError::Conflict`].
        Conflict = 4,
    }
}

ipc::wire_enum! {
    /// Отказ чтения очереди.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum NextError {
        /// Предыдущий `next` сессии ещё ждёт ответа.
        Busy = 1,
    }
}

ipc::wire_enum! {
    /// Отказ выдачи дескриптора.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum DescribeError {
        /// Номер не принадлежит объявленному устройству.
        UnknownDevice = 1,
    }
}

ipc::wire_enum! {
    /// Отказ подключения источника.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum AttachError {
        /// Источник уже подключён.
        Busy = 1,
        /// Источник не может обслуживать свои устройства.
        Refused = 2,
    }
}

impl From<DescriptorError> for DeclareError {
    fn from(error: DescriptorError) -> Self {
        match error {
            DescriptorError::Malformed => Self::Malformed,
            DescriptorError::Inconsistent => Self::Inconsistent,
            DescriptorError::TooLarge => Self::TooLarge,
        }
    }
}

impl From<CommandError> for UpdateError {
    fn from(error: CommandError) -> Self {
        match error {
            CommandError::Malformed => Self::Malformed,
            CommandError::Undeclared => Self::Undeclared,
            CommandError::Conflict => Self::Conflict,
        }
    }
}

const _: () = assert!(
    max_body_len(&[<Result<RecordBuf, NextError> as WireTyped>::WIRE_TYPE]) <= BODY_MAX,
    "next response frame exceeds the body budget"
);

const _: () = assert!(
    max_body_len(&[<Result<DeviceLayout, DescribeError> as WireTyped>::WIRE_TYPE]) <= BODY_MAX,
    "describe response frame exceeds the body budget"
);

ipc::protocol! {
    /// Источник ввода: объявление устройств и их команды. Устройство принадлежит сессии,
    /// которая его объявила, и удаляется с её закрытием.
    pub mod input_inject {
        name = "InputInject";

        /// Проверяет дескриптор и выдаёт устройству номер. Номера начинаются с 1 и не
        /// повторяются.
        call fn declare(descriptor: DeviceDescriptor) -> Result<u16, DeclareError>;

        /// Применяет команду из записей `records` к устройству `device` и добавляет её кадр в
        /// очередь каждой сессии `Input`. Команда без времени `time` получает время приёма.
        /// Ответ не ждёт чтения кадра потребителями.
        call fn update(
            device: u16,
            time: Option<u64>,
            records: &[u8],
        ) -> Result<(), UpdateError>;
    }
}

ipc::protocol! {
    /// Потребитель ввода: очередь кадров всех устройств и их дескрипторы.
    ///
    /// Очередь сессии начинается с записи `Resync`, за которой для каждого устройства в порядке
    /// номеров идут `DeviceAdded` и снимок. Очередь, которая не вмещает новый кадр, заменяется
    /// такой же последовательностью по текущему состоянию устройств.
    pub mod input {
        name = "Input";

        /// Отдаёт записи из начала очереди: целые кадры и одиночные записи состава устройств,
        /// сколько их помещается в ответ. Пустая очередь задерживает ответ до первой записи.
        call(defer) fn next() -> Result<RecordBuf, NextError>;

        /// Отдаёт дескриптор объявленного устройства `device`.
        call fn describe(device: u16) -> Result<DeviceLayout, DescribeError>;
    }
}

ipc::protocol! {
    /// Источник ввода. Источник объявляет свои устройства по переданной ему сессии
    /// `InputInject`.
    pub mod input_source {
        name = "InputSource";

        /// Передаёт источнику сессию `inject`. Источник объявляет по ней свои устройства и
        /// держит её, пока обслуживает их.
        call fn attach(
            own inject: ipc::wire::Session<input_inject::Protocol>,
        ) -> Result<(), AttachError>;
    }
}
