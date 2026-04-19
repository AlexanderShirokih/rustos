//! Post-MMU фаза загрузки: инициализация драйверов, подсистем и передача управления kmain.

use alloc::{boxed::Box, sync::Arc};

use drivers_aarch64::drivers;
use drivers_common::{
    CapabilityStoreExt, CapabilityStoreMutExt,
    scanner::DriverScanner,
    services::{
        scheduler::{Priority, SchedulerService, SchedulerServiceExt, SpawnConfig},
        timer::{TickHandler, TimerService},
    },
};
use drivers_common_aarch64::adapt_tree;
use io::buffered_writer::BufferedWriter;
use kernel::{
    kernel_context::KernelContext,
    kmain::kmain,
    sched::{Scheduler, TimerSource, Uninit},
};
use klog::{debug, info, set_stdout};
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
    sched::Aarch64Context,
};

const SCHED_PRIO_LEVELS: usize = 32;
const SCHED_CPU_COUNT: usize = 1;
const SCHED_THREAD_COUNT: usize = 64;

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
    start_scheduler(kernel)
}

struct TimerSourceAdapter(Arc<dyn TimerService>);

impl TimerSource for TimerSourceAdapter {
    fn now_ns(&self) -> u64 {
        self.0.now_ns()
    }

    fn schedule_next(&self, deadline_ns: u64) {
        self.0.schedule_next(deadline_ns);
    }
}

fn start_scheduler(kernel: &mut KernelContext) -> ! {
    let timer = kernel.with_runtime_state(|caps, _| {
        caps.require_service::<dyn TimerService>()
            .expect("TimerService must be available before scheduler startup")
    });

    let scheduler = Scheduler::<
        Aarch64Context,
        TimerSourceAdapter,
        Uninit,
        SCHED_PRIO_LEVELS,
        SCHED_CPU_COUNT,
        SCHED_THREAD_COUNT,
    >::new(TimerSourceAdapter(timer.clone()))
    .bootstrap();

    let scheduler_handle = Arc::new(scheduler.handle());
    let scheduler_service: Arc<dyn SchedulerService> = scheduler_handle.clone();
    let tick_handler: Arc<dyn TickHandler> = scheduler_handle;

    kernel.with_runtime_state(|caps, _| {
        caps.provide_service::<dyn SchedulerService>(scheduler_service.clone())
            .expect("SchedulerService registration must succeed");
    });

    timer.set_handler(tick_handler);
    spawn_demo_processes(scheduler_service);
    scheduler.start()
}

fn spawn_demo_processes(scheduler: Arc<dyn SchedulerService>) {
    for (idx, period_ms) in [(1_u32, 100_u64), (2, 300), (3, 700)] {
        let thread_scheduler = scheduler.clone();
        scheduler
            .spawn(
                SpawnConfig::new("demo").priority(Priority::normal()),
                move || loop {
                    klog::info!("Process {idx} tick");
                    thread_scheduler.sleep_ms(period_ms);
                },
            )
            .expect("demo process spawn must succeed");
    }
}
