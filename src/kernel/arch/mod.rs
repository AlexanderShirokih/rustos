#[cfg(target_arch = "aarch64")]
pub mod aarch64;

use crate::kernel::kernel::Kernel;
use crate::kernel::memory::physical::PhysicalMemoryManager;

/// Set up memory for the current architecture
pub fn setup_memory(kernel: &Kernel, pmm: &mut PhysicalMemoryManager) {
    #[cfg(target_arch = "aarch64")]
    {
        use crate::kernel::arch::aarch64::kernel::Aarch64Kernel;
        <Kernel as Aarch64Kernel>::setup_memory(kernel, pmm);
    }
}
