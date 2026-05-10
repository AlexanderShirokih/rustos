//! Платформенная проверка политики Origin: kernel-side `SVC` отвергается
//! диспатчером с `KernelOriginated`.
//!
//! Полный путь "vector → exception_entry → syscall::dispatch → handler →
//! kobject" из user-контекста покрыт тестами EL0-входа в
//! [`super::userspace_entry`] и `super::userspace_via_scheduler`;
//! тонкие случаи диспатчера (`BadSyscall`, `InvalidArgument`, маски сигналов)
//! — host-юнит-тестами в `syscall::bridge`. Здесь нужен только один
//! интеграционный тест на реальном trap-vector'е, доказывающий, что
//! Origin-фильтр действительно стоит раньше парсинга op.

use core::arch::asm;

use syscall::{SyscallError, SyscallOp};
use test_harness_qemu::register_test;

/// `svc` из EL1 возвращает `-KernelOriginated` независимо от номера
/// операции и аргументов: trap зарезервирован за user→kernel-переходом.
fn syscall_from_kernel_origin_is_rejected() {
    let result: i64;
    // SAFETY: SVC с произвольными аргументами — диспатчер отвергает trap
    // по Origin::Kernel до того, как трогает аргументы или handler.
    unsafe {
        asm!(
            "svc #{op}",
            in("x0") 0_u64,
            in("x1") 0_u64,
            in("x2") 0_u64,
            lateout("x0") result,
            op = const SyscallOp::ObjectSignal as u16,
            options(nostack, preserves_flags),
        );
    }
    test_harness_qemu::kassert_eq!(result, i64::from(SyscallError::KernelOriginated));
}

register_test!(
    SYSCALL_KERNEL_ORIGIN_REJECTED,
    "syscall_from_kernel_origin_is_rejected",
    syscall_from_kernel_origin_is_rejected
);
