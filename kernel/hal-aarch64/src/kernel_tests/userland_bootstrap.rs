//! E2E полной цепочки userland: `spawn_process` -> `run_bootstrap` -> rootkeeper -> ожидание завершения.

use alloc::vec::Vec;

use capability::{Capability, CapabilityTarget, Rights, SIGNALED, install_handle, signal_wait_one};
use kernel_tests::kernel_test;
use kernelspace::bootstrap::run_bootstrap;
use memory::physical_address::PageAlignedAddress;
use scheduler::{ArchContext, Priority, SchedulerServiceExt, SpawnConfig};

use crate::{consts::HIGHER_HALF_BASE, sched::Aarch64Context};

/// Bounded-бюджет ожидания как детектор провала: хэппи-пас будится сигналом
/// терминации за миллисекунды.
const WAIT_BUDGET_NS: u64 = 5_000_000_000;

#[kernel_test]
fn userland_bootstrap_log() {
    let blob = kernelspace::kernel_tests::userland_blob()
        .expect("userland blob must be present in test mode");

    // Физбаза blob в initrd - обратная трансляция higher-half direct map.
    let blob_phys = PageAlignedAddress::from_usize(blob.as_ptr() as usize - HIGHER_HALF_BASE)
        .expect("initrd blob base is page-aligned");

    let launch = kernelspace::bootstrap::spawn_process(
        kernelspace::kernel_tests::user_process_launcher().as_ref(),
        blob,
        blob_phys,
        Vec::new(),
        Aarch64Context::USER_VA_END,
    )
    .expect("spawn_process must succeed");

    kernel_tests::kassert_eq!(launch.info.initial_handle_id.raw().get(), 1 << 16);

    let process = launch.info.process_object.clone();
    let port = launch.port;
    let resources = launch.resources;

    kernelspace::kernel_tests::scheduler()
        .spawn(
            SpawnConfig::new("bootstrap-server").priority(Priority::normal()),
            move || run_bootstrap(&port, resources),
        )
        .expect("bootstrap server task spawn");

    // rootkeeper выходит кодом 0, только если magic образа совпала.
    let process_id = install_handle(Capability::new(
        CapabilityTarget::Process(process.clone()),
        Rights::READ,
    ))
    .expect("install bootstrap process handle");

    let observed = signal_wait_one(process_id, SIGNALED, Some(WAIT_BUDGET_NS))
        .expect("wait for bootstrap process exit");

    kernel_tests::kassert!(observed & SIGNALED != 0);
    kernel_tests::kassert_eq!(process.exit_code(), 0);
}
