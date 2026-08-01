//! Подъём вторичных CPU.
//!
//! CPU поднимаются по одному: следующий стартует после того, как предыдущий
//! доложил о выходе в post-MMU код.

mod entry;
mod handoff;
mod start_method;

use core::hint::spin_loop;

use drivers_common::services::interrupts::CpuInterfaceError;
use fdt::devicetree::DeviceTree;
use hal_common::{cpu::CpuAffinity, smp::StartMethod};
use klog::{info, warn};
use memory::physical_address::PageAlignedAddress;
use spin::Once;
use start_method::StartError;

use crate::cpu;

// Ожидание готовности идёт до инициализации таймера и измеряется итерациями.
const READY_POLL_ATTEMPTS: usize = 100_000_000;

/// Корни трансляции, которыми стартующий CPU включает MMU.
pub struct TranslationRoots {
    /// Корень identity-отображения: он держит PC живым в момент включения MMU.
    pub lower: PageAlignedAddress,
    /// Корень higher-half.
    pub higher: PageAlignedAddress,
}

static DEVICE_TREE: Once<&'static DeviceTree<'static>> = Once::new();

/// Запоминает параметры подъёма. Вызывается boot CPU в post-MMU фазе, до
/// инициализации драйверов.
pub fn prepare(device_tree: &'static DeviceTree<'static>, roots: &TranslationRoots) {
    handoff::publish_roots(
        roots.lower.as_usize() as u64,
        roots.higher.as_usize() as u64,
    );
    let _ = DEVICE_TREE.call_once(|| device_tree);
}

/// Поднимает все CPU из таблицы топологии, кроме текущего.
///
/// Вызывается boot CPU после инициализации контроллера прерываний: стартующий
/// CPU настраивает свой интерфейс контроллера, а тот требует уже настроенного
/// distributor.
pub fn start_secondaries() {
    let Some(device_tree) = DEVICE_TREE.get().copied() else {
        warn!("SMP: bring-up requested before the device tree was published");
        return;
    };

    let Some(boot) = cpu::try_current_id() else {
        warn!("SMP: the topology does not describe the CPU that runs bring-up");
        return;
    };

    let psci = hal_common::smp::psci_method(device_tree);
    let entry_point = entry::entry_point_physical();

    let mut started = 0usize;
    for index in 0..cpu::count() {
        let Some(affinity) = cpu::affinity(index) else {
            continue;
        };
        if index == boot.as_index() {
            continue;
        }

        match start_one(device_tree, psci, index, affinity, entry_point) {
            Ok(()) => started += 1,
            Err(error) if error.leaves_slot_claimed() => {
                // Слот параметров описывает ровно один стартующий CPU.
                // Переназначить его, пока прежний владелец может быть жив,
                // значит отдать двум CPU общий стек и общий per-CPU блок.
                warn!("SMP: bring-up stopped at CPU {index}: {error}");
                break;
            }
            Err(error) => warn!("CPU {index} did not start: {error}"),
        }
    }

    info!("SMP: {} of {} CPU(s) online", started + 1, cpu::count());
}

fn start_one(
    device_tree: &DeviceTree<'_>,
    psci: Option<StartMethod>,
    index: usize,
    affinity: CpuAffinity,
    entry_point: usize,
) -> Result<(), StartError> {
    // Способ объявляет узел самого CPU; `/psci` - общий fallback.
    let method = cpu_node_method(device_tree, affinity)
        .or(psci)
        .ok_or(StartError::MethodUnknown)?;

    let expected = handoff::online_count() + 1;
    handoff::publish_pending_id(index as u64);

    // SAFETY: `entry_point` - физический адрес точки входа вторичного CPU, а
    // сам CPU кода ядра ещё не исполняет.
    unsafe { start_method::start(method, affinity, entry_point)? };

    wait_ready(expected)
}

fn wait_ready(expected: usize) -> Result<(), StartError> {
    for _ in 0..READY_POLL_ATTEMPTS {
        if handoff::online_count() >= expected {
            return Ok(());
        }
        spin_loop();
    }

    Err(StartError::NotReady)
}

fn cpu_node_method(device_tree: &DeviceTree<'_>, affinity: CpuAffinity) -> Option<StartMethod> {
    let cpus = device_tree.find("/cpus")?;

    cpus.children()
        .filter(|node| hal_common::cpu::node_affinity(&cpus, node) == Some(affinity))
        .find_map(|node| hal_common::smp::node_method(&node))
}

fn per_cpu_init() -> Result<(), CpuInterfaceError> {
    kernelspace::irq_bridge::init_interrupts_on_this_cpu()
}
