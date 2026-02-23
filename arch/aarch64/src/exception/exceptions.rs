use core::arch::{asm, global_asm, naked_asm};
use core::fmt::Write;
use crate::write_sysreg;

use collections::StaticString;

use super::esr::Esr;
use super::gpreg::GpReg;

/// Ёмкость stack-буфера для диагностики исключений (байт).
const EXCEPTION_BUF_SIZE: usize = 2048;

// Размер фрейма должен быть кратен 16 (требование AArch64 ABI для выравнивания стека).
const _: () = assert!(size_of::<ExceptionFrame>().is_multiple_of(16));

/// Контекст процессора, сохраняемый при входе в обработчик исключения.
#[repr(C)]
pub struct ExceptionFrame {
    /// Регистры общего назначения x0–x30.
    pub regs: [GpReg; 31],
    /// Указатель стека на момент исключения (до выделения фрейма).
    pub sp: u64,
    /// Exception Link Register — адрес возврата.
    pub elr: u64,
    /// Saved Program Status Register.
    pub spsr: u64,
    /// Exception Syndrome Register — описание причины исключения.
    pub esr: Esr,
    /// Fault Address Register — адрес, вызвавший исключение (для aborts).
    pub far: u64,
}

/// Тип исключения, определяемый вектором входа.
#[repr(u64)]
#[derive(Debug, Clone, Copy)]
pub enum ExceptionKind {
    /// Синхронное исключение (data abort, instruction abort, SVC, BRK и т.д.).
    Sync = 0,
    /// Аппаратное прерывание (IRQ).
    Irq = 1,
    /// Fast Interrupt Request.
    Fiq = 2,
    /// System Error (асинхронная аппаратная ошибка).
    SError = 3,
}

// Таблица векторов исключений
global_asm!(
    ".balign 2048",
    ".global exception_vectors",
    "exception_vectors:",

    // Группа 0: Current EL with SP_EL0
    ".balign 128",
    "b {sync_current_el_sp0}",
    ".balign 128",
    "b {irq_current_el_sp0}",
    ".balign 128",
    "b {fiq_current_el_sp0}",
    ".balign 128",
    "b {serror_current_el_sp0}",

    // Группа 1: Current EL with SP_ELx
    ".balign 128",
    "b {sync_current_el_spx}",
    ".balign 128",
    "b {irq_current_el_spx}",
    ".balign 128",
    "b {fiq_current_el_spx}",
    ".balign 128",
    "b {serror_current_el_spx}",

    // Группа 2: Lower EL using AArch64
    ".balign 128",
    "b {sync_lower_el_a64}",
    ".balign 128",
    "b {irq_lower_el_a64}",
    ".balign 128",
    "b {fiq_lower_el_a64}",
    ".balign 128",
    "b {serror_lower_el_a64}",

    // Группа 3: Lower EL using AArch32
    ".balign 128",
    "b {sync_lower_el_a32}",
    ".balign 128",
    "b {irq_lower_el_a32}",
    ".balign 128",
    "b {fiq_lower_el_a32}",
    ".balign 128",
    "b {serror_lower_el_a32}",

    sync_current_el_sp0   = sym sync_current_el_sp0,
    irq_current_el_sp0    = sym irq_current_el_sp0,
    fiq_current_el_sp0    = sym fiq_current_el_sp0,
    serror_current_el_sp0 = sym serror_current_el_sp0,

    sync_current_el_spx   = sym sync_current_el_spx,
    irq_current_el_spx    = sym irq_current_el_spx,
    fiq_current_el_spx    = sym fiq_current_el_spx,
    serror_current_el_spx = sym serror_current_el_spx,

    sync_lower_el_a64     = sym sync_lower_el_a64,
    irq_lower_el_a64      = sym irq_lower_el_a64,
    fiq_lower_el_a64      = sym fiq_lower_el_a64,
    serror_lower_el_a64   = sym serror_lower_el_a64,

    sync_lower_el_a32     = sym sync_lower_el_a32,
    irq_lower_el_a32      = sym irq_lower_el_a32,
    fiq_lower_el_a32      = sym fiq_lower_el_a32,
    serror_lower_el_a32   = sym serror_lower_el_a32,
);

/// Таблица векторов исключений AArch64.
pub struct ExceptionVectors {
    _table: [u8; 2048],
}

