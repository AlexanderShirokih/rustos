//! Post-MMU фаза загрузки: инициализация драйверов, подсистем и передача управления kmain.

use alloc::{boxed::Box, collections::BTreeSet, sync::Arc, vec::Vec};
use core::num::NonZeroUsize;

use drivers_aarch64::drivers;
use drivers_common::scanner::EmbeddedDriversScanner;
use drivers_common_aarch64::{
    adapt_to_fdt_tree, device_windows::enumerate_device_windows, fdt_adapter::FdtNode,
};
use fdt::{
    devicetree::DeviceTree,
    devicetreeext::{self, SkippedWindow},
};
use hal_common::cpu::CpuTopology;
use io::buffered_writer::BufferedWriter;
use kernelspace::{
    kernel_context::{BootDtb, KernelContext, KmmioArena, UserlandImage},
    kmain::kmain,
    scheduler_bootstrap::KernelTimerSource,
};
use klog::{info, warn};
use memory::{
    AccessMask, MemoryRegion, PAGE_SIZE,
    physical_address::{PageAlignedAddress, PhysicalAddress},
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};
use scheduler::{Bootstrapped, CpuId, Scheduler, SchedulerConfig};

use crate::{
    boot::boot_early::{_stack_bottom, _stack_top, BOOT_STACK_FILL},
    consts::{HIGHER_HALF_BASE, KMMIO_BASE, KMMIO_MAX_SIZE},
    cpu,
    exception::ExceptionVectors,
    memory::memory_setup::{Enabled, MemorySetup},
    sched::Aarch64Context,
};

#[cfg(feature = "short-quantum")]
const QUANTUM_NS: u64 = 500_000;
#[cfg(not(feature = "short-quantum"))]
const QUANTUM_NS: u64 = scheduler::DEFAULT_QUANTUM_NS;

const SCHED_CONFIG: SchedulerConfig = SchedulerConfig::new(32, 64).with_quantum_ns(QUANTUM_NS);

