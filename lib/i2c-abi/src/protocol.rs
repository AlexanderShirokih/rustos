use crate::{
    buffer::TransferBuf,
    error::{I2cError, TransferError},
};

ipc::protocol! {
    /// Шина устройств I2C за контроллером.
    pub mod i2c {
        name = "I2c";

        /// Выдаёт сессию устройства за контроллером с узлом `device`. Адрес устройства задаёт
        /// описание. Устройство занято до ухода клиента сессии.
        ///
        /// Отказы:
        /// - `NoDevice` - путь не называет устройство за контроллером либо контроллер не готов
        ///   к работе;
        /// - `Busy` - устройство занято сессией;
        /// - `Unsupported` - шина не работает с адресом устройства;
        /// - `NoSpace` - шине не хватает ресурса на сессию устройства.
        call fn open(
            device: platform_abi::DeviceId,
        ) -> own Result<ipc::wire::Session<crate::i2c_device::Protocol>, I2cError>;
    }
}

ipc::protocol! {
    /// Устройство I2C, выданное сессии. Передача отвечает после её завершения на шине.
    pub mod i2c_device {
        name = "I2cDevice";

        /// Передаёт устройству байты `data` одним сообщением с остановкой в конце.
        ///
        /// Отказы:
        /// - `Length` - `data` пусто либо длиннее, чем контроллер передаёт за раз;
        /// - `NoAck` - устройство не подтвердило адрес либо байт;
        /// - `Bus` - контроллер сообщил об ошибке шины;
        /// - `Timeout` - контроллер не завершил передачу за отведённое время.
        call fn write(data: &[u8]) -> Result<(), TransferError>;

        /// Читает `count` байт устройства одним сообщением с остановкой в конце.
        ///
        /// Отказы:
        /// - `Length` - `count` равно 0 либо больше, чем контроллер принимает за раз;
        /// - `NoAck`, `Bus`, `Timeout` - как у `write`.
        call fn read(count: u16) -> Result<TransferBuf, TransferError>;

        /// Передаёт устройству байты `data`, затем повторным стартом читает у него `count` байт.
        /// Остановка на шине следует только после чтения.
        ///
        /// Отказы:
        /// - `Length` - `data` или `count` нарушают пределы `write` и `read`;
        /// - `NoAck`, `Bus`, `Timeout` - как у `write`.
        call fn write_read(data: &[u8], count: u16) -> Result<TransferBuf, TransferError>;
    }
}
