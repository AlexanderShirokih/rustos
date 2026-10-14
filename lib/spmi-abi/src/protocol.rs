use syscall::kind;

use crate::{
    error::{DeviceError, SpmiError},
    trigger::Trigger,
};

ipc::protocol! {
    /// Шина устройств PMIC за арбитром SPMI.
    pub mod spmi {
        name = "Spmi";

        /// Выдаёт сессию устройства за арбитром с узлом `device`. Устройство занято до ухода
        /// клиента сессии.
        ///
        /// Отказы:
        /// - `NoDevice` - путь не называет устройство за арбитром либо шина не работает с
        ///   арбитром;
        /// - `Busy` - устройство занято сессией другого клиента;
        /// - `NoSpace` - шине не хватает ресурса на сессию устройства.
        call fn open(
            device: platform_abi::DeviceId,
        ) -> own Result<ipc::wire::Session<crate::spmi_device::Protocol>, SpmiError>;

        /// Выдаёт сессию устройства `device`, как `open`, с `count` блоками подряд с блока
        /// записи `reg` узла устройства. Число блоков устройства, у которого одна запись `reg`
        /// на все блоки, называет его драйвер.
        ///
        /// Отказы:
        /// - `NoDevice`, `Busy`, `NoSpace` - как у `open`;
        /// - `Range` - у узла нет своего `reg` либо в нём больше одной записи, `count` равен 0,
        ///   последний блок выше 0xFF либо блок после первого принадлежит другому устройству
        ///   того же ведомого.
        call fn open_blocks(
            device: platform_abi::DeviceId,
            count: u8,
        ) -> own Result<ipc::wire::Session<crate::spmi_device::Protocol>, SpmiError>;
    }
}

ipc::protocol! {
    /// Устройство PMIC, выданное сессии: блоки регистров из `reg` его узла либо блоки
    /// `open_blocks`, прерывания из `interrupts` и прерывания битов блоков.
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

        /// Заменяет биты `mask` регистра `address` битами `value`, остальные биты регистра
        /// сохраняются. Регистр записывается, только если его значение меняется. Между
        /// чтением и записью шина не выполняет команд других сессий.
        ///
        /// Отказы:
        /// - `OutOfRange` - регистр не лежит в блоке устройства;
        /// - `Denied` - значение меняется, а канал блока принадлежит другой среде исполнения;
        /// - `Failed` - арбитр не выполнил команду.
        call fn update(address: u16, mask: u8, value: u8) -> Result<(), DeviceError>;

        /// Выдаёт сессию `SpmiInterrupt` прерывания `resource` - номера записи в `interrupts`
        /// устройства - и разрешает прерывание. Срабатывание поднимает `SIGNALED` его `Signal`
        /// и запрещает прерывание до `ack` сессии прерывания. Уход клиента сессии прерывания
        /// либо сессии устройства запрещает прерывание и отзывает каждую копию его `Signal`.
        ///
        /// Отказы:
        /// - `NotDescribed` - в `interrupts` нет записи `resource`, её тип неверен либо
        ///   прерывание узла доставляет не этот арбитр;
        /// - `OutOfRange` - ведомый либо блок записи не принадлежат устройству либо бит записи
        ///   вне 0..=7;
        /// - `Busy` - прерывание выдано открытой сессии прерывания устройства;
        /// - `Denied` - канал блока принадлежит другой среде исполнения;
        /// - `Failed` - арбитр не выполнил команду;
        /// - `Unavailable` - шина не смогла создать `Signal` либо сессию прерывания.
        call fn irq(
            resource: u16,
        ) -> own Result<ipc::wire::Session<crate::spmi_interrupt::Protocol>, DeviceError>;

        /// Выдаёт сессию `SpmiInterrupt` прерывания бита `bit` блока `block` устройства с
        /// условием `trigger` и разрешает прерывание, как `irq`. Запись `interrupts` того же
        /// бита того же блока называет то же прерывание.
        ///
        /// Отказы:
        /// - `OutOfRange` - блок не принадлежит устройству либо бит вне 0..=7;
        /// - `Busy`, `Denied`, `Failed`, `Unavailable` - как у `irq`.
        call fn block_irq(
            block: u8,
            bit: u8,
            trigger: Trigger,
        ) -> own Result<ipc::wire::Session<crate::spmi_interrupt::Protocol>, DeviceError>;
    }
}

ipc::protocol! {
    /// Прерывание устройства PMIC, выданное сессией `SpmiDevice`.
    pub mod spmi_interrupt {
        name = "SpmiInterrupt";

        /// Выдаёт копию `Signal` прерывания. Повторный вызов выдаёт копию того же `Signal`.
        ///
        /// Отказы:
        /// - `Unavailable` - шина не смогла скопировать `Signal` либо клиент сессии устройства
        ///   ушёл.
        call fn signal() -> own Result<ipc::wire::Cap<kind::Signal>, DeviceError>;

        /// Снимает защёлку прерывания, опускает `SIGNALED` его `Signal` и снова разрешает
        /// прерывание.
        ///
        /// Отказы:
        /// - `Failed` - арбитр не выполнил команду;
        /// - `Unavailable` - шина не смогла опустить `SIGNALED` либо клиент сессии устройства
        ///   ушёл.
        call fn ack() -> Result<(), DeviceError>;
    }
}
