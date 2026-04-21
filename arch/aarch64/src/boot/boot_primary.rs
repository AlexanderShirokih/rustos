//! Post-MMU фаза загрузки: инициализация драйверов, подсистем и передача управления kmain.

use alloc::boxed::Box;

use drivers_aarch64::drivers;
use drivers_common::scanner::EmbeddedDriversScanner;
use drivers_common_aarch64::adapt_to_fdt_tree;
use io::buffered_writer::BufferedWriter;
use kernel::{kernel_context::KernelContext, kmain::kmain, sched::SchedulerConfig};
use klog::info;
use memory::{
    physical_address::{PageAlignedAddress, PhysicalAddress},
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};

use crate::{
    HIGHER_HALF_BASE,
    exception::ExceptionVectors,
    memory::memory_setup::{Enabled, MemorySetup},
    sched::Aarch64Context,
};

const SCHED_CONFIG: SchedulerConfig = SchedulerConfig::new(32, 64);

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

    let result = MemorySetup::<Enabled>::switch_to_heap_allocator(
        higher_root,
        frame_allocator_phys,
        higher_half_base,
    )
    .expect("Failed to install heap allocator");

    ExceptionVectors::instance().install();

    let buffered = Box::leak(Box::new(BufferedWriter::new()));
    klog::set_stdout(buffered);

    info!("primary_main: post-MMU initialization complete");

    let dtb_virt = dtb_phys + HIGHER_HALF_BASE;
    let device_tree = fdt::devicetree::DeviceTree::from_ptr(dtb_virt)
        .expect("Failed to parse DTB at virtual address");
    let device_tree: &'static fdt::devicetree::DeviceTree =
        unsafe { core::mem::transmute(&device_tree) };

    let mut driver_scanner = EmbeddedDriversScanner::new();
    let root = adapt_to_fdt_tree(device_tree)
        .root()
        .expect("DTB root node missing");
    driver_scanner.scan_and_probe(root, drivers());

    let memory_mapper: &'static dyn memory::memory_mapper::MemoryMapper =
        Box::leak(result.memory_mapper);
    let kernel = Box::leak(Box::new(KernelContext::new(
        memory_mapper,
        result.base_offset,
    )));

    kmain::<Aarch64Context>(driver_scanner, kernel, buffered, SCHED_CONFIG)
}
