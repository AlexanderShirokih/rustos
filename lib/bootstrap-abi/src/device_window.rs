//! Поиск device-окна по базовому PA: FDT даёт координаты устройства, право
//! на MMIO - cap из набора ядра; связь между ними - совпадение базы.

use ipc::{
    Transport,
    wire::{Cap, IpcError},
};

use crate::BootstrapClient;

/// Окно device-памяти, восстановимое из транспортного cap.
pub trait DeviceWindow: Sized {
    /// Усыновляет cap окна во владение.
    fn adopt(cap: Cap) -> Self;

    /// Базовый PA окна; `None`, если окно не инспектируется.
    fn base(&self) -> Option<u64>;
}

impl<T: Transport> BootstrapClient<T> {
    /// Находит device-окно с базой `base` перебором `acquire_device_memory`.
    /// `Ok(None)` - набор исчерпан (индекс вне набора либо отказ выдачи).
    pub fn acquire_device_memory_by_base<W: DeviceWindow>(
        &self,
        base: u64,
    ) -> Result<Option<W>, IpcError> {
        for index in 0.. {
            let Ok(cap) = self.acquire_device_memory(index)? else {
                return Ok(None);
            };
            let window = W::adopt(cap);
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

    use ipc::{Transport, wire::Cap};
    use ipc_test::MockEnd;

    use super::DeviceWindow;
    use crate::{BootstrapClient, BootstrapService, dispatch_bootstrap};

    /// Набор из двух окон: индекс `i` -> cap `0x90 + i`.
    struct Vendor;

    impl BootstrapService for Vendor {
        fn log(
            &mut self,
            _tag: ipc::wire::Str<'_, { crate::LOG_TAG_MAX }>,
            _message: ipc::wire::Str<'_, { crate::LOG_MESSAGE_MAX }>,
        ) {
        }

        fn acquire_irq_control(&mut self) -> Result<Cap, u32> {
            Err(1)
        }

        fn acquire_userland_image(&mut self) -> Result<Cap, u32> {
            Err(1)
        }

        fn acquire_boot_fdt(&mut self) -> Result<Cap, u32> {
            Err(1)
        }

        fn acquire_device_memory(&mut self, index: u32) -> Result<Cap, u32> {
            if index >= 2 {
                return Err(7);
            }
            Ok(Cap::from_raw(
                NonZeroU32::new(0x90 + index).expect("non-zero"),
            ))
        }
    }

    /// Окно-заглушка: база выводится из raw cap как `(raw - 0x90) * 0x1000`;
    /// окно с raw `0x90` не инспектируется.
    struct FakeWindow {
        cap: Cap,
    }

    impl DeviceWindow for FakeWindow {
        fn adopt(cap: Cap) -> Self {
            Self { cap }
        }

        fn base(&self) -> Option<u64> {
            if self.cap.raw().get() == 0x90 {
                return None;
            }
            Some(u64::from(self.cap.raw().get() - 0x90) * 0x1000)
        }
    }

    fn with_vendor<R>(body: impl FnOnce(&BootstrapClient<MockEnd>) -> R) -> R {
        let (client_end, server_end) = MockEnd::pair();
        let client = BootstrapClient::new(client_end);
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut vendor = Vendor;
                while server_end.wait_readable(u64::MAX).is_ok() {
                    if dispatch_bootstrap(&mut vendor, &server_end).is_err() {
                        break;
                    }
                }
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
        // База 0 принадлежала бы окну 0x90, но оно без base - совпадения нет.
        let found = with_vendor(|client| {
            client
                .acquire_device_memory_by_base::<FakeWindow>(0)
                .expect("call ok")
        });
        assert!(found.is_none());
    }
}
