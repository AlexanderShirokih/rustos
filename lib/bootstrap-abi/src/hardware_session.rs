//! Контракт [`hardware`] держателя устройств машины с ядром.

use syscall::kind;

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
    use std::thread;

    use ipc::{IntoWireValue, wire::Cap};
    use ipc_test::{MockHandle, MockResources};
    use syscall::{ObjectKind, kind};

    use super::{BootFdt, hardware};

    fn cap<K: ObjectKind>(raw: u32) -> MockHandle<K> {
        MockHandle::new(raw)
    }

    struct Sink;

    impl hardware::Service<MockResources> for Sink {
        fn acquire_irq_control(
            &self,
            _frame: &MockResources,
        ) -> impl IntoWireValue<MockResources, Read = Result<Cap<kind::IrqControl>, u32>> {
            Ok::<_, u32>(cap::<kind::IrqControl>(0x77))
        }

        fn acquire_boot_fdt(
            &self,
            _frame: &MockResources,
        ) -> impl IntoWireValue<MockResources, Read = Result<BootFdt, u32>> {
            Ok::<_, u32>(BootFdt {
                region: cap::<kind::MemoryRegion>(0x99),
                offset: 0x600,
            })
        }

        fn acquire_device_memory(
            &self,
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
    fn device_acquisitions_answer_with_their_capability_or_refusal() {
        let (client_end, server_end) = ipc_test::pair();
        let client = hardware::Client::new(client_end);
        thread::scope(|scope| {
            scope.spawn(|| {
                let sink = Sink;
                for _ in 0..4 {
                    server_end
                        .serve(|frame| hardware::dispatch(&sink, frame))
                        .expect("dispatch ok");
                }
            });
            assert_eq!(client.acquire_irq_control().wait(), Ok(cap(0x77)));
            assert_eq!(
                client.acquire_boot_fdt().wait(),
                Ok(BootFdt {
                    region: cap(0x99),
                    offset: 0x600,
                })
            );
            assert_eq!(client.acquire_device_memory(1).wait(), Ok(cap(0x91)));
            assert_eq!(
                client.acquire_device_memory(9).wait(),
                Err(ipc::CallError::Remote(7))
            );
        });
    }
}
