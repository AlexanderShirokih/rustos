//! Поиск device-окна по базовому PA: FDT даёт координаты устройства, право
//! на MMIO - cap из набора ядра. Связь между ними - совпадение базы.

use ipc::{ReceivedResources, Transport, wire::IpcError};

use crate::bootstrap;

/// Окно device-памяти, построенное из доставленной capability `H`.
pub trait DeviceWindow<H>: Sized + From<H> {
    /// Базовый PA окна. `None`, если окно не инспектируется.
    fn base(&self) -> Option<u64>;
}

impl<T: Transport> bootstrap::Client<T> {
    /// Находит device-окно с базой `base` перебором `acquire_device_memory`.
    /// `Ok(None)` - набор исчерпан (индекс вне набора либо отказ выдачи).
    pub fn acquire_device_memory_by_base<
        W: DeviceWindow<<T::Resources as ReceivedResources>::Handle>,
    >(
        &self,
        base: u64,
    ) -> Result<Option<W>, IpcError> {
        for index in 0.. {
            let mut response = self.acquire_device_memory(index)?;
            let Ok(cap) = response.value else {
                return Ok(None);
            };
            let window = W::from(response.claim(cap).ok_or(IpcError::InvalidValue)?);
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
    use crate::bootstrap;

    struct Vendor;

    impl bootstrap::Service for Vendor {
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

        fn acquire_boot_fdt(&mut self) -> Result<crate::BootFdt, u32> {
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

    struct FakeWindow {
        cap: Cap,
    }

    impl From<Cap> for FakeWindow {
        fn from(cap: Cap) -> Self {
            Self { cap }
        }
    }

    impl DeviceWindow<Cap> for FakeWindow {
        fn base(&self) -> Option<u64> {
            if self.cap.raw().get() == 0x90 {
                return None;
            }
            Some(u64::from(self.cap.raw().get() - 0x90) * 0x1000)
        }
    }

    fn with_vendor<R>(body: impl FnOnce(&bootstrap::Client<MockEnd>) -> R) -> R {
        let (client_end, server_end) = MockEnd::pair();
        let client = bootstrap::Client::new(client_end);
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut vendor = Vendor;
                while server_end.wait_readable(ipc::Timeout::INFINITE).is_ok() {
                    if bootstrap::dispatch(&mut vendor, &server_end).is_err() {
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
