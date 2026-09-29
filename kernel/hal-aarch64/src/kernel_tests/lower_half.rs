#![allow(unsafe_code)]

use collections::{CriticalSection as _, DaifCriticalSection};
use kernel_tests::kernel_test;
use kernelspace::syscall_bridge;
use memory::{MemFlags, virtual_address::PageAlignedVirtualAddress};
use scheduler::{AddressSpace, ArchContext};

use super::probe_va;
use crate::{
    memory::{
        mmu::Mmu,
        regs::{
            common::EL1,
            tlb::TranslationLookasideBuffer,
            ttbr::{LowerHalf, TranslationTableBaseRegister},
        },
    },
    sched::Aarch64Context,
};

type Ttbr0 = TranslationTableBaseRegister<EL1, LowerHalf>;

const PAGE_SIZE: usize = memory::PAGE_SIZE.get();

const PROBE_VA: usize = probe_va::LOWER_HALF_WALK.base();

const PAR_FAULT: u64 = 1;
const PAR_FST_SHIFT: u64 = 1;
const PAR_FST_MASK: u64 = 0x3F;
const FST_TRANSLATION_FAULT_LEVEL_0: u64 = 0b00_0100;

fn translate_read(va: usize) -> u64 {
    let par: u64;
    // SAFETY: `at` не обращается к памяти и заполняет только `PAR_EL1` текущего CPU. Первый `isb`
    // применяет предыдущие записи системных регистров, второй упорядочивает запрос и чтение
    // результата.
    unsafe {
        core::arch::asm!(
            "isb",
            "at s1e1r, {va}",
            "isb",
            "mrs {par}, par_el1",
            va = in(reg) va,
            par = out(reg) par,
            options(nostack, preserves_flags),
        );
    }
    par
}

fn is_level_0_translation_fault(par: u64) -> bool {
    par & PAR_FAULT == PAR_FAULT
        && (par >> PAR_FST_SHIFT) & PAR_FST_MASK == FST_TRANSLATION_FAULT_LEVEL_0
}

#[kernel_test]
fn lower_half_walks_follow_address_space_and_stay_off_after_disable() {
    let factory =
        syscall_bridge::address_space_factory().expect("address space factory must be installed");
    let fa = syscall_bridge::frame_allocator().expect("FrameAllocator must be installed");

    let user_as = AddressSpace::new_user(factory).expect("create user AS");
    let mapper = user_as.mapper().expect("user variant has mapper");
    let frame = fa.allocate_frame().expect("RAM frame available");
    let va = PageAlignedVirtualAddress::from_usize(PROBE_VA).expect("PROBE_VA aligned");
    mapper
        .map_exact(va, frame.page_address(), PAGE_SIZE, MemFlags::user_rw())
        .expect("map_exact probe page as UserRW");

    let handle = user_as.handle().expect("user variant has handle");

    let irq_mask = DaifCriticalSection::enter();

    Aarch64Context::switch_address_space(Some(handle));
    let mapped = translate_read(PROBE_VA);

    Aarch64Context::switch_address_space(None);
    TranslationLookasideBuffer::<EL1>::invalidate();
    Ttbr0::set(handle.root);
    let after_kernel_switch = translate_read(PROBE_VA);

    Aarch64Context::switch_address_space(None);
    Aarch64Context::switch_address_space(Some(handle));
    let remapped = translate_read(PROBE_VA);

    Mmu::<EL1>::disable_lower_half();
    Ttbr0::set(handle.root);
    let after_disable = translate_read(PROBE_VA);

    Aarch64Context::switch_address_space(None);
    drop(irq_mask);

    kernel_tests::kassert_eq!(mapped & PAR_FAULT, 0);
    kernel_tests::kassert!(is_level_0_translation_fault(after_kernel_switch));
    kernel_tests::kassert_eq!(remapped & PAR_FAULT, 0);
    kernel_tests::kassert!(is_level_0_translation_fault(after_disable));

    drop(user_as);
    let _ = fa.deallocate_frame(frame);
}
