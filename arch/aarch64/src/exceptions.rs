use core::arch::global_asm;

global_asm!(
    r#"
.section .text
.balign 0x800                   // Таблица выровнена по 2KB

.global __exception_vector
__exception_vector:

// ───────────────────────────────────────────────────────────────
// Current EL with SP_EL0 (не используем, но должны быть)
// ───────────────────────────────────────────────────────────────
.balign 0x80
curr_el_sp0_sync:
    b       __exception_hang

.balign 0x80
curr_el_sp0_irq:
    b       __exception_hang

.balign 0x80
curr_el_sp0_fiq:
    b       __exception_hang

.balign 0x80
curr_el_sp0_serror:
    b       __exception_hang

// ───────────────────────────────────────────────────────────────
// Current EL with SP_ELx (это мы используем в ядре)
// ───────────────────────────────────────────────────────────────
.balign 0x80
curr_el_spx_sync:
    b       __exception_hang

.balign 0x80
curr_el_spx_irq:
    b       __exception_hang

.balign 0x80
curr_el_spx_fiq:
    b       __exception_hang

.balign 0x80
curr_el_spx_serror:
    b       __exception_hang

// ───────────────────────────────────────────────────────────────
// Lower EL using AArch64
// ───────────────────────────────────────────────────────────────
.balign 0x80
lower_el_aarch64_sync:
    b       __exception_hang

.balign 0x80
lower_el_aarch64_irq:
    b       __exception_hang

.balign 0x80
lower_el_aarch64_fiq:
    b       __exception_hang

.balign 0x80
lower_el_aarch64_serror:
    b       __exception_hang

// ───────────────────────────────────────────────────────────────
// Lower EL using AArch32
// ───────────────────────────────────────────────────────────────
.balign 0x80
lower_el_aarch32_sync:
    b       __exception_hang

.balign 0x80
lower_el_aarch32_irq:
    b       __exception_hang

.balign 0x80
lower_el_aarch32_fiq:
    b       __exception_hang

.balign 0x80
lower_el_aarch32_serror:
    b       __exception_hang

// ───────────────────────────────────────────────────────────────
// Общий обработчик: просто зависаем
// ───────────────────────────────────────────────────────────────
__exception_hang:
    msr     daifset, #0xf       // Маскируем все прерывания
1:
    wfe
    b       1b
"#
);

/// Установить VBAR_EL1 на нашу таблицу исключений
#[inline(always)]
pub fn init() {
    unsafe {
        core::arch::asm!(
        "adrp   {tmp}, __exception_vector",
        "add    {tmp}, {tmp}, #:lo12:__exception_vector",
        "msr    vbar_el1, {tmp}",
        "isb",
        tmp = out(reg) _,
        options(nostack, preserves_flags)
        );
    }
}