impl ExceptionVectors {
    /// Возвращает ссылку на таблицу по identity-mapped адресу.
    pub fn instance() -> &'static Self {
        #[allow(improper_ctypes)]
        unsafe extern "C" {
            static exception_vectors: ExceptionVectors;
        }
        unsafe { &exception_vectors }
    }

    /// Записывает адрес таблицы в `VBAR_EL1`.
    pub fn install(&self) {
        // SAFETY: Прерывания замаскированы (DAIF). Адрес таблицы векторов
        // выровнен на 2KB (требование ARMv8) — обеспечивается repr(align(2048)).
        unsafe {
            write_sysreg!(vbar_el1, self as *const Self as usize);
            asm!("isb", options(nomem, nostack, preserves_flags));
        }
    }
}

/// Общий диспетчер исключений
extern "C" fn exception_handler(frame: &mut ExceptionFrame, kind: ExceptionKind) {
    match kind {
        ExceptionKind::Sync => sync_handler(frame),
        ExceptionKind::Irq => irq_handler(frame),
        ExceptionKind::Fiq => fiq_handler(frame),
        ExceptionKind::SError => serror_handler(frame),
    }
}

/// Обработчик синхронных исключений
fn sync_handler(frame: &ExceptionFrame) {
    let ec = frame.esr.exception_class();
    let iss = frame.esr.iss();

    let mut buf = StaticString::<EXCEPTION_BUF_SIZE>::new();
    let _ = writeln!(buf, "Synchronous exception");
    let _ = writeln!(
        buf,
        "  EC:   {:#04x} ({})",
        frame.esr.raw() >> 26,
        ec.description()
    );
    let _ = writeln!(buf, "  ISS:  {:#010x}", iss);
    write_frame(&mut buf, frame);

    panic!("{}", buf);
}

/// Обработчик IRQ делегирует диспетчеризацию bridge-слою ядра.
fn irq_handler(_frame: &ExceptionFrame) {
    kernel::irq_bridge::dispatch_interrupt();
}

/// Обработчик FIQ — паника с диагностикой.
fn fiq_handler(frame: &ExceptionFrame) {
    let mut buf = StaticString::<EXCEPTION_BUF_SIZE>::new();
    let _ = writeln!(buf, "FIQ");
    write_frame(&mut buf, frame);

    panic!("{}", buf);
}

/// Обработчик SError — паника с диагностикой.
fn serror_handler(frame: &ExceptionFrame) {
    let iss = frame.esr.iss();

    let mut buf = StaticString::<EXCEPTION_BUF_SIZE>::new();
    let _ = writeln!(buf, "SError (asynchronous hardware error)");
    let _ = writeln!(buf, "  ISS:  {:#010x}", iss);
    write_frame(&mut buf, frame);

    panic!("{}", buf);
}

fn write_frame(buf: &mut StaticString<EXCEPTION_BUF_SIZE>, frame: &ExceptionFrame) {
    let _ = writeln!(buf, "  ELR:  {:#018x}", frame.elr);
    let _ = writeln!(buf, "  FAR:  {:#018x}", frame.far);
    let _ = writeln!(buf, "  ESR:  {:#018x}", frame.esr.raw());
    let _ = writeln!(buf, "  SPSR: {:#018x}", frame.spsr);
    let _ = writeln!(buf, "  SP:   {:#018x}", frame.sp);
    let _ = writeln!(buf, "Registers:");

    let r = &frame.regs;
    (0..7).for_each(|group| {
        let i = group * 4;
        let _ = writeln!(
            buf,
            "  x{:02}={:#018x} x{:02}={:#018x} x{:02}={:#018x} x{:02}={:#018x}",
            i,
            r[i],
            i + 1,
            r[i + 1],
            i + 2,
            r[i + 2],
            i + 3,
            r[i + 3]
        );
    });
    let _ = writeln!(
        buf,
        "  x28={:#018x} x29={:#018x} x30={:#018x}",
        r[28], r[29], r[30]
    );
}

