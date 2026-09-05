//! Bootstrap-контракт: канал init процесса к ядру.

#![cfg_attr(not(test), no_std)]

#[cfg(test)]
extern crate std;

mod device_window;
mod log;

pub use device_window::DeviceWindow;

/// Граница тега источника в [`bootstrap::log`].
pub const LOG_TAG_MAX: usize = 64;

/// Граница строки [`bootstrap::log`]: остаток кадра за тегом.
pub const LOG_MESSAGE_MAX: usize =
    ipc::wire::FIELD_DATA_MAX - LOG_TAG_MAX - ipc::wire::FIELD_OVERHEAD;

ipc::wire_value! {
    /// Boot FDT: регион поверх страниц с блобом и его начало внутри региона.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct BootFdt {
        /// Read-only регион, целиком покрывающий блоб.
        pub region: ipc::wire::Cap,
        /// Смещение первого байта блоба от начала региона.
        pub offset: u32,
    }
}

ipc::protocol! {
    /// Канал init-процесса к ядру.
    pub mod bootstrap {
        name = "Bootstrap";

        /// Передаёт строку для логгера ядра klog. `tag` - источник строки
        /// (обычно имя процесса). Пустой тег - строка без тега.
        cast fn log(
            tag: ipc::wire::Str<{ LOG_TAG_MAX }>,
            message: ipc::wire::Str<{ LOG_MESSAGE_MAX }>,
        );

        /// Выдаёт копию корневого `IrqControl` в таблицу вызывателя (оригинал
        /// остаётся у ядра). `Err` - ненулевой код ошибки выдачи.
        call fn acquire_irq_control() -> Result<ipc::wire::Cap, u32>;

        /// Выдаёт вызывателю read-only capability на регион поверх байт userland-образа.
        call fn acquire_userland_image() -> Result<ipc::wire::Cap, u32>;

        /// Выдаёт вызывателю read-only capability на регион поверх байт boot FDT (DTB)
        /// вместе со смещением блоба внутри региона.
        call fn acquire_boot_fdt() -> Result<BootFdt, u32>;

        /// Выдаёт capability на Device-MMIO регион `index` (kernel-owned
        /// исключены). `Err` - индекс вне набора либо отказ выдачи.
        call fn acquire_device_memory(index: u32) -> Result<ipc::wire::Cap, u32>;
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU32;
    use std::{format, string::String, thread};

    use ipc::{
        Transport,
        wire::{Cap, Str},
    };
    use ipc_test::MockEnd;

    use super::{BootFdt, LOG_MESSAGE_MAX, LOG_TAG_MAX, bootstrap};

    #[derive(Default)]
    struct Sink {
        last_log: Option<String>,
    }

    impl bootstrap::Service for Sink {
        fn log(&mut self, tag: Str<'_, { LOG_TAG_MAX }>, message: Str<'_, { LOG_MESSAGE_MAX }>) {
            self.last_log = Some(format!("[{}] {}", tag.as_str(), message.as_str()));
        }

        fn acquire_irq_control(&mut self) -> Result<Cap, u32> {
            Ok(Cap::from_raw(NonZeroU32::new(0x77).expect("non-zero")))
        }

        fn acquire_userland_image(&mut self) -> Result<Cap, u32> {
            Ok(Cap::from_raw(NonZeroU32::new(0x88).expect("non-zero")))
        }

        fn acquire_boot_fdt(&mut self) -> Result<BootFdt, u32> {
            Ok(BootFdt {
                region: Cap::from_raw(NonZeroU32::new(0x99).expect("non-zero")),
                offset: 0x600,
            })
        }

        fn acquire_device_memory(&mut self, index: u32) -> Result<Cap, u32> {
            // Зеркалит сервер: за пределами набора - Err; иначе Cap, кодирующий индекс.
            if index >= 2 {
                return Err(7);
            }
            Ok(Cap::from_raw(
                NonZeroU32::new(0x90 + index).expect("non-zero"),
            ))
        }
    }

    #[test]
    fn log_round_trip() {
        let (client_end, server_end) = MockEnd::pair();
        let client = bootstrap::Client::new(client_end);
        client
            .log(
                Str::new("rootkeeper").expect("tag within bound"),
                Str::new("boot ok").expect("message within bound"),
            )
            .expect("log write");
        let mut sink = Sink::default();
        bootstrap::dispatch(&mut sink, &server_end).expect("dispatch ok");
        assert_eq!(sink.last_log.as_deref(), Some("[rootkeeper] boot ok"));
    }

    #[test]
    fn acquire_irq_control_round_trip() {
        let (client_end, server_end) = MockEnd::pair();
        let client = bootstrap::Client::new(client_end);
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut sink = Sink::default();
                server_end
                    .wait_readable(ipc::Timeout::INFINITE)
                    .expect("server wait");
                bootstrap::dispatch(&mut sink, &server_end).expect("dispatch ok");
            });
            let mut response = client.acquire_irq_control().expect("call ok");
            assert_eq!(
                response.value,
                Ok(Cap::from_raw(NonZeroU32::new(0x77).expect("non-zero")))
            );
            let cap = response.value.expect("IRQ control");
            assert_eq!(response.claim(cap), Some(cap));
        });
    }

    #[test]
    fn acquire_userland_image_round_trip() {
        let (client_end, server_end) = MockEnd::pair();
        let client = bootstrap::Client::new(client_end);
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut sink = Sink::default();
                server_end
                    .wait_readable(ipc::Timeout::INFINITE)
                    .expect("server wait");
                bootstrap::dispatch(&mut sink, &server_end).expect("dispatch ok");
            });
            let mut response = client.acquire_userland_image().expect("call ok");
            assert_eq!(
                response.value,
                Ok(Cap::from_raw(NonZeroU32::new(0x88).expect("non-zero")))
            );
            let cap = response.value.expect("userland image");
            assert_eq!(response.claim(cap), Some(cap));
        });
    }

    #[test]
    fn acquire_boot_fdt_round_trip() {
        let (client_end, server_end) = MockEnd::pair();
        let client = bootstrap::Client::new(client_end);
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut sink = Sink::default();
                server_end
                    .wait_readable(ipc::Timeout::INFINITE)
                    .expect("server wait");
                bootstrap::dispatch(&mut sink, &server_end).expect("dispatch ok");
            });
            let mut response = client.acquire_boot_fdt().expect("call ok");
            assert_eq!(
                response.value,
                Ok(BootFdt {
                    region: Cap::from_raw(NonZeroU32::new(0x99).expect("non-zero")),
                    offset: 0x600,
                })
            );
            let cap = response.value.expect("boot FDT").region;
            assert_eq!(response.claim(cap), Some(cap));
        });
    }

    #[test]
    fn acquire_device_memory_round_trip() {
        let (client_end, server_end) = MockEnd::pair();
        let client = bootstrap::Client::new(client_end);
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut sink = Sink::default();
                server_end
                    .wait_readable(ipc::Timeout::INFINITE)
                    .expect("server wait");
                bootstrap::dispatch(&mut sink, &server_end).expect("dispatch ok");
            });
            // Аргумент index доезжает до сервера и влияет на ответный Cap.
            let mut response = client.acquire_device_memory(1).expect("call ok");
            assert_eq!(
                response.value,
                Ok(Cap::from_raw(NonZeroU32::new(0x91).expect("non-zero")))
            );
            let cap = response.value.expect("device memory");
            assert_eq!(response.claim(cap), Some(cap));
        });
    }

    #[test]
    fn acquire_device_memory_out_of_range_returns_err() {
        let (client_end, server_end) = MockEnd::pair();
        let client = bootstrap::Client::new(client_end);
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut sink = Sink::default();
                server_end
                    .wait_readable(ipc::Timeout::INFINITE)
                    .expect("server wait");
                bootstrap::dispatch(&mut sink, &server_end).expect("dispatch ok");
            });
            let result = client.acquire_device_memory(9).expect("call ok");
            assert_eq!(result.value, Err(7));
        });
    }
}
