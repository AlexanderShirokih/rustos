use syscall::kind;

use crate::{
    description::DescriptionRegion, device::DEVICE_ID_MAX, error::PlatformError, window::MmioWindow,
};

ipc::protocol! {
    /// Описание платформы и ресурсы её устройств.
    pub mod platform {
        name = "Platform";

        /// Выдаёт копию линии прерывания `resource` устройства `device`. `device` - путь
        /// [`DeviceId`](crate::DeviceId), `resource` - номер спецификатора в `interrupts`
        /// устройства.
        call fn irq(
            device: ipc::wire::Str<{ DEVICE_ID_MAX }>,
            resource: u16,
        ) -> own Result<ipc::wire::Cap<kind::IrqLine>, PlatformError>;

        /// Выдаёт окно регистров `resource` устройства `device`: копию региона на запись,
        /// смещение и размер окна. `resource` - номер записи в `reg` устройства. Окна устройств
        /// сессии в одном регионе приходят этим регионом, каждое со своим смещением.
        call fn mmio(
            device: ipc::wire::Str<{ DEVICE_ID_MAX }>,
            resource: u16,
        ) -> own Result<MmioWindow, PlatformError>;

        /// Выдаёт описание платформы: копию региона только для чтения и смещение начала
        /// описания в нём. Повтор в той же сессии выдаёт копию того же региона.
        call fn description() -> own Result<DescriptionRegion, PlatformError>;
    }
}
