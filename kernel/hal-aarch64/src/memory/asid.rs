//! Глобальный ASID-аллокатор для AArch64.
//!
//! Тонкий слой поверх `hal_aarch64_asid::GlobalAsidAllocator`: подставляет локальный сброс TLB,
//! который аллокатор выполняет на каждом CPU в первом `acquire` после rollover, и передаёт id
//! текущего CPU. По id аллокатор ведёт активный тег каждого CPU и не отдаёт этот тег другому
//! адресному пространству при rollover на другом ядре.

use core::sync::atomic::AtomicU64;

pub(crate) use hal_aarch64_asid::{GlobalAsidAllocator, unpack_asid};
use scheduler::{ArchCpu, MAX_CPUS};

use crate::{
    memory::regs::{common::EL1, tlb::TranslationLookasideBuffer},
    sched::Aarch64Cpu,
};

fn flush_local_tlb() {
    TranslationLookasideBuffer::<EL1>::invalidate();
}

static ASID_ALLOCATOR: GlobalAsidAllocator<MAX_CPUS> = GlobalAsidAllocator::new(flush_local_tlb);

pub(crate) fn init_global(width: hal_aarch64_asid::AsidWidth) {
    ASID_ALLOCATOR.init(width);
}

pub(crate) fn acquire(tag_slot: &AtomicU64) -> (u16, u64) {
    ASID_ALLOCATOR.acquire(tag_slot, Aarch64Cpu::current_id().raw())
}
