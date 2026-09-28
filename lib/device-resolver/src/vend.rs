use bootstrap_abi::{ContainingWindow, DeviceWindow, bootstrap};
use ipc::{Channel, ReceivedResources};
use syscall::kind;

use crate::platform::{DeviceMmio, ResolveError};

/// Забирает у ядра capability окна набора, которое содержит MMIO-окно устройства `window`, и
/// смещение `window` от начала окна набора. Индекс окна в наборе ядра наружу не выходит.
pub fn acquire_mmio<
    C: Channel,
    W: DeviceWindow<<C::Resources as ReceivedResources>::Handle<kind::MemoryRegion>>,
>(
    kernel: &bootstrap::Client<C>,
    window: DeviceMmio,
) -> Result<ContainingWindow<W>, ResolveError> {
    kernel
        .acquire_device_memory_containing(window.base, window.size)
        .ok()
        .flatten()
        .ok_or(ResolveError::MmioUnavailable)
}
