//! Идентификация CPU и per-CPU блок платформенного слоя.
//!
//! `TPIDR_EL1` хранит адрес [`CpuLocal`] текущего CPU; ноль в регистре
//! означает, что блок ещё не установлен.

use core::{
    ptr,
    sync::atomic::{AtomicPtr, AtomicU16, Ordering},
};

use hal_common::cpu::{CpuAffinity, CpuTopology};
use scheduler::{CpuId, MAX_CPUS};
use spin::Once;

use crate::{read_sysreg, write_sysreg};

/// Таблица топологии, рассчитанная на тот же предел, что и [`CpuId`].
pub type Topology = CpuTopology<MAX_CPUS>;

static TOPOLOGY: Once<Topology> = Once::new();

// Блоки живут всё время работы ядра - условие того, что их адрес допустимо
// держать в `TPIDR_EL1`.
static CPU_LOCALS: [CpuLocal; MAX_CPUS] = [const { CpuLocal::new() }; MAX_CPUS];

/// Per-CPU состояние платформенного слоя. Поле `scheduler` остаётся `null`
/// до bootstrap планировщика.
pub struct CpuLocal {
    id: AtomicU16,
    scheduler: AtomicPtr<()>,
}

impl CpuLocal {
    const fn new() -> Self {
        Self {
            id: AtomicU16::new(0),
            scheduler: AtomicPtr::new(ptr::null_mut()),
        }
    }

    pub fn id(&self) -> CpuId {
        // `install_local` кладёт сюда уже проверенный идентификатор.
        CpuId::new(self.id.load(Ordering::Relaxed)).unwrap_or(CpuId::ZERO)
    }

    /// Per-CPU состояние планировщика; `null` до его bootstrap.
    pub fn scheduler(&self) -> *mut () {
        self.scheduler.load(Ordering::Acquire)
    }

    pub fn set_scheduler(&self, scheduler: *mut ()) {
        self.scheduler.store(scheduler, Ordering::Release);
    }
}

/// Публикует таблицу топологии. Вызывается boot CPU один раз, до старта
/// вторичных CPU; повторный вызов таблицу не меняет.
pub fn init_topology(topology: Topology) {
    let _ = TOPOLOGY.call_once(|| topology);
}

/// Число CPU, известных ядру. До [`init_topology`] - один (boot CPU).
pub fn count() -> usize {
    TOPOLOGY.get().map_or(1, Topology::len)
}

/// Affinity CPU с логическим идентификатором `index`.
pub fn affinity(index: usize) -> Option<CpuAffinity> {
    TOPOLOGY.get()?.affinity(index)
}

/// Логический идентификатор CPU с заданным `affinity`.
pub fn logical_id(affinity: CpuAffinity) -> Option<CpuId> {
    let index = TOPOLOGY.get()?.index_of(affinity)?;
    u16::try_from(index).ok().and_then(CpuId::new)
}

/// Affinity текущего CPU из `MPIDR_EL1`.
pub fn current_affinity() -> CpuAffinity {
    // SAFETY: MPIDR_EL1 доступен на EL1 и только читается.
    let mpidr = unsafe { read_sysreg!(mpidr_el1) };

    // Поля Aff0..Aff3 регистра совпадают с `reg` ARM-биндинга `cpus`, а
    // служебные биты `U`, `MT` и `RES1` лежат вне них и отбрасываются.
    CpuAffinity::from_reg(mpidr)
}

/// Логический идентификатор текущего CPU. `None` - таблица опубликована,
/// но affinity в ней нет: такой CPU в работе не участвует.
pub fn try_current_id() -> Option<CpuId> {
    match current() {
        Some(local) => Some(local.id()),
        // До публикации таблицы код исполняет только boot CPU.
        None if TOPOLOGY.get().is_none() => Some(CpuId::ZERO),
        None => logical_id(current_affinity()),
    }
}

/// Логический идентификатор текущего CPU, приведённый к требованию
/// [`scheduler::ArchCpu`]: неизвестный CPU получает нулевой идентификатор.
/// Путь старта CPU обязан спрашивать [`try_current_id`].
pub fn current_id() -> CpuId {
    try_current_id().unwrap_or(CpuId::ZERO)
}

/// Per-CPU блок текущего CPU; `None`, пока он не установлен.
pub fn current() -> Option<&'static CpuLocal> {
    // SAFETY: TPIDR_EL1 либо нулевой (блок не установлен), либо хранит адрес
    // элемента `CPU_LOCALS`, записанный `install_local`.
    let raw = unsafe { read_sysreg!(tpidr_el1) } as usize;
    if raw == 0 {
        return None;
    }
    // SAFETY: см. выше; `CPU_LOCALS` статичен, ссылка живёт всё время работы ядра.
    Some(unsafe { &*(raw as *const CpuLocal) })
}

/// Закрепляет за текущим CPU блок с идентификатором `id` и публикует его
/// адрес в `TPIDR_EL1`. `None` - идентификатор вне [`MAX_CPUS`].
pub fn install_local(id: CpuId) -> Option<&'static CpuLocal> {
    let local = CPU_LOCALS.get(id.as_index())?;
    local.id.store(id.raw(), Ordering::Relaxed);

    // SAFETY: TPIDR_EL1 - kernel-private регистр EL1; адрес элемента
    // статического массива валиден всё время работы ядра.
    unsafe { write_sysreg!(tpidr_el1, ptr::from_ref(local) as usize as u64) };
    Some(local)
}
