//! Post-MMU фаза загрузки: инициализация драйверов, подсистем и передача управления kmain.

use alloc::{boxed::Box, collections::BTreeSet, sync::Arc, vec::Vec};
use core::num::NonZeroUsize;

use drivers_aarch64::drivers;
use drivers_common::scanner::EmbeddedDriversScanner;
use drivers_common_aarch64::{
    adapt_to_fdt_tree, device_windows::enumerate_device_windows, tree_ext::AddressSpace,
};
use io::buffered_writer::BufferedWriter;
use kernelspace::{
    kernel_context::{BootDtb, KernelContext, KmmioArena, UserlandImage},
    kmain::kmain,
    scheduler_bootstrap::KernelTimerSource,
};
use klog::{info, warn};
use memory::{
    AccessMask, MemoryRegion, page_round_up,
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

enum PrimaryBootError {
    UnalignedHigherRoot,
    DtbParse,
    DtbRootMissing,
    DtbScanDepth,
    UnalignedDtbBase,
    EmptyDtb,
    UnalignedInitrdBase,
}

impl core::fmt::Display for PrimaryBootError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let text = match self {
            PrimaryBootError::UnalignedHigherRoot => "higher-half root table PA is not 4K-aligned",
            PrimaryBootError::DtbParse => "DTB at the handed-off address is not parseable",
            PrimaryBootError::DtbRootMissing => "DTB has no root node",
            PrimaryBootError::DtbScanDepth => "DTB nesting depth exceeds the supported limit",
            PrimaryBootError::UnalignedDtbBase => "DTB base is not 4K-aligned",
            PrimaryBootError::EmptyDtb => "DTB totalsize is zero",
            PrimaryBootError::UnalignedInitrdBase => "initrd base is not 4K-aligned",
        };
        f.write_str(text)
    }
}

fn primary_main_impl(handoff: &BootHandoff) -> ! {
    match primary_boot(handoff) {
        Ok(never) => match never {},
        Err(error) => {
            klog::fatal!("Kernel boot failed: {error}");
            kernelspace::power::system_off(1)
        }
    }
}

fn primary_boot(handoff: &BootHandoff) -> Result<core::convert::Infallible, PrimaryBootError> {
    let &BootHandoff {
        dtb_phys,
        higher_root_pa,
        frame_allocator_phys,
        initrd_start,
        initrd_size,
    } = handoff;

    let higher_half_base =
        PageAlignedVirtualAddress::new_unchecked(VirtualAddress::new(HIGHER_HALF_BASE));
    let higher_root = PageAlignedAddress::from_usize(higher_root_pa)
        .ok_or(PrimaryBootError::UnalignedHigherRoot)?;
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
    let device_tree =
        fdt::devicetree::DeviceTree::from_ptr(dtb_virt).map_err(|_| PrimaryBootError::DtbParse)?;

    // SAFETY: DTB замаплен в higher-half и существует в течение всей жизни ядра, поэтому
    // продление времени жизни ссылки до `'static` корректно.
    let device_tree: &'static fdt::devicetree::DeviceTree =
        unsafe { core::mem::transmute(&device_tree) };

    let mut driver_scanner = EmbeddedDriversScanner::new();
    let root = adapt_to_fdt_tree(device_tree)
        .root()
        .ok_or(PrimaryBootError::DtbRootMissing)?;
    driver_scanner
        .scan_and_probe(root, drivers())
        .map_err(|_| PrimaryBootError::DtbScanDepth)?;

    // Device-MMIO окна для userspace-драйверов: узлы, под которые ядро
    // подобрало драйвер (GIC, PL011), исключены. Малформленные окна
    // пропускаются, не валя boot.
    let claimed: BTreeSet<usize> = driver_scanner.claimed_node_ids().collect();
    let mut device_regions = Vec::new();
    for window in enumerate_device_windows(&root, &claimed) {
        match device_window_to_region(&window) {
            Some(region) => device_regions.push(region),
            None => warn!(
                "device window {:#x}+{:#x} skipped: misaligned base",
                window.offset, window.size
            ),
        }
    }

    let memory_mapper: &'static (dyn memory::memory_mapper::MemoryMapper + Send + Sync) =
        result.memory_mapper;

    let address_space_factory: &'static (
                 dyn memory::memory_mapper::AddressSpaceFactory + Send + Sync
             ) = Box::leak(result.address_space_factory);

    let boot_dtb = BootDtb::new(
        PageAlignedAddress::from_usize(dtb_phys).ok_or(PrimaryBootError::UnalignedDtbBase)?,
        NonZeroUsize::new(device_tree.size()).ok_or(PrimaryBootError::EmptyDtb)?,
    );

    let userland_image = if initrd_size > 0 {
        let va = initrd_start + HIGHER_HALF_BASE;
        // SAFETY: initrd зарезервирован, фреймы не переиспользуются.
        let bytes = unsafe { core::slice::from_raw_parts(va as *const u8, initrd_size) };
        let phys_base = PageAlignedAddress::from_usize(initrd_start)
            .ok_or(PrimaryBootError::UnalignedInitrdBase)?;
        Some(UserlandImage { bytes, phys_base })
    } else {
        None
    };

    let kernel = Box::leak(Box::new(KernelContext::new(
        memory_mapper,
        address_space_factory,
        result.frame_allocator,
        KmmioArena {
            base: KMMIO_BASE,
            size: KMMIO_MAX_SIZE,
        },
        userland_image,
        device_regions,
        boot_dtb,
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

/// Строит Device-MMIO регион поверх окна FDT. `None` - база не выровнена на
/// страницу (окно пропускается, что сдвигает индексы последующих в наборе).
/// Размер округляется вверх до страницы; округление предполагает постраничную
/// изоляцию device-окон от kernel-owned MMIO (верно для плоской раскладки).
fn device_window_to_region(window: &AddressSpace) -> Option<Arc<MemoryRegion>> {
    let base = PageAlignedAddress::from_usize(window.offset)?;
    let size = page_round_up(NonZeroUsize::new(window.size)?)?;
    Some(Arc::new(MemoryRegion::create_physical_device(
        base,
        size,
        AccessMask::RW,
    )))
}

type InitTask = fn(
    &Scheduler<Aarch64Context, KernelTimerSource, Bootstrapped>,
    &mut KernelContext,
) -> Result<(), scheduler::SpawnError>;

fn spawn_init_process_impl() -> InitTask {
    #[cfg(feature = "kernel-tests")]
    {
        kernelspace::kernel_tests::spawn_kernel_tests_process::<Aarch64Context>
    }
    #[cfg(not(feature = "kernel-tests"))]
    {
        kernelspace::init::spawn_init_process::<Aarch64Context>
    }
}
