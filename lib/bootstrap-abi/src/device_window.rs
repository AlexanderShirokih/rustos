//! Поиск device-окна по базовому PA: FDT даёт координаты устройства, право
//! на MMIO - cap из набора ядра. Связь между ними - совпадение базы.

use ipc::{Channel, ReceivedResources, wire::IpcError};
use syscall::kind;

use crate::bootstrap;

/// Окно device-памяти, построенное из доставленной capability `H`.
pub trait DeviceWindow<H>: Sized + From<H> {
    /// Базовый PA окна. `None`, если окно не инспектируется.
    fn base(&self) -> Option<u64>;
}

impl<C: Channel> bootstrap::Client<C> {
    /// Находит device-окно с базой `base` перебором `acquire_device_memory`.
    /// `Ok(None)` - набор исчерпан (индекс вне набора либо отказ выдачи).
    pub fn acquire_device_memory_by_base<
        W: DeviceWindow<<C::Resources as ReceivedResources>::Handle<kind::MemoryRegion>>,
    >(
        &self,
        base: u64,
    ) -> Result<Option<W>, IpcError> {
        for index in 0.. {
            let Ok(region) = self.acquire_device_memory(index)? else {
                return Ok(None);
            };
            let window = W::from(region);
            if window.base() == Some(base) {
                return Ok(Some(window));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU32;
    use std::thread;

    use ipc::wire::Cap;
    use syscall::{Handle, ObjectKind, kind};

    use super::DeviceWindow;

    fn cap<K: ObjectKind>(raw: u32) -> Cap<K> {
        Cap::from_handle(Handle::from_abi(NonZeroU32::new(raw).expect("non-zero")))
    }
    use crate::bootstrap;

    struct Vendor;

    impl<F: ipc::ReceivedResources> bootstrap::Service<F> for Vendor {
        fn log(
            &mut self,
            _frame: &F,
            _tag: ipc::wire::Str<'_, { crate::LOG_TAG_MAX }>,
            _message: ipc::wire::Str<'_, { crate::LOG_MESSAGE_MAX }>,
        ) {
        }

        fn acquire_irq_control(&mut self, _frame: &F) -> Result<Cap<kind::IrqControl>, u32> {
            Err(1)
        }

        fn acquire_userland_image(&mut self, _frame: &F) -> Result<Cap<kind::MemoryRegion>, u32> {
            Err(1)
        }

        fn acquire_boot_fdt(&mut self, _frame: &F) -> Result<crate::BootFdt, u32> {
            Err(1)
        }

        fn acquire_device_memory(
            &mut self,
            _frame: &F,
            index: u32,
        ) -> Result<Cap<kind::MemoryRegion>, u32> {
            if index >= 2 {
                return Err(7);
            }
            Ok(cap(0x90 + index))
        }
    }

    struct FakeWindow {
        cap: Cap<kind::MemoryRegion>,
    }

    impl From<Cap<kind::MemoryRegion>> for FakeWindow {
        fn from(cap: Cap<kind::MemoryRegion>) -> Self {
            Self { cap }
        }
    }

    impl DeviceWindow<Cap<kind::MemoryRegion>> for FakeWindow {
        fn base(&self) -> Option<u64> {
            if self.cap.raw().get() == 0x90 {
                return None;
            }
            Some(u64::from(self.cap.raw().get() - 0x90) * 0x1000)
        }
    }

    fn with_vendor<R>(body: impl FnOnce(&bootstrap::Client<ipc_test::MockChannel>) -> R) -> R {
        let (client_end, server_end) = ipc_test::pair();
        let client = bootstrap::Client::new(client_end);
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut vendor = Vendor;
                let _ = server_end.serve_all(|frame| bootstrap::dispatch(&mut vendor, frame));
            });
            let result = body(&client);
            drop(client);
            result
        })
    }

    #[test]
    fn finds_window_with_matching_base() {
        let found = with_vendor(|client| {
            client
                .acquire_device_memory_by_base::<FakeWindow>(0x1000)
                .expect("call ok")
        });
        assert_eq!(found.expect("window found").cap.raw().get(), 0x91);
    }

    #[test]
    fn exhausted_set_yields_none() {
        let found = with_vendor(|client| {
            client
                .acquire_device_memory_by_base::<FakeWindow>(0x5000)
                .expect("call ok")
        });
        assert!(found.is_none());
    }

    #[test]
    fn uninspectable_window_is_skipped_not_matched() {
        // База 0 принадлежала бы окну 0x90, но окно без base никогда не совпадает.
        let found = with_vendor(|client| {
            client
                .acquire_device_memory_by_base::<FakeWindow>(0)
                .expect("call ok")
        });
        assert!(found.is_none());
    }
}
