// Протоколы общие с фикстурой, каждый бинарь использует свою сторону.
#![allow(dead_code)]

ipc::protocol! {
    pub mod slow_bus {
        name = "AsyncComponentBus";
        call async fn wait() -> u64;
    }
}

ipc::protocol! {
    pub mod async_probe {
        name = "AsyncComponentProbe";
        call async fn slow(own bus: ipc::wire::Session<super::slow_bus::Protocol>) -> Result<u64, u8>;
        call fn quick() -> u32;
        call fn active() -> u32;
    }
}
