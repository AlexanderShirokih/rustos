//! Контракты первого процесса с ядром: [`bootstrap`] и [`devices`].
//!
//! Ядро выдаёт первому процессу handle порта [`bootstrap`] при запуске, аргументом точки входа.
//! По нему процесс пишет строки в журнал ядра, получает регион userland-образа и сессию
//! [`devices`]. Сессия [`devices`] выдаёт полномочия на устройства машины: копию контроллера
//! прерываний, регион boot FDT и окна набора устройств. Держатель сессии не получает других
//! полномочий первого процесса.
//!
//! [`devices::Client::acquire_device_memory_containing`] ищет окно набора, которое содержит
//! окно устройства. [`bootstrap::Client::log_str`] режет строку длиннее кадра по границе
//! символа.

#![cfg_attr(not(test), no_std)]

#[cfg(test)]
extern crate std;

mod device_window;
mod log;

pub use device_window::{ContainingWindow, DeviceWindow};
use syscall::kind;

/// Граница тега источника в [`bootstrap::log`].
pub const LOG_TAG_MAX: usize = 64;

/// Граница строки [`bootstrap::log`]: остаток кадра за тегом.
pub const LOG_MESSAGE_MAX: usize =
    ipc::wire::FIELD_DATA_MAX - LOG_TAG_MAX - ipc::wire::FIELD_OVERHEAD;

ipc::wire_value! {
    /// Boot FDT: регион поверх страниц с блобом и его начало внутри региона.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct BootFdt<M = ipc::wire::Cap<kind::MemoryRegion>> {
        /// Read-only регион, целиком покрывающий блоб.
        pub region: M,
        /// Смещение первого байта блоба от начала региона.
        pub offset: u32,
    }
}

ipc::protocol! {
    /// Канал начального процесса к ядру.
    pub mod bootstrap {
        name = "Bootstrap";

        /// Передаёт строку для логгера ядра klog. `tag` - источник строки
        /// (обычно имя процесса). Пустой тег - строка без тега.
        cast fn log(
            tag: ipc::wire::Str<{ LOG_TAG_MAX }>,
            message: ipc::wire::Str<{ LOG_MESSAGE_MAX }>,
        );

        /// Выдаёт вызывателю read-only capability на регион поверх байт userland-образа.
        call fn acquire_userland_image() -> own Result<ipc::wire::Cap<kind::MemoryRegion>, u32>;

        /// Выдаёт вызывателю сессию [`devices`]. Капа сессии разрешает отправку, перенос и
        /// дублирование и не разрешает приём. `Err` - ненулевой код ошибки выдачи.
        call fn acquire_devices() -> own Result<ipc::wire::Session<devices::Protocol>, u32>;
    }
}

