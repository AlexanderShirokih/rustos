use bootstrap_abi::{DeviceWindow, bootstrap};
use ipc::{Channel, ReceivedResources};
use syscall::kind;

use crate::platform::{DeviceMmio, ResolveError};

/// Забирает у ядра capability MMIO-окна `window`. Индекс окна в наборе ядра
/// наружу не выходит. Набор перебирается по базовому адресу.
pub fn acquire_mmio<
    C: Channel,
    W: DeviceWindow<<C::Resources as ReceivedResources>::Handle<kind::MemoryRegion>>,
>(
    kernel: &bootstrap::Client<C>,
    window: DeviceMmio,
) -> Result<W, ResolveError> {
    kernel
        .acquire_device_memory_by_base(window.base)
        .ok()
        .flatten()
        .ok_or(ResolveError::MmioUnavailable)
}
