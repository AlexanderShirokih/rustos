use bootstrap_abi::{DeviceWindow, bootstrap};
use ipc::{ReceivedResources, Transport};

use crate::platform::{DeviceMmio, ResolveError};

/// Забирает у ядра capability MMIO-окна `window`. Индекс окна в наборе ядра
/// наружу не выходит. Набор перебирается по базовому адресу.
pub fn acquire_mmio<T: Transport, W: DeviceWindow<<T::Resources as ReceivedResources>::Handle>>(
    kernel: &bootstrap::Client<T>,
    window: DeviceMmio,
) -> Result<W, ResolveError> {
    kernel
        .acquire_device_memory_by_base(window.base)
        .ok()
        .flatten()
        .ok_or(ResolveError::MmioUnavailable)
}