ipc::protocol! {
    /// Канал держателя устройств машины к ядру.
    pub mod devices {
        name = "Devices";

        /// Выдаёт копию корневого `IrqControl` в таблицу вызывателя (оригинал
        /// остаётся у ядра). `Err` - ненулевой код ошибки выдачи.
        call fn acquire_irq_control() -> own Result<ipc::wire::Cap<kind::IrqControl>, u32>;

        /// Выдаёт вызывателю read-only capability на регион поверх байт boot FDT (DTB)
        /// вместе со смещением блоба внутри региона.
        call fn acquire_boot_fdt() -> own Result<BootFdt, u32>;

        /// Выдаёт capability на Device-MMIO регион `index` (kernel-owned
        /// исключены). `Err` - индекс вне набора либо отказ выдачи.
        call fn acquire_device_memory(index: u32) -> own Result<ipc::wire::Cap<kind::MemoryRegion>, u32>;
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU32;
    use std::{format, string::String, thread};

    use ipc::{
        IntoWireValue,
        wire::{Cap, Session, Str},
    };
    use syscall::{Handle, ObjectKind, kind};

    use super::{BootFdt, LOG_MESSAGE_MAX, LOG_TAG_MAX, bootstrap, devices};

    fn cap<K: ObjectKind>(raw: u32) -> Cap<K> {
        Cap::from_handle(Handle::from_abi(NonZeroU32::new(raw).expect("non-zero")))
    }

    #[derive(Default)]
    struct Sink {
        last_log: Option<String>,
    }

    impl<F: ipc::ReceivedResources> bootstrap::Service<F> for Sink {
        fn log(
            &mut self,
            _frame: &F,
            tag: Str<'_, { LOG_TAG_MAX }>,
            message: Str<'_, { LOG_MESSAGE_MAX }>,
        ) {
            self.last_log = Some(format!("[{}] {}", tag.as_str(), message.as_str()));
        }

        fn acquire_userland_image(
            &mut self,
            _frame: &F,
        ) -> impl IntoWireValue<Read = Result<Cap<kind::MemoryRegion>, u32>> {
            Ok::<_, u32>(cap(0x88))
        }

        fn acquire_devices(
            &mut self,
            _frame: &F,
        ) -> impl IntoWireValue<Read = Result<Session<devices::Protocol>, u32>> {
            Ok::<_, u32>(Session::<devices::Protocol>::from_handle(cap(0x55)))
        }
    }

    impl<F: ipc::ReceivedResources> devices::Service<F> for Sink {
        fn acquire_irq_control(
            &mut self,
            _frame: &F,
        ) -> impl IntoWireValue<Read = Result<Cap<kind::IrqControl>, u32>> {
            Ok::<_, u32>(cap(0x77))
        }

        fn acquire_boot_fdt(
            &mut self,
            _frame: &F,
        ) -> impl IntoWireValue<Read = Result<BootFdt, u32>> {
            Ok::<_, u32>(BootFdt {
                region: cap(0x99),
                offset: 0x600,
            })
        }

        fn acquire_device_memory(
            &mut self,
            _frame: &F,
            index: u32,
        ) -> impl IntoWireValue<Read = Result<Cap<kind::MemoryRegion>, u32>> {
            if index >= 2 {
                return Err(7);
            }
            Ok(cap(0x90 + index))
        }
    }

    #[test]
    fn log_round_trip() {
        let (client_end, server_end) = ipc_test::pair();
        let client = bootstrap::Client::new(client_end);
        client
            .log(
                Str::new("rootkeeper").expect("tag within bound"),
                Str::new("boot ok").expect("message within bound"),
            )
            .expect("log write");
        let mut sink = Sink::default();
        server_end
            .serve(|frame| bootstrap::dispatch(&mut sink, frame))
            .expect("dispatch ok");
        assert_eq!(sink.last_log.as_deref(), Some("[rootkeeper] boot ok"));
    }

    #[test]
    fn bootstrap_acquisitions_answer_with_their_capability() {
        let (client_end, server_end) = ipc_test::pair();
        let client = bootstrap::Client::new(client_end);
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut sink = Sink::default();
                for _ in 0..2 {
                    server_end
                        .serve(|frame| bootstrap::dispatch(&mut sink, frame))
                        .expect("dispatch ok");
                }
            });
            assert_eq!(client.acquire_userland_image(), Ok(Ok(cap(0x88))));
            assert_eq!(
                client
                    .acquire_devices()
                    .map(|session| session.map(Session::into_handle)),
                Ok(Ok(cap(0x55)))
            );
        });
    }

    #[test]
    fn device_acquisitions_answer_with_their_capability_or_refusal() {
        let (client_end, server_end) = ipc_test::pair();
        let client = devices::Client::new(client_end);
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut sink = Sink::default();
                for _ in 0..4 {
                    server_end
                        .serve(|frame| devices::dispatch(&mut sink, frame))
                        .expect("dispatch ok");
                }
            });
            assert_eq!(client.acquire_irq_control(), Ok(Ok(cap(0x77))));
            assert_eq!(
                client.acquire_boot_fdt(),
                Ok(Ok(BootFdt {
                    region: cap(0x99),
                    offset: 0x600,
                }))
            );
            assert_eq!(client.acquire_device_memory(1), Ok(Ok(cap(0x91))));
            assert_eq!(client.acquire_device_memory(9), Ok(Err(7)));
        });
    }
}
