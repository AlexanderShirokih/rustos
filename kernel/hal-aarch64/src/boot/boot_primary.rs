//! Post-MMU фаза загрузки: инициализация драйверов, подсистем и передача управления kmain.

use alloc::boxed::Box;
use core::num::NonZeroUsize;

use drivers_aarch64::drivers;
use drivers_common::scanner::EmbeddedDriversScanner;
use drivers_common_aarch64::adapt_to_fdt_tree;
use io::buffered_writer::BufferedWriter;
use kernelspace::{
    kernel_context::KernelContext, kmain::kmain, scheduler_bootstrap::KernelTimerSource,
};
use klog::info;
use memory::{
    physical_address::{PageAlignedAddress, PhysicalAddress},
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};
use scheduler::{Bootstrapped, Scheduler, SchedulerConfig};

use crate::{
    consts::{HIGHER_HALF_BASE, KMMIO_BASE, KMMIO_MAX_SIZE},
    exception::ExceptionVectors,
    memory::memory_setup::{Enabled, MemorySetup},
    sched::Aarch64Context,
};

const SCHED_CONFIG: SchedulerConfig = SchedulerConfig::new(32, 64);

/// Физические адреса, пробрасываемые из pre-MMU фазы в post-MMU.
pub(super) struct BootHandoff {
    pub dtb_phys: usize,
    pub higher_root_pa: usize,
    pub frame_allocator_phys: usize,
    pub initrd_start: usize,
    pub initrd_size: usize,
}

/// К этому моменту:
/// - PC и SP - виртуальные адреса (TTBR1)
/// - TTBR0 ещё активен (identity mapping)
/// - GLOBAL_ALLOCATOR в PHASE_FROZEN
pub fn primary_main(
    dtb_phys: usize,
    higher_root_pa: usize,
    frame_allocator_phys: usize,
    initrd_start: usize,
    initrd_size: usize,
) -> ! {
    let handoff = BootHandoff {
        dtb_phys,
        higher_root_pa,
        frame_allocator_phys,
        initrd_start,
        initrd_size,
    };
    primary_main_impl(&handoff)
}

fn primary_main_impl(handoff: &BootHandoff) -> ! {
    let &BootHandoff {
        dtb_phys,
        higher_root_pa,
        frame_allocator_phys,
        initrd_start,
        initrd_size,
    } = handoff;

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
    // продление времени жизни ссылки до `'static` корректно.
    let device_tree: &'static fdt::devicetree::DeviceTree =
        unsafe { core::mem::transmute(&device_tree) };

    let mut driver_scanner = EmbeddedDriversScanner::new();
    let root = adapt_to_fdt_tree(device_tree)
        .root()
        .expect("DTB root node missing");
    driver_scanner
        .scan_and_probe(root, drivers())
        .expect("DTB nesting depth exceeds the supported limit");

    let memory_mapper: &'static (dyn memory::memory_mapper::MemoryMapper + Send + Sync) =
        result.memory_mapper;

    let address_space_factory: &'static (
                 dyn memory::memory_mapper::AddressSpaceFactory + Send + Sync
             ) = Box::leak(result.address_space_factory);
    let (userland_blob, userland_blob_phys): (
        Option<&'static [u8]>,
        Option<PageAlignedAddress>,
    ) = if initrd_size > 0 {
        let va = initrd_start + HIGHER_HALF_BASE;
        // SAFETY: initrd зарезервирован в MemoryLayout (RegionTag::Other), фреймы не переиспользуются.
        let blob = unsafe { core::slice::from_raw_parts(va as *const u8, initrd_size) };
        // База initrd обязана быть 4K-выровнена: образ маппится в userspace
        // постранично. Поддерживаемые загрузчики (QEMU virt, Android boot
        // ramdisk) это гарантируют.
        let phys =
            PageAlignedAddress::from_usize(initrd_start).expect("initrd base is 4K-aligned");
        (Some(blob), Some(phys))
    } else {
        (None, None)
    };

    let mmio_arena_base = PageAlignedVirtualAddress::new_unchecked(VirtualAddress::new(KMMIO_BASE));
    let mmio_arena_size =
        NonZeroUsize::new(KMMIO_MAX_SIZE).expect("KMMIO_MAX_SIZE must be non-zero");

    let kernel = Box::leak(Box::new(KernelContext::new(
        memory_mapper,
        address_space_factory,
        result.frame_allocator,
        mmio_arena_base,
        mmio_arena_size,
        userland_blob,
        userland_blob_phys,
        dtb_virt,
    )));

    #[cfg(feature = "power-semihosting")]
    kernelspace::power::install(crate::power::machine_off_exit_code);

    kmain::<Aarch64Context, _>(
        driver_scanner,
        kernel,
        buffered,
        SCHED_CONFIG,
        spawn_init_process_impl(),
    )
}

fn spawn_init_process_impl()
-> fn(&Scheduler<Aarch64Context, KernelTimerSource, Bootstrapped>, &mut KernelContext) {
    #[cfg(feature = "kernel-tests")]
    {
        kernelspace::kernel_tests::spawn_kernel_tests_process::<Aarch64Context>
    }
    #[cfg(not(feature = "kernel-tests"))]
    {
        kernelspace::init::spawn_init_process::<Aarch64Context>
    }
}
