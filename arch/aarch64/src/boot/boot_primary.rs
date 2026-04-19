//! Post-MMU фаза загрузки: инициализация драйверов, подсистем и передача управления kmain.

use alloc::{boxed::Box, sync::Arc};

use drivers_aarch64::drivers;
use drivers_common::{
    scanner::DriverScanner,
    services::scheduler::{Priority, SchedulerService, SchedulerServiceExt, SpawnConfig},
};
use drivers_common_aarch64::adapt_tree;
use io::buffered_writer::BufferedWriter;
use kernel::{
    kernel_context::KernelContext,
    kmain::kmain,
    sched::{Bootstrapped, KernelTimerSource, Scheduler, bootstrap_scheduler},
};
use klog::{debug, info};
use memory::{
    physical_address::{PageAlignedAddress, PhysicalAddress},
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};

use crate::{
    HIGHER_HALF_BASE,
    exception::ExceptionVectors,
    memory::{
        memory_setup::{Enabled, MemorySetup},
        mmu::Mmu,
    },
    sched::{Aarch64Cpu, Aarch64Context},
};
use kernel::sched::ArchCpu;

const SCHED_PRIO_LEVELS: usize = 32;
const SCHED_CPU_COUNT: usize = 1;
const SCHED_THREAD_COUNT: usize = 64;

type AarchScheduler = Scheduler<
    Aarch64Context,
    KernelTimerSource,
    Bootstrapped,
    SCHED_PRIO_LEVELS,
    SCHED_CPU_COUNT,
    SCHED_THREAD_COUNT,
>;

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

    Mmu::new().disable_lower_half();

    ExceptionVectors::instance().install();

    let buffered = Box::leak(Box::new(BufferedWriter::new()));
    klog::set_stdout(buffered);

    info!("primary_main: post-MMU initialization complete");
    debug!("primary_main: TTBR0 (identity) cleared, TTBR1 (higher-half) active");

    let dtb_virt = dtb_phys + HIGHER_HALF_BASE;

    let device_tree = fdt::devicetree::DeviceTree::from_ptr(dtb_virt)
        .expect("Failed to parse DTB at virtual address");

    let device_tree: &'static fdt::devicetree::DeviceTree =
        unsafe { core::mem::transmute(&device_tree) };

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
    boot_scheduler(kernel)
}

fn boot_scheduler(kernel: &mut KernelContext) -> ! {
    // Гарантия: scheduler bootstrap'ится при замаскированных IRQ. Тики
    // обработчика прилетят только после первого `enable_preemption()` внутри
    // trampoline уже выбранного потока.
    Aarch64Cpu::disable_preemption();

    let scheduler: AarchScheduler = bootstrap_scheduler::<
        Aarch64Context,
        SCHED_PRIO_LEVELS,
        SCHED_CPU_COUNT,
        SCHED_THREAD_COUNT,
    >(kernel);

    let scheduler_service = kernel.with_runtime_state(|caps, _| {
        use drivers_common::CapabilityStoreExt;
        caps.require_service::<dyn SchedulerService>()
            .expect("SchedulerService must be registered by bootstrap_scheduler")
    });

    spawn_demo_processes(&scheduler, scheduler_service);

    scheduler.start()
}

fn spawn_demo_processes(scheduler: &AarchScheduler, service: Arc<dyn SchedulerService>) {
    for (idx, period_ms) in [(1_u32, 100_u64), (2, 300), (3, 700)] {
        let thread_service = service.clone();
        scheduler
            .spawn(
                SpawnConfig::new("demo").priority(Priority::normal()),
                move || loop {
                    klog::info!("Process {idx} tick");
                    thread_service.sleep_ms(period_ms);
                },
            )
            .expect("demo process spawn must succeed");
    }
}
