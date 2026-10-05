use syscall::kind;

use crate::error::{SlotError, VirtioMmioError};

ipc::protocol! {
    /// Шина транспортов virtio-mmio машины.
    pub mod virtio_mmio {
        name = "VirtioMmio";

        /// Выдаёт сессию слота первого свободного транспорта, регистр DeviceID которого равен
        /// ненулевому `device_type`. Транспорт занят до ухода клиента сессии.
        ///
        /// Отказы:
        /// - `NoDevice` - `device_type` равен 0 либо транспорта с таким DeviceID нет;
        /// - `Busy` - каждый транспорт с таким DeviceID занят;
        /// - `NoSpace` - шине не хватает ресурса на сессию слота.
        call fn open(
            device_type: u32,
        ) -> own Result<ipc::wire::Session<crate::virtio_slot::Protocol>, VirtioMmioError>;
    }
}

ipc::protocol! {
    /// Транспорт virtio-mmio, выданный сессии слота.
    pub mod virtio_slot {
        name = "VirtioSlot";

        /// Читает 32-битный регистр окна транспорта по смещению `offset`.
        ///
        /// Отказы:
        /// - `OutOfRange` - регистр выходит за окно либо `offset` не кратно 4.
        call fn read(offset: u16) -> Result<u32, SlotError>;

        /// Записывает 32-битный регистр окна транспорта по смещению `offset` после барьера
        /// памяти: запись клиента в общую с устройством память, сделанная до запроса, видна
        /// устройству раньше записи регистра.
        ///
        /// Отказы:
        /// - `OutOfRange` - регистр выходит за окно либо `offset` не кратно 4.
        call fn write(offset: u16, value: u32) -> Result<(), SlotError>;

        /// Выдаёт копию линии прерывания транспорта. Уход клиента сессии отзывает каждую
        /// выданную ему копию.
        ///
        /// Отказы:
        /// - `Unavailable` - шина не смогла скопировать линию.
        call fn irq() -> own Result<ipc::wire::Cap<kind::IrqLine>, SlotError>;
    }
}
