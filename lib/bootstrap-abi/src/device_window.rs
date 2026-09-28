//! Поиск device-окна, которое содержит окно устройства: FDT даёт координаты устройства,
//! право на MMIO - cap из набора ядра. Связь между ними - вхождение окна устройства в
//! физический диапазон окна набора.

use core::ops::Range;

use ipc::{Channel, ReceivedResources, wire::IpcError};
use syscall::kind;

use crate::bootstrap;

/// Окно device-памяти, построенное из доставленной capability `H`.
pub trait DeviceWindow<H>: Sized + From<H> {
    /// Физический диапазон окна. `None`, если окно не инспектируется.
    fn physical_range(&self) -> Option<Range<u64>>;
}

/// Device-окно набора, которое содержит окно устройства.
#[derive(Debug)]
pub struct ContainingWindow<W> {
    /// Окно набора.
    pub window: W,
    /// Смещение окна устройства от начала окна набора.
    pub offset: u64,
}

impl<C: Channel> bootstrap::Client<C> {
    /// Находит перебором `acquire_device_memory` device-окно, которое целиком содержит
    /// `size` байт от физического адреса `base`, и смещение `base` от начала этого окна.
    /// `Ok(None)` - окно пустое, его конец выходит за разрядность `u64` либо набор исчерпан
    /// (индекс вне набора либо отказ выдачи).
    pub fn acquire_device_memory_containing<
        W: DeviceWindow<<C::Resources as ReceivedResources>::Handle<kind::MemoryRegion>>,
    >(
        &self,
        base: u64,
        size: u64,
    ) -> Result<Option<ContainingWindow<W>>, IpcError> {
        let Some(end) = base.checked_add(size).filter(|end| *end > base) else {
            return Ok(None);
        };

        for index in 0.. {
            let Ok(region) = self.acquire_device_memory(index)? else {
                return Ok(None);
            };
            let window = W::from(region);
            let offset = window
                .physical_range()
                .filter(|range| range.start <= base && end <= range.end)
                .map(|range| base - range.start);
            if let Some(offset) = offset {
                return Ok(Some(ContainingWindow { window, offset }));
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
        fn physical_range(&self) -> Option<core::ops::Range<u64>> {
            if self.cap.raw().get() == 0x90 {
                return None;
            }
            let base = u64::from(self.cap.raw().get() - 0x90) * 0x1000;
            Some(base..base + 0x1000)
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

    fn containing(base: u64, size: u64) -> Option<(u32, u64)> {
        with_vendor(|client| {
            client
                .acquire_device_memory_containing::<FakeWindow>(base, size)
                .expect("call ok")
        })
        .map(|found| (found.window.cap.raw().get(), found.offset))
    }

    #[test]
    fn finds_the_window_containing_the_device() {
        assert_eq!(containing(0x1200, 0x100), Some((0x91, 0x200)));
    }

    #[test]
    fn a_device_at_the_window_start_has_no_offset() {
        assert_eq!(containing(0x1000, 0x1000), Some((0x91, 0)));
    }

    #[test]
    fn a_device_at_the_window_end_is_found() {
        assert_eq!(containing(0x1f00, 0x100), Some((0x91, 0xf00)));
    }

    #[test]
    fn an_empty_device_window_is_not_found() {
        assert_eq!(containing(0x2000, 0), None);
    }

    #[test]
    fn a_device_past_the_address_space_is_not_found() {
        assert_eq!(containing(u64::MAX, 2), None);
    }

    #[test]
    fn a_device_across_the_window_edge_is_not_found() {
        assert_eq!(containing(0x1f00, 0x200), None);
    }

    #[test]
    fn a_device_outside_the_set_is_not_found() {
        assert_eq!(containing(0x5000, 0x100), None);
    }

    #[test]
    fn an_uninspectable_window_contains_no_device() {
        assert_eq!(containing(0, 0x100), None);
    }
}
