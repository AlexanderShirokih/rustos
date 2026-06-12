//! Pre-MMU фаза загрузки: protocol-agnostic часть.
//!
//! Ограничения этой фазы (higher-half VMA, MMU выключен):
//! - Абсолютные указатели в данных (vtable, fat ptr data) хранят VMA -> недоступны.
//! - Нельзя использовать `dyn Trait` dispatch, `klog`.

use core::{arch::naked_asm, hint::spin_loop};

use fdt::devicetree::DeviceTree;
use hal_common::boot::{BootInfo, BootPayloadRange, HwDescription};
use memory::virtual_address::{PageAlignedVirtualAddress, VirtualAddress};

use super::boot_primary::primary_main;
use crate::{
    consts::HIGHER_HALF_BASE,
    memory::{
        memory_setup::{Early, Enabled, MemorySetup},
        setup::build_memory_layout,
    },
};

unsafe extern "C" {
    pub(super) static _stack_top: u8;
    pub(super) static _bss_start: u8;
    pub(super) static _bss_end: u8;
}

/// Pre-MMU фаза: BootInfo -> память -> bump allocator -> page tables -> MMU -> jump.
pub(super) fn boot_main(boot_info: &BootInfo) -> Result<(), ()> {
    let dtb_phys = match boot_info.hw_description {
        HwDescription::Fdt(ref blob) => blob.phys.as_usize(),
        HwDescription::Acpi(_) | _ => return Err(()),
    };

    let device_tree = DeviceTree::from_ptr(dtb_phys).map_err(|_| ())?;

    // Строим раскладку памяти из DTB + символов линкера (физические адреса через adrp)
    let memory_layout =
        build_memory_layout(&device_tree, boot_info.userland_blob).map_err(|_| ())?;

    // Создаём MemorySetup и устанавливаем bump-аллокатор
    let early_setup = MemorySetup::<Early>::create(memory_layout).map_err(|_| ())?;
    let installed = early_setup.install();

    // Подготавливаем таблицы страниц (freeze bump, create frame allocator, map RAM)
    let prepared = installed.prepare().map_err(|_| ())?;

    // Маппим все регионы и включаем MMU
    let higher_half_base =
        PageAlignedVirtualAddress::new_unchecked(VirtualAddress::new(HIGHER_HALF_BASE));
    let enabled = prepared.enable(higher_half_base).map_err(|_| ())?;

    let Enabled {
        roots,
        frame_allocator,
        ..
    } = enabled.state;
    let higher_root_pa = roots.higher_pa.as_usize();
    let frame_allocator_phys = core::ptr::from_ref(frame_allocator) as usize;
    let initrd_start = boot_info.userland_blob.map_or(0, |r| r.start().as_usize());
    let initrd_size = boot_info
        .userland_blob
        .map_or(0, BootPayloadRange::size_bytes);

    // Прыжок в higher half - управление передаётся в primary_main и не возвращается
    // SAFETY: MMU включён, TTBR1 содержит корректный маппинг higher-half.
    unsafe {
        jump_to_higher_half(
            dtb_phys,
            higher_root_pa,
            frame_allocator_phys,
            initrd_start,
            initrd_size,
        );
    };

    loop {
        spin_loop();
    }
}

/// Прыжок с identity-адреса на higher-half адрес, переключение стека, вызов primary_main.
/// После прыжка управление передаётся в `primary_main` и не возвращается.
///
/// # Safety
///
/// MMU должен быть включён. TTBR1 должен содержать корректный маппинг higher-half.
/// TTBR0 должен покрывать текущий PC (identity mapping ещё нужен для инструкции `br x0`).
#[unsafe(naked)]
unsafe extern "C" fn jump_to_higher_half(
    _dtb_phys: usize,
    _higher_root_pa: usize,
    _frame_allocator_phys: usize,
    _initrd_start: usize,
    _initrd_size: usize,
) {
    const HALF_47_32: u64 = ((HIGHER_HALF_BASE as u64) >> 32) & 0xFFFF;
    const HALF_63_48: u64 = ((HIGHER_HALF_BASE as u64) >> 48) & 0xFFFF;

    naked_asm!(
        // extern "C" ABI (AArch64): аргументы в x0-x4:
        //   x0 = dtb_phys
        //   x1 = higher_root_pa
        //   x2 = frame_allocator_phys
        //   x3 = initrd_start
        //   x4 = initrd_size
        "mov x19, x0",
        "mov x20, x1",
        "mov x21, x2",
        "mov x22, x3",
        "mov x23, x4",

        // Вычисляем виртуальный адрес метки 1f и прыгаем на него
        "adr x0, 1f",
        "movz x1, #{half_32}, lsl #32",
        "movk x1, #{half_48}, lsl #48",
        "add x0, x0, x1",
        "br x0",                        // PC теперь виртуальный (TTBR1)

        "1:",
        // Переключаем стек на виртуальный адрес.
        // ldr загружает значение VMA из literal pool (замапленного через TTBR1).
        "ldr x1, ={stack_top}",
        "mov sp, x1",

        // Передаём параметры в primary_main:
        // x0=dtb_phys, x1=higher_root_pa, x2=frame_allocator_phys, x3=initrd_start, x4=initrd_size
        "mov x0, x19",
        "mov x1, x20",
        "mov x2, x21",
        "mov x3, x22",
        "mov x4, x23",
        "b {primary_main}",

        // Сюда не должны добраться
        "2:",
        "wfi",
        "b 2b",

        half_32 = const HALF_47_32,
        half_48 = const HALF_63_48,
        stack_top = sym _stack_top,
        primary_main = sym primary_main,
    )
}
