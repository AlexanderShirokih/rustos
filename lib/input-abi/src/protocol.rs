//! Протоколы хаба ввода [`input_inject`] для источников, [`input`] для потребителей и
//! [`input_focus`] для держателя фокуса, протокол источника [`input_source`].

use ipc::{WireTyped, schema::max_body_len, wire::BODY_MAX};

use crate::{
    descriptor::{DescriptorError, DeviceDescriptor, DeviceLayout},
    number::{DeviceNumber, FocusNumber},
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
        /// Устройство не объявлено либо не видно сессии.
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

ipc::wire_enum! {
    /// Отказ операции держателя фокуса.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum FocusError {
        /// Фокусом владеет другая сессия `InputFocus`.
        Busy = 1,
        /// Достигнут предел сессий `Input` либо исчерпаны память очереди или номера сессий фокуса.
        NoSpace = 2,
        /// Номер не принадлежит открытой сессии фокуса.
        UnknownSession = 3,
        /// Номер не принадлежит объявленному устройству.
        UnknownDevice = 4,
    }
}

ipc::wire_value! {
    /// Сессия `Input`, открытая держателю фокуса, и её номер для [`input_focus`]`::focus`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct FocusSession<S = ipc::wire::Session<crate::input::Protocol>> {
        /// Сессия `Input`, которую держатель передаёт приложению.
        pub session: S,
        /// Номер сессии.
        pub number: FocusNumber,
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

        /// Проверяет дескриптор и выдаёт устройству номер.
        call fn declare(descriptor: DeviceDescriptor) -> Result<DeviceNumber, DeclareError>;

        /// Применяет команду из записей `records` к устройству `device` и добавляет её кадр в
        /// очередь каждой сессии `Input`, которой видно устройство. Команда без времени `time`
        /// получает время приёма. Ответ не ждёт чтения кадра потребителями.
        call fn update(
            device: DeviceNumber,
            time: Option<u64>,
            records: &[u8],
        ) -> Result<(), UpdateError>;
    }
}

ipc::protocol! {
    /// Протокол потребителя ввода отдаёт очередь кадров видимых устройств и их дескрипторы.
    ///
    /// Обычной сессии видно каждое устройство. Сессии из [`input_focus`]`::open` устройства
    /// видны, только пока она в фокусе, а зарезервированные не видны никогда.
    ///
    /// Очередь сессии начинается с записи `Resync`, за которой для каждого видимого устройства в
    /// порядке номеров идут `DeviceAdded` и снимок. Такая последовательность по текущему
    /// состоянию заменяет очередь, которая не вмещает новый кадр, и очередь сессии, у которой
    /// сменился фокус. Зарезервированное устройство уходит из очереди сессии фокуса записью
    /// `DeviceRemoved`.
    pub mod input {
        name = "Input";

        /// Отдаёт записи из начала очереди: целые кадры и одиночные записи состава устройств,
        /// сколько их помещается в ответ. Пустая очередь задерживает ответ до первой записи.
        call async fn next() -> Result<RecordBuf, NextError>;

        /// Отдаёт дескриптор устройства `device`, видимого сессии.
        call fn describe(device: DeviceNumber) -> Result<DeviceLayout, DescribeError>;
    }
}

ipc::protocol! {
    /// Протокол держателя фокуса открывает сессии `Input` приложений, выбирает сессию в фокусе
    /// и скрывает от сессий фокуса устройства, которые держатель оставляет себе.
    ///
    /// Держатель один. Операции другой сессии `InputFocus` получают `Busy`, пока держатель не
    /// закроет свою. Закрытие сессии держателя закрывает его сессии фокуса и снимает резервы.
    pub mod input_focus {
        name = "InputFocus";

        /// Открывает сессию `Input` вне фокуса.
        call fn open() -> own Result<FocusSession, FocusError>;

        /// Передаёт фокус сессии с номером `session` либо снимает фокус при `None`.
        call fn focus(session: Option<FocusNumber>) -> Result<(), FocusError>;

        /// Скрывает устройство `device` от сессий фокуса до его ухода.
        call fn reserve(device: DeviceNumber) -> Result<(), FocusError>;
    }
}

ipc::protocol! {
    /// Источник ввода. Источник объявляет свои устройства по переданной ему сессии
    /// `InputInject`.
    pub mod input_source {
        name = "InputSource";

        /// Передаёт источнику сессию `inject`. Источник объявляет по ней свои устройства и
        /// держит её, пока обслуживает их.
        call async fn attach(
            own inject: ipc::wire::Session<input_inject::Protocol>,
        ) -> Result<(), AttachError>;
    }
}
