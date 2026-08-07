use bootstrap::{BootstrapClient, DeviceWindow};
use ipc::Transport;

use crate::platform::{DeviceMmio, ResolveError};

/// Забирает у ядра capability MMIO-окна `window`. Индекс окна в наборе ядра
/// наружу не выходит: набор перебирается по базовому адресу.
pub fn acquire_mmio<T: Transport, W: DeviceWindow>(
    kernel: &BootstrapClient<T>,
    window: DeviceMmio,
) -> Result<W, ResolveError> {
    kernel
        .acquire_device_memory_by_base(window.base)
        .ok()
        .flatten()
        .ok_or(ResolveError::MmioUnavailable)
}
