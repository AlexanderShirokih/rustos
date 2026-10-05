//! Идентификация CPU и per-CPU блок платформенного слоя.
//!
//! `TPIDR_EL1` хранит адрес [`CpuLocal`] текущего CPU. Ноль в регистре
//! означает, что блок ещё не установлен.

use core::{
    ptr,
    sync::atomic::{AtomicPtr, AtomicU16, Ordering},
};

use hal_common::cpu::{CpuAffinity, CpuTopology};
use scheduler::{CpuId, MAX_CPUS};
use spin::Once;

use crate::{read_sysreg, write_sysreg};

pub(crate) type Topology = CpuTopology<MAX_CPUS>;

static TOPOLOGY: Once<Topology> = Once::new();

static CPU_LOCALS: [CpuLocal; MAX_CPUS] = [const { CpuLocal::new() }; MAX_CPUS];

#[repr(C, align(128))]
pub(crate) struct CpuLocal {
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

    pub(crate) fn id(&self) -> CpuId {
        // `install_local` кладёт сюда уже проверенный идентификатор.
        CpuId::new(self.id.load(Ordering::Relaxed)).unwrap_or(CpuId::ZERO)
    }

    pub(crate) fn scheduler(&self) -> *mut () {
        self.scheduler.load(Ordering::Acquire)
    }

    pub(crate) fn set_scheduler(&self, scheduler: *mut ()) {
        self.scheduler.store(scheduler, Ordering::Release);
    }
}

pub(crate) fn init_topology(topology: Topology) {
    let _ = TOPOLOGY.call_once(|| topology);
}

pub(crate) fn count() -> usize {
    TOPOLOGY.get().map_or(1, Topology::len)
}

pub(crate) fn affinity(index: usize) -> Option<CpuAffinity> {
    TOPOLOGY.get()?.affinity(index)
}

pub(crate) fn logical_id(affinity: CpuAffinity) -> Option<CpuId> {
    let index = TOPOLOGY.get()?.index_of(affinity)?;
    u16::try_from(index).ok().and_then(CpuId::new)
}

pub(crate) fn confirm_current_id(claim: u64) -> Option<CpuId> {
    let topology = TOPOLOGY.get()?;
    let index = usize::try_from(claim).ok()?;

    if !topology.confirms(current_affinity(), index) {
        return None;
    }

    u16::try_from(index).ok().and_then(CpuId::new)
}

pub(crate) fn current_affinity() -> CpuAffinity {
    // SAFETY: MPIDR_EL1 доступен на EL1 и только читается.
    let mpidr = unsafe { read_sysreg!(mpidr_el1) };

    // Поля Aff0..Aff3 регистра совпадают с `reg` ARM-биндинга `cpus`, а
    // служебные биты `U`, `MT` и `RES1` лежат вне них и отбрасываются.
    CpuAffinity::from_reg(mpidr)
}

pub(crate) fn try_current_id() -> Option<CpuId> {
    match current() {
        Some(local) => Some(local.id()),
        // До публикации таблицы код исполняет только boot CPU.
        None if TOPOLOGY.get().is_none() => Some(CpuId::ZERO),
        None => logical_id(current_affinity()),
    }
}

pub(crate) fn current_id() -> CpuId {
    try_current_id().unwrap_or(CpuId::ZERO)
}

pub(crate) fn current() -> Option<&'static CpuLocal> {
    // SAFETY: TPIDR_EL1 либо нулевой (блок не установлен), либо хранит адрес
    // элемента `CPU_LOCALS`, записанный `install_local`.
    let raw = unsafe { read_sysreg!(tpidr_el1) } as usize;
    if raw == 0 {
        return None;
    }
    // SAFETY: см. выше; `CPU_LOCALS` статичен, ссылка остаётся действительной всё время работы
    // ядра.
    Some(unsafe { &*(raw as *const CpuLocal) })
}

pub(crate) fn install_local(id: CpuId) -> Option<&'static CpuLocal> {
    let local = CPU_LOCALS.get(id.as_index())?;
    local.id.store(id.raw(), Ordering::Relaxed);

    // SAFETY: TPIDR_EL1 - kernel-private регистр EL1; адрес элемента
    // статического массива валиден всё время работы ядра.
    unsafe { write_sysreg!(tpidr_el1, ptr::from_ref(local) as usize as u64) };
    Some(local)
}
