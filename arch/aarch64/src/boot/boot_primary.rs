//! Post-MMU фаза загрузки: инициализация драйверов, подсистем и передача управления kmain.

use alloc::boxed::Box;

use drivers_aarch64::drivers;
use drivers_common::scanner::DriverScanner;
use drivers_common_aarch64::adapt_tree;
use io::buffered_writer::BufferedWriter;
use kernel::kernel_context::KernelContext;
use kernel::kmain::kmain;
use klog::{debug, info, set_stdout};
use memory::physical_address::{PageAlignedAddress, PhysicalAddress};
use memory::virtual_address::{PageAlignedVirtualAddress, VirtualAddress};

use crate::HIGHER_HALF_BASE;
use crate::exception::ExceptionVectors;
use crate::memory::memory_setup::{Enabled, MemorySetup};
use crate::memory::mmu::Mmu;

/// К этому моменту:
/// - PC и SP - виртуальные адреса (TTBR1)
/// - TTBR0 ещё активен (identity mapping)
/// - GLOBAL_ALLOCATOR в PHASE_FROZEN
pub fn primary_main(dtb_phys: usize, higher_root_pa: usize, frame_allocator_phys: usize) -> ! {
    let higher_half_base =
        PageAlignedVirtualAddress::new_unchecked(VirtualAddress::new(HIGHER_HALF_BASE));
    let higher_root =
        PageAlignedAddress::from_usize(higher_root_pa).expect("higher_root_pa must be 4K-aligned");
    let frame_allocator_phys = PhysicalAddress::new(frame_allocator_phys);

    let result = MemorySetup::<Enabled>::install_from_raw(
        higher_root,
        frame_allocator_phys,
        higher_half_base,
    )
    .expect("Failed to install heap allocator");

    // Немедленно удаляем identity mapping - SP и PC уже виртуальные
    Mmu::new().disable_lower_half();

    // Устанавливаем векторы исключений
    ExceptionVectors::instance().install();

    // BufferedWriter как stdout до инициализации UART
    let buffered = Box::leak(Box::new(BufferedWriter::new()));
    set_stdout(buffered);

    info!("primary_main: post-MMU initialization complete");
    debug!("primary_main: TTBR0 (identity) cleared, TTBR1 (higher-half) active");

    // Парсим DTB по виртуальному адресу (phys + HIGHER_HALF_BASE)
    let dtb_virt = dtb_phys + HIGHER_HALF_BASE;

    let device_tree = fdt::devicetree::DeviceTree::from_ptr(dtb_virt)
        .expect("Failed to parse DTB at virtual address");

    // Делаем DeviceTree 'static (он в mapped RAM, lifetime корректен на всё время работы ядра)
    let device_tree: &'static fdt::devicetree::DeviceTree =
        unsafe { core::mem::transmute(&device_tree) };

    // Единый scan: все драйверы из .drivers.kernel
    let mut driver_scanner = DriverScanner::new();
    let root = adapt_tree(device_tree)
        .root()
        .expect("DTB root node missing");
    driver_scanner.scan_and_probe(root, drivers());

    let memory_mapper: &'static dyn memory::memory_mapper::MemoryMapper =
        Box::leak(result.memory_mapper);
    let kernel = KernelContext::new(memory_mapper, result.base_offset);
    let kernel = Box::leak(Box::new(kernel));

    kmain(driver_scanner, kernel, buffered);

    loop {
        core::hint::spin_loop();
    }
}
