use platform_abi::DeviceId;

use crate::error::ClocksError;

ipc::protocol! {
    /// Такты узлов устройств, которыми управляет контроллер тактирования.
    pub mod clocks {
        name = "Clocks";

        /// Включает такты узла `device` у этого контроллера и занимает узел сессией до ухода
        /// её клиента.
        ///
        /// Отказы:
        /// - `NoDevice` - узла нет, он недоступен либо не называет тактов контроллера;
        /// - `Busy` - узел занят другой сессией;
        /// - `Unsupported` - запись тактов повреждена либо такт вне таблицы;
        /// - `Timeout` - ветвь не сообщила о работе.
        call fn enable(device: DeviceId) -> Result<(), ClocksError>;

        /// Задаёт частоту `hz` такта с именем `name` узла `device` этой сессии.
        ///
        /// Отказы:
        /// - `NoDevice` - узел не занят этой сессией;
        /// - `Unsupported` - имени, генератора либо частоты нет в таблице;
        /// - `Timeout` - генератор не применил настройку.
        call fn set_rate(device: DeviceId, name: &str, hz: u64) -> Result<(), ClocksError>;
    }
}
