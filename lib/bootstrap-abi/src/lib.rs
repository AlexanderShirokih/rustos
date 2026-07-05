//! Bootstrap-контракт: канал init процесса к ядру.

#![cfg_attr(not(test), no_std)]

#[cfg(test)]
extern crate std;

mod device_window;
mod log;

pub use device_window::DeviceWindow;

/// Граница тега источника в [`Bootstrap::log`].
pub const LOG_TAG_MAX: usize = 32;

/// Граница строки [`Bootstrap::log`]: остаток кадра за тегом.
pub const LOG_MESSAGE_MAX: usize =
    ipc::wire::FIELD_DATA_MAX - LOG_TAG_MAX - ipc::wire::FIELD_OVERHEAD;

#[ipc::protocol(name = "Bootstrap")]
pub trait Bootstrap {
    /// Передаёт строку для логгера ядра klog. `tag` - источник строки
    /// (обычно имя процесса); пустой тег - строка без тега.
    #[cast]
    fn log(
        &self,
        tag: ipc::wire::Str<{ LOG_TAG_MAX }>,
        message: ipc::wire::Str<{ LOG_MESSAGE_MAX }>,
    );

    /// Выдаёт копию корневого `IrqControl` в таблицу вызывателя (оригинал
    /// остаётся у ядра). `Err` - ненулевой код ошибки выдачи.
    #[call]
    fn acquire_irq_control(&self) -> Result<ipc::wire::Cap, u32>;

    /// Выдаёт вызывателю read-only capability на регион поверх байт userland-образа.
    #[call]
    fn acquire_userland_image(&self) -> Result<ipc::wire::Cap, u32>;

    /// Выдаёт вызывателю read-only capability на регион поверх байт boot FDT (DTB).
    #[call]
    fn acquire_boot_fdt(&self) -> Result<ipc::wire::Cap, u32>;

    /// Выдаёт capability на Device-MMIO регион `index` (kernel-owned
    /// исключены). `Err` - индекс вне набора либо отказ выдачи.
    #[call]
    fn acquire_device_memory(&self, index: u32) -> Result<ipc::wire::Cap, u32>;
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

    use super::{
        BootstrapClient, BootstrapService, LOG_MESSAGE_MAX, LOG_TAG_MAX, dispatch_bootstrap,
    };

    #[derive(Default)]
    struct Sink {
        last_log: Option<String>,
    }

    impl BootstrapService for Sink {
        fn log(&mut self, tag: Str<'_, { LOG_TAG_MAX }>, message: Str<'_, { LOG_MESSAGE_MAX }>) {
            self.last_log = Some(format!("[{}] {}", tag.as_str(), message.as_str()));
        }

        fn acquire_irq_control(&mut self) -> Result<Cap, u32> {
            Ok(Cap::from_raw(NonZeroU32::new(0x77).expect("non-zero")))
        }

        fn acquire_userland_image(&mut self) -> Result<Cap, u32> {
            Ok(Cap::from_raw(NonZeroU32::new(0x88).expect("non-zero")))
        }

        fn acquire_boot_fdt(&mut self) -> Result<Cap, u32> {
            Ok(Cap::from_raw(NonZeroU32::new(0x99).expect("non-zero")))
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
        let client = BootstrapClient::new(client_end);
        client
            .log(
                Str::new("rootkeeper").expect("tag within bound"),
                Str::new("boot ok").expect("message within bound"),
            )
            .expect("log write");
        let mut sink = Sink::default();
        dispatch_bootstrap(&mut sink, &server_end).expect("dispatch ok");
        assert_eq!(sink.last_log.as_deref(), Some("[rootkeeper] boot ok"));
    }

    #[test]
    fn acquire_irq_control_round_trip() {
        let (client_end, server_end) = MockEnd::pair();
        let client = BootstrapClient::new(client_end);
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut sink = Sink::default();
                server_end.wait_readable(u64::MAX).expect("server wait");
                dispatch_bootstrap(&mut sink, &server_end).expect("dispatch ok");
            });
            let cap = client.acquire_irq_control().expect("call ok");
            assert_eq!(
                cap,
                Ok(Cap::from_raw(NonZeroU32::new(0x77).expect("non-zero")))
            );
        });
    }

    #[test]
    fn acquire_userland_image_round_trip() {
        let (client_end, server_end) = MockEnd::pair();
        let client = BootstrapClient::new(client_end);
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut sink = Sink::default();
                server_end.wait_readable(u64::MAX).expect("server wait");
                dispatch_bootstrap(&mut sink, &server_end).expect("dispatch ok");
            });
            let cap = client.acquire_userland_image().expect("call ok");
            assert_eq!(
                cap,
                Ok(Cap::from_raw(NonZeroU32::new(0x88).expect("non-zero")))
            );
        });
    }

    #[test]
    fn acquire_boot_fdt_round_trip() {
        let (client_end, server_end) = MockEnd::pair();
        let client = BootstrapClient::new(client_end);
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut sink = Sink::default();
                server_end.wait_readable(u64::MAX).expect("server wait");
                dispatch_bootstrap(&mut sink, &server_end).expect("dispatch ok");
            });
            let cap = client.acquire_boot_fdt().expect("call ok");
            assert_eq!(
                cap,
                Ok(Cap::from_raw(NonZeroU32::new(0x99).expect("non-zero")))
            );
        });
    }

    #[test]
    fn acquire_device_memory_round_trip() {
        let (client_end, server_end) = MockEnd::pair();
        let client = BootstrapClient::new(client_end);
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut sink = Sink::default();
                server_end.wait_readable(u64::MAX).expect("server wait");
                dispatch_bootstrap(&mut sink, &server_end).expect("dispatch ok");
            });
            // Аргумент index доезжает до сервера и влияет на ответный Cap.
            let cap = client.acquire_device_memory(1).expect("call ok");
            assert_eq!(
                cap,
                Ok(Cap::from_raw(NonZeroU32::new(0x91).expect("non-zero")))
            );
        });
    }

    #[test]
    fn acquire_device_memory_out_of_range_returns_err() {
        let (client_end, server_end) = MockEnd::pair();
        let client = BootstrapClient::new(client_end);
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut sink = Sink::default();
                server_end.wait_readable(u64::MAX).expect("server wait");
                dispatch_bootstrap(&mut sink, &server_end).expect("dispatch ok");
            });
            let result = client.acquire_device_memory(9).expect("call ok");
            assert_eq!(result, Err(7));
        });
    }
}