/// Генерирует `#[naked]` функцию-заглушку для обработки исключения.
macro_rules! exception_entry {
    ($name:ident, $kind:expr) => {
        #[unsafe(naked)]
        extern "C" fn $name() {
            naked_asm!(
                // Сохранение контекста
                "sub sp, sp, #{frame_size}",

                // Регистры общего назначения x0–x29 (15 пар)
                "stp x0,  x1,  [sp, #0]",
                "stp x2,  x3,  [sp, #16]",
                "stp x4,  x5,  [sp, #32]",
                "stp x6,  x7,  [sp, #48]",
                "stp x8,  x9,  [sp, #64]",
                "stp x10, x11, [sp, #80]",
                "stp x12, x13, [sp, #96]",
                "stp x14, x15, [sp, #112]",
                "stp x16, x17, [sp, #128]",
                "stp x18, x19, [sp, #144]",
                "stp x20, x21, [sp, #160]",
                "stp x22, x23, [sp, #176]",
                "stp x24, x25, [sp, #192]",
                "stp x26, x27, [sp, #208]",
                "stp x28, x29, [sp, #224]",

                // x30 (LR) и оригинальный SP (до sub)
                "add x9, sp, #{frame_size}",
                "stp x30, x9, [sp, #240]",

                // Системные регистры: ELR, SPSR, ESR, FAR
                "mrs x9,  elr_el1",
                "mrs x10, spsr_el1",
                "stp x9,  x10, [sp, #256]",
                "mrs x9,  esr_el1",
                "mrs x10, far_el1",
                "stp x9,  x10, [sp, #272]",

                // Вызов Rust-диспетчера
                // x0 = &mut ExceptionFrame, x1 = ExceptionKind (u64)
                "mov x0, sp",
                "mov x1, #{kind}",
                "bl  {handler}",

                // Системные регистры (до восстановления GPR, т.к. x9/x10 — временные)
                "ldp x9,  x10, [sp, #256]",
                "msr elr_el1,  x9",
                "msr spsr_el1, x10",

                // Регистры общего назначения
                "ldp x0,  x1,  [sp, #0]",
                "ldp x2,  x3,  [sp, #16]",
                "ldp x4,  x5,  [sp, #32]",
                "ldp x6,  x7,  [sp, #48]",
                "ldp x8,  x9,  [sp, #64]",
                "ldp x10, x11, [sp, #80]",
                "ldp x12, x13, [sp, #96]",
                "ldp x14, x15, [sp, #112]",
                "ldp x16, x17, [sp, #128]",
                "ldp x18, x19, [sp, #144]",
                "ldp x20, x21, [sp, #160]",
                "ldp x22, x23, [sp, #176]",
                "ldp x24, x25, [sp, #192]",
                "ldp x26, x27, [sp, #208]",
                "ldp x28, x29, [sp, #224]",
                "ldr x30, [sp, #240]",

                "add sp, sp, #{frame_size}",
                "eret",

                frame_size = const core::mem::size_of::<ExceptionFrame>(),
                kind       = const ($kind),
                handler    = sym exception_handler,
            );
        }
    };
}

// Current EL with SP_EL0
exception_entry!(sync_current_el_sp0, ExceptionKind::Sync as u64);
exception_entry!(irq_current_el_sp0, ExceptionKind::Irq as u64);
exception_entry!(fiq_current_el_sp0, ExceptionKind::Fiq as u64);
exception_entry!(serror_current_el_sp0, ExceptionKind::SError as u64);

// Current EL with SP_ELx
exception_entry!(sync_current_el_spx, ExceptionKind::Sync as u64);
exception_entry!(irq_current_el_spx, ExceptionKind::Irq as u64);
exception_entry!(fiq_current_el_spx, ExceptionKind::Fiq as u64);
exception_entry!(serror_current_el_spx, ExceptionKind::SError as u64);

// Lower EL using AArch64
exception_entry!(sync_lower_el_a64, ExceptionKind::Sync as u64);
exception_entry!(irq_lower_el_a64, ExceptionKind::Irq as u64);
exception_entry!(fiq_lower_el_a64, ExceptionKind::Fiq as u64);
exception_entry!(serror_lower_el_a64, ExceptionKind::SError as u64);

// Lower EL using AArch32
exception_entry!(sync_lower_el_a32, ExceptionKind::Sync as u64);
exception_entry!(irq_lower_el_a32, ExceptionKind::Irq as u64);
exception_entry!(fiq_lower_el_a32, ExceptionKind::Fiq as u64);
exception_entry!(serror_lower_el_a32, ExceptionKind::SError as u64);
