//! Bootstrap-контракт: канал init процесса к ядру.

#![cfg_attr(not(test), no_std)]

#[cfg(test)]
extern crate std;

/// Граница строки [`Bootstrap::log`].
pub const LOG_MESSAGE_MAX: usize = ipc::wire::FIELD_DATA_MAX;

#[ipc::protocol(name = "Bootstrap")]
pub trait Bootstrap {
    /// Передаёт строку для логгера ядра klog.
    #[cast]
    fn log(&self, message: ipc::wire::FieldStr);

    /// Выдаёт копию корневого `IrqControl` в таблицу вызывателя (оригинал
    /// остаётся у ядра). `Err` - ненулевой код ошибки выдачи.
    #[call]
    fn acquire_irq_control(&self) -> Result<ipc::wire::Cap, u32>;

    /// Выдаёт вызывателю read-only capability на регион поверх байт userland-образа.
    /// `Err` - ненулевой код ошибки выдачи.
    #[call]
    fn acquire_userland_image(&self) -> Result<ipc::wire::Cap, u32>;

    /// Выдаёт capability на Device-MMIO регион `index` из FDT-набора (kernel-owned
    /// исключены). `Err` - ненулевой код ошибки: индекс вне набора либо отказ выдачи.
    #[call]
    fn acquire_device_memory(&self, index: u32) -> Result<ipc::wire::Cap, u32>;
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU32;
    use std::{
        string::{String, ToString},
        thread,
    };

    use ipc::{
        Transport,
        wire::{Cap, FieldStr},
    };
    use ipc_test::MockEnd;

    use super::{BootstrapClient, BootstrapService, dispatch_bootstrap};

    #[derive(Default)]
    struct Sink {
        last_log: Option<String>,
    }

    impl BootstrapService for Sink {
        fn log(&mut self, message: FieldStr<'_>) {
            self.last_log = Some(message.as_str().to_string());
        }

        fn acquire_irq_control(&mut self) -> Result<Cap, u32> {
            Ok(Cap::from_raw(NonZeroU32::new(0x77).expect("non-zero")))
        }

        fn acquire_userland_image(&mut self) -> Result<Cap, u32> {
            Ok(Cap::from_raw(NonZeroU32::new(0x88).expect("non-zero")))
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
            .log(FieldStr::new("boot ok").expect("within bound"))
            .expect("log write");
        let mut sink = Sink::default();
        dispatch_bootstrap(&mut sink, &server_end).expect("dispatch ok");
        assert_eq!(sink.last_log.as_deref(), Some("boot ok"));
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
