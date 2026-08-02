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

use crate::{cpu, read_sysreg};

const READY_TIMEOUT_MS: u64 = 500;

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

    let Some(counter) = Counter::probe() else {
        warn!("SMP: the firmware left the counter frequency unset, bring-up cannot bound the wait");
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

        // Номер стека - число уже поднятых CPU: стек занимает только тот, кто
        // стартовал, а после отказа, не оставившего слот занятым, номер
        // достаётся следующему CPU.
        let Some(stack) = handoff::SecondaryStackIndex::new(started) else {
            warn!("SMP: no boot stack left for CPU {index}");
            break;
        };

        match start_one(
            device_tree,
            psci,
            index,
            stack,
            affinity,
            entry_point,
            &counter,
        ) {
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

    handoff::ONLINE.close();
    info!("SMP: {} of {} CPU(s) online", online_cpus(), cpu::count());
}

/// Число CPU, работающих под управлением планировщика: вторичные, дошедшие до
/// post-MMU кода, и boot CPU.
pub fn online_cpus() -> usize {
    handoff::ONLINE.count() + 1
}

fn start_one(
    device_tree: &DeviceTree<'_>,
    psci: Option<StartMethod>,
    index: usize,
    stack: handoff::SecondaryStackIndex,
    affinity: CpuAffinity,
    entry_point: usize,
    counter: &Counter,
) -> Result<(), StartError> {
    // Способ объявляет узел самого CPU; `/psci` - общий fallback.
    let method = cpu_node_method(device_tree, affinity)
        .or(psci)
        .ok_or(StartError::MethodUnknown)?;

    let expected = handoff::ONLINE.count() + 1;
    handoff::publish_pending(index as u64, stack);

    // SAFETY: `entry_point` - физический адрес точки входа вторичного CPU, а
    // сам CPU кода ядра ещё не исполняет.
    unsafe { start_method::start(method, affinity, entry_point)? };

    wait_ready(expected, counter)
}

fn wait_ready(expected: usize, counter: &Counter) -> Result<(), StartError> {
    let deadline = counter.deadline_after_ms(READY_TIMEOUT_MS);

    while !Counter::passed(deadline) {
        if handoff::ONLINE.count() >= expected {
            return Ok(());
        }
        spin_loop();
    }

    Err(StartError::NotReady)
}

struct Counter {
    frequency: u64,
}

const MIN_COUNTER_HZ: u64 = 1_000;
const MAX_COUNTER_HZ: u64 = 100_000_000_000;

impl Counter {
    fn probe() -> Option<Self> {
        // SAFETY: CNTFRQ_EL0 доступен на EL1 и только читается.
        let frequency = unsafe { read_sysreg!(cntfrq_el0) };

        (MIN_COUNTER_HZ..=MAX_COUNTER_HZ)
            .contains(&frequency)
            .then_some(Self { frequency })
    }

    fn deadline_after_ms(&self, timeout_ms: u64) -> u64 {
        let ticks = timeout_ms.saturating_mul(self.frequency) / 1_000;

        Self::now().saturating_add(ticks)
    }

    fn passed(deadline: u64) -> bool {
        Self::now() >= deadline
    }

    fn now() -> u64 {
        // SAFETY: CNTPCT_EL0 доступен на EL1 и только читается.
        unsafe { read_sysreg!(cntpct_el0) }
    }
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
