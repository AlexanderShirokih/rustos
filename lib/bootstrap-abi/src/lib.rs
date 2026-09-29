//! Контракты первого процесса с ядром: [`bootstrap`] и [`hardware`].
//!
//! Ядро выдаёт первому процессу handle порта [`bootstrap`] при запуске, аргументом точки входа.
//! По нему процесс пишет строки в журнал ядра, получает регион userland-образа и сессию
//! [`hardware`]. Сессия [`hardware`] выдаёт полномочия на устройства машины: копию контроллера
//! прерываний, регион boot FDT и окна набора устройств. Держатель сессии не получает других
//! полномочий первого процесса.
//!
//! [`hardware::Client::acquire_device_memory_containing`] ищет окно набора, которое содержит
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

        /// Выдаёт вызывателю сессию [`hardware`]. Капа сессии разрешает отправку, перенос и
        /// дублирование и не разрешает приём. `Err` - ненулевой код ошибки выдачи.
        call fn acquire_hardware() -> own Result<ipc::wire::Session<hardware::Protocol>, u32>;
    }
}

ipc::protocol! {
    /// Канал держателя устройств машины к ядру.
    pub mod hardware {
        name = "Hardware";

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
    use std::{format, string::String, thread};

    use ipc::{
        IntoWireValue,
        wire::{Cap, Session, Str},
    };
    use ipc_test::{MockHandle, MockResources};
    use syscall::{ObjectKind, kind};

    use super::{BootFdt, LOG_MESSAGE_MAX, LOG_TAG_MAX, bootstrap, hardware};

    fn cap<K: ObjectKind>(raw: u32) -> MockHandle<K> {
        MockHandle::new(raw)
    }

    #[derive(Default)]
    struct Sink {
        last_log: Option<String>,
    }

    impl bootstrap::Service<MockResources> for Sink {
        fn log(
            &mut self,
            _frame: &MockResources,
            tag: Str<'_, { LOG_TAG_MAX }>,
            message: Str<'_, { LOG_MESSAGE_MAX }>,
        ) {
            self.last_log = Some(format!("[{}] {}", tag.as_str(), message.as_str()));
        }

        fn acquire_userland_image(
            &mut self,
            _frame: &MockResources,
        ) -> impl IntoWireValue<MockResources, Read = Result<Cap<kind::MemoryRegion>, u32>>
        {
            Ok::<_, u32>(cap::<kind::MemoryRegion>(0x88))
        }

        fn acquire_hardware(
            &mut self,
            _frame: &MockResources,
        ) -> impl IntoWireValue<MockResources, Read = Result<Session<hardware::Protocol>, u32>>
        {
            Ok::<_, u32>(Session::<hardware::Protocol, _>::from_handle(cap::<
                kind::Port,
            >(
                0x55
            )))
        }
    }

    impl hardware::Service<MockResources> for Sink {
        fn acquire_irq_control(
            &mut self,
            _frame: &MockResources,
        ) -> impl IntoWireValue<MockResources, Read = Result<Cap<kind::IrqControl>, u32>> {
            Ok::<_, u32>(cap::<kind::IrqControl>(0x77))
        }

        fn acquire_boot_fdt(
            &mut self,
            _frame: &MockResources,
        ) -> impl IntoWireValue<MockResources, Read = Result<BootFdt, u32>> {
            Ok::<_, u32>(BootFdt {
                region: cap::<kind::MemoryRegion>(0x99),
                offset: 0x600,
            })
        }

        fn acquire_device_memory(
            &mut self,
            _frame: &MockResources,
            index: u32,
        ) -> impl IntoWireValue<MockResources, Read = Result<Cap<kind::MemoryRegion>, u32>>
        {
            if index >= 2 {
                return Err(7);
            }
            Ok(cap::<kind::MemoryRegion>(0x90 + index))
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
                    .acquire_hardware()
                    .map(|session| session.map(Session::into_handle)),
                Ok(Ok(cap(0x55)))
            );
        });
    }

    #[test]
    fn device_acquisitions_answer_with_their_capability_or_refusal() {
        let (client_end, server_end) = ipc_test::pair();
        let client = hardware::Client::new(client_end);
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut sink = Sink::default();
                for _ in 0..4 {
                    server_end
                        .serve(|frame| hardware::dispatch(&mut sink, frame))
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