pub(super) struct BootHandoff {
    pub dtb_phys: usize,
    pub higher_root_pa: usize,
    /// Корень identity-таблиц, по которому вторичные CPU проходят включение MMU без смены
    /// адресации.
    pub lower_root_pa: usize,
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
    lower_root_pa: usize,
) -> ! {
    let handoff = BootHandoff {
        dtb_phys,
        higher_root_pa,
        lower_root_pa,
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
    EmptyDtb,
    BootStackExhausted,
    UnalignedLowerRoot,
}

impl core::fmt::Display for PrimaryBootError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let text = match self {
            PrimaryBootError::UnalignedHigherRoot => "higher-half root table PA is not 4K-aligned",
            PrimaryBootError::DtbParse => "DTB at the handed-off address is not parseable",
            PrimaryBootError::DtbRootMissing => "DTB has no root node",
            PrimaryBootError::DtbScanDepth => "DTB nesting depth exceeds the supported limit",
            PrimaryBootError::EmptyDtb => "DTB totalsize is zero",
            PrimaryBootError::BootStackExhausted => "pre-MMU phase used up the whole boot stack",
            PrimaryBootError::UnalignedLowerRoot => "identity root table PA is not 4K-aligned",
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
        lower_root_pa,
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

    report_boot_stack(&boot_stack_usage())?;

    let dtb_virt = dtb_phys + HIGHER_HALF_BASE;
    let device_tree =
        fdt::devicetree::DeviceTree::from_ptr(dtb_virt).map_err(|_| PrimaryBootError::DtbParse)?;

    // SAFETY: DTB замаплен в higher-half и существует в течение всей жизни ядра, поэтому
    // продление времени жизни ссылки до `'static` корректно.
    let device_tree: &'static fdt::devicetree::DeviceTree =
        unsafe { core::mem::transmute(&device_tree) };

    crate::power::install(device_tree);

    // До этой точки логический идентификатор CPU задан только значением по умолчанию,
    // поэтому подсистемы с per-CPU состоянием инициализируются ниже.
    init_cpu_topology(device_tree);

    crate::smp::prepare(
        device_tree,
        &crate::smp::TranslationRoots {
            lower: PageAlignedAddress::from_usize(lower_root_pa)
                .ok_or(PrimaryBootError::UnalignedLowerRoot)?,
            higher: higher_root,
        },
    );

    // Консоль назначает загрузчик через `/chosen/stdout-path`; без назначения
    // роль остаётся за первым подходящим узлом.
    let console_node = hal_common::scanner::designated_console(device_tree).map(|node| node.key());
    let mut driver_scanner = EmbeddedDriversScanner::new(console_node);
    let root = adapt_to_fdt_tree(device_tree)
        .root()
        .ok_or(PrimaryBootError::DtbRootMissing)?;
    driver_scanner
        .scan_and_probe(root, drivers())
        .map_err(|_| PrimaryBootError::DtbScanDepth)?;

    // Device-MMIO регионы для userspace-драйверов из целых страниц: окна узлов, под которые
    // ядро подобрало драйвер (GIC, PL011), и их поддеревья исключены. Окно в странице такого
    // узла либо его потомка и окно поверх RAM вне `no-map` региона пропускаются, загрузка
    // продолжается.
    let claimed: BTreeSet<usize> = driver_scanner.claimed_node_ids().collect();
    let device_regions = collect_device_regions(device_tree, &root, &claimed);

    let memory_mapper: &'static (dyn memory::memory_mapper::MemoryMapper + Send + Sync) =
        result.memory_mapper;

    let address_space_factory: &'static (
                 dyn memory::memory_mapper::AddressSpaceFactory + Send + Sync
             ) = Box::leak(result.address_space_factory);

    let boot_dtb = BootDtb::new(
        PhysicalAddress::new(dtb_phys),
        NonZeroUsize::new(device_tree.size()).ok_or(PrimaryBootError::EmptyDtb)?,
    );

    let userland_image = if initrd_size > 0 {
        let va = initrd_start + HIGHER_HALF_BASE;
        // SAFETY: initrd зарезервирован, фреймы не переиспользуются.
        let bytes = unsafe { core::slice::from_raw_parts(va as *const u8, initrd_size) };
        Some(UserlandImage {
            bytes,
            phys_base: PhysicalAddress::new(initrd_start),
        })
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

    kmain::<Aarch64Context, _>(
        driver_scanner,
        kernel,
        buffered,
        SCHED_CONFIG,
        spawn_initial_process_impl(),
    )
}

struct BootStackUsage {
    used: usize,
    size: usize,
}

impl BootStackUsage {
    fn is_exhausted(&self) -> bool {
        self.used == self.size
    }
}

fn boot_stack_usage() -> BootStackUsage {
    let bottom = (&raw const _stack_bottom) as usize;
    let top = (&raw const _stack_top) as usize;
    let size = top.saturating_sub(bottom);

    let mut address = bottom;
    while address < top {
        // SAFETY: адрес лежит в `.stack` между символами линкера, выровнен на
        // 8 (граница секции кратна 16) и замаплен в higher-half.
        let word = unsafe { (address as *const u64).read_volatile() };
        if word != BOOT_STACK_FILL {
            break;
        }
        address += size_of::<u64>();
    }

    BootStackUsage {
        used: top.saturating_sub(address),
        size,
    }
}

fn report_boot_stack(usage: &BootStackUsage) -> Result<(), PrimaryBootError> {
    if usage.is_exhausted() {
        // Ниже `_stack_bottom` линкер кладёт `.bss`: продолжать загрузку по
        // повреждённому состоянию нельзя.
        return Err(PrimaryBootError::BootStackExhausted);
    }

    info!("boot stack: {} of {} bytes used", usage.used, usage.size);
    Ok(())
}

fn init_cpu_topology(device_tree: &'static fdt::devicetree::DeviceTree<'static>) {
    let boot = cpu::current_affinity();
    cpu::init_topology(CpuTopology::parse(device_tree, boot));

    // Нулевой индекс закреплён за boot CPU построением таблицы топологии,
    // поэтому блок для него в `CPU_LOCALS` есть всегда.
    let _boot_local = cpu::install_local(CpuId::ZERO);

    info!(
        "CPU topology: {} CPU(s), boot affinity {:#x}",
        cpu::count(),
        boot.raw()
    );
}

fn collect_device_regions(
    device_tree: &DeviceTree,
    root: &FdtNode,
    claimed: &BTreeSet<usize>,
) -> Vec<Arc<MemoryRegion>> {
    let mut windows = enumerate_device_windows(root, claimed);
    let regions =
        devicetreeext::page_regions(device_tree, &mut windows, PAGE_SIZE, |window, reason| {
            warn!(
                "device window {:#x}+{:#x} skipped: {}",
                window.offset,
                window.size,
                skip_reason(reason)
            );
        });

    regions
        .filter_map(|region| {
            let base = PageAlignedAddress::from_usize(region.offset)?;
            let size = NonZeroUsize::new(region.size)?;
            Some(Arc::new(MemoryRegion::create_physical_device(
                base,
                size,
                AccessMask::RW,
            )))
        })
        .collect()
}

const fn skip_reason(reason: SkippedWindow) -> &'static str {
    match reason {
        SkippedWindow::SharesExcludedPage => "shares a page with a kernel-owned device",
        SkippedWindow::Inadmissible => {
            "pages overlap RAM outside a no-map region or exceed the address space"
        }
    }
}

type InitialProcess = fn(
    &Scheduler<Aarch64Context, KernelTimerSource, Bootstrapped>,
    &mut KernelContext,
) -> Result<(), scheduler::SpawnError>;

fn spawn_initial_process_impl() -> InitialProcess {
    #[cfg(feature = "kernel-tests")]
    {
        kernelspace::kernel_tests::spawn_kernel_tests_process::<Aarch64Context>
    }
    #[cfg(not(feature = "kernel-tests"))]
    {
        kernelspace::initial_process::spawn_initial_process::<Aarch64Context>
    }
}
