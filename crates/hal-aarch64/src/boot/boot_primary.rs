//! Post-MMU фаза загрузки: инициализация драйверов, подсистем и передача управления kmain.

use alloc::boxed::Box;

use drivers_aarch64::drivers;
use drivers_common::scanner::EmbeddedDriversScanner;
use drivers_common_aarch64::adapt_to_fdt_tree;
use io::buffered_writer::BufferedWriter;
use klog::info;
use main::{
    kernel_context::KernelContext,
    kmain::kmain,
    sched::{Bootstrapped, KernelTimerSource, Scheduler, SchedulerConfig},
};
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
    );

    ExceptionVectors::instance().install();

    let buffered = Box::leak(Box::new(BufferedWriter::new()));
    klog::set_stdout(buffered);

    info!("primary_main: post-MMU initialization complete");

    let dtb_virt = dtb_phys + HIGHER_HALF_BASE;
    let device_tree = fdt::devicetree::DeviceTree::from_ptr(dtb_virt)
        .expect("Failed to parse DTB at virtual address");
    // SAFETY: DTB замаплен в higher-half и существует в течение всей жизни ядра, поэтому
    // продление времени жизни ссылки до `'static` корректно. `transmute` тут используется
    // только для удлинения lifetime - layout `&DeviceTree` остаётся прежним.
    let device_tree: &'static fdt::devicetree::DeviceTree =
        unsafe { core::mem::transmute(&device_tree) };

    let mut driver_scanner = EmbeddedDriversScanner::new();
    let root = adapt_to_fdt_tree(device_tree)
        .root()
        .expect("DTB root node missing");
    driver_scanner.scan_and_probe(root, drivers());

    let memory_mapper: &'static (dyn memory::memory_mapper::MemoryMapper + Send + Sync) =
        Box::leak(result.memory_mapper);
    let address_space_factory: &'static (
                 dyn memory::memory_mapper::AddressSpaceFactory + Send + Sync
             ) = Box::leak(result.address_space_factory);
    let kernel = Box::leak(Box::new(KernelContext::new(
        memory_mapper,
        address_space_factory,
        result.base_offset,
    )));

    #[cfg(feature = "qemu-tests")]
    qemu_test_harness::runner::install_backend(&qemu_test_harness_aarch64::BACKEND);

    kmain::<Aarch64Context, _>(
        driver_scanner,
        kernel,
        buffered,
        SCHED_CONFIG,
        pick_init_task(),
    )
}

/// Возвращает init-таск для текущей сборочной фичи: production-init
/// или qemu-tests harness. Вынесено в отдельную функцию, чтобы держать
/// fea-выбор в одном месте.
fn pick_init_task()
-> fn(&Scheduler<Aarch64Context, KernelTimerSource, Bootstrapped>, &mut KernelContext) {
    #[cfg(feature = "qemu-tests")]
    {
        main::qemu_tests::spawn_qemu_tests_process::<Aarch64Context>
    }
    #[cfg(not(feature = "qemu-tests"))]
    {
        main::init::spawn_init_process::<Aarch64Context>
    }
}
