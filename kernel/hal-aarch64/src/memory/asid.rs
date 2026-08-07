//! Глобальный ASID-аллокатор для AArch64.
//!
//! Тонкий слой поверх `hal_aarch64_asid::GlobalAsidAllocator`: подставляет
//! callback для full-flush TLB во всём inner-shareable домене на rollover
//! и подтягивает текущий CPU id, чтобы аллокатор удерживал per-CPU
//! "активный ASID" и не реиспользовал его при rollover на другом ядре.

use core::sync::atomic::AtomicU64;

pub use hal_aarch64_asid::{GlobalAsidAllocator, unpack_asid};
use scheduler::{ArchCpu, MAX_CPUS};

use crate::{
    memory::regs::{common::EL1, tlb::TranslationLookasideBuffer},
    sched::Aarch64Cpu,
};

fn flush_all_inner_shareable() {
    TranslationLookasideBuffer::<EL1>::invalidate_global_inner_shareable();
}

static ASID_ALLOCATOR: GlobalAsidAllocator<MAX_CPUS> =
    GlobalAsidAllocator::new(flush_all_inner_shareable);

/// Настраивает глобальный аллокатор ASID шириной, объявленной железом.
/// Вызывается на загрузке boot CPU, после включения MMU.
pub fn init_global(width: hal_aarch64_asid::AsidWidth) {
    ASID_ALLOCATOR.init(width);
}

/// Lazy acquire ASID для текущего CPU. Возвращает (asid, raw_tag).
pub fn acquire(tag_slot: &AtomicU64) -> (u16, u64) {
    ASID_ALLOCATOR.acquire(tag_slot, Aarch64Cpu::current_id().raw())
}
