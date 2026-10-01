use syscall::kind;

use crate::{error::PlatformError, window::MmioWindow};

ipc::protocol! {
    /// Ресурсы устройств совместимости, которую называет область сессии.
    pub mod platform {
        name = "Platform";

        /// Выдаёт копию линии прерывания `index` устройства `instance`.
        call fn irq(
            instance: u16,
            index: u16,
        ) -> own Result<ipc::wire::Cap<kind::IrqLine>, PlatformError>;

        /// Выдаёт окно регистров `index` устройства `instance`: копию региона на запись и
        /// смещение регистров в нём. Окна устройств сессии в одном регионе приходят этим
        /// регионом, каждое со своим смещением.
        call fn mmio(instance: u16, index: u16) -> own Result<MmioWindow, PlatformError>;
    }
}
