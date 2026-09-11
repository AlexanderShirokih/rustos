//! Экспорт `device-forwarding-fixture` с копиями её линии прерывания и MMIO-окна.

// Модуль подключён к прогону и к фикстуре, и каждый бинарь пользуется своим подмножеством.
#![allow(dead_code)]

use ipc::wire::Cap;
use syscall::kind;

pub const FORWARDED_EXIT_CODE: u64 = 6;

ipc::wire_value! {
    #[derive(Debug)]
    pub struct Forwarded<L = Cap<kind::IrqLine>, W = Cap<kind::MemoryRegion>> {
        pub line: L,
        pub window: W,
    }
}

ipc::protocol! {
    pub mod device_forwarding {
        name = "DeviceForwarding";

        call fn forward() -> own Forwarded;
    }
}
