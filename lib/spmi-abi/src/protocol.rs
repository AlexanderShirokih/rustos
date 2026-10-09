use syscall::kind;

use crate::error::{DeviceError, SpmiError};

ipc::protocol! {
    /// Шина устройств PMIC за арбитром SPMI.
    pub mod spmi {
        name = "Spmi";

        /// Выдаёт сессию первого свободного устройства за арбитром, в списке совместимостей
        /// которого есть `compatible`. Устройство занято до ухода клиента сессии.
        call fn open(
            compatible: &str,
        ) -> own Result<ipc::wire::Session<crate::spmi_device::Protocol>, SpmiError>;
    }
}

ipc::protocol! {
    /// Устройство PMIC, выданное сессии: блоки регистров из `reg` его узла и прерывания из
    /// `interrupts`.
    pub mod spmi_device {
        name = "SpmiDevice";

        /// Читает `count` байт регистров подряд с 16-битного адреса `address`. Байт регистра
        /// `address` - младший байт значения, старшие байты сверх `count` равны 0.
        ///
        /// Отказы:
        /// - `OutOfRange` - `count` вне 1..=8 либо байты не лежат в одном блоке устройства;
        /// - `Failed` - арбитр не выполнил команду.
        call fn read(address: u16, count: u8) -> Result<u64, DeviceError>;

        /// Записывает `count` младших байт `value` в регистры подряд с 16-битного адреса
        /// `address`, младший байт - в регистр `address`.
        ///
        /// Отказы:
        /// - `OutOfRange` - `count` вне 1..=8 либо байты не лежат в одном блоке устройства;
        /// - `Denied` - канал блока принадлежит другой среде исполнения;
        /// - `Failed` - арбитр не выполнил команду.
        call fn write(address: u16, count: u8, value: u64) -> Result<(), DeviceError>;

        /// Выдаёт копию `Signal` прерывания `resource` - номера записи в `interrupts`
        /// устройства - и разрешает прерывание. Срабатывание поднимает `SIGNALED` и запрещает
        /// прерывание до `ack`. Уход клиента сессии запрещает прерывание и отзывает каждую
        /// выданную ему копию.
        ///
        /// Отказы:
        /// - `NotDescribed` - в `interrupts` нет записи `resource`, её тип неверен либо
        ///   прерывание узла доставляет не этот арбитр;
        /// - `OutOfRange` - ведомый либо блок записи не принадлежат устройству либо бит записи
        ///   вне 0..=7;
        /// - `Denied` - канал блока принадлежит другой среде исполнения;
        /// - `Failed` - арбитр не выполнил команду;
        /// - `Unavailable` - шина не смогла создать либо скопировать `Signal`.
        call fn irq(
            resource: u16,
        ) -> own Result<ipc::wire::Cap<kind::Signal>, DeviceError>;

        /// Снимает защёлку прерывания `resource`, опускает `SIGNALED` его `Signal` и снова
        /// разрешает прерывание.
        ///
        /// Отказы:
        /// - `NotDescribed`, `OutOfRange`, `Denied` - как у `irq`;
        /// - `Unavailable` - прерывание не выдано сессии;
        /// - `Failed` - арбитр не выполнил команду.
        call fn ack(resource: u16) -> Result<(), DeviceError>;
    }
}
