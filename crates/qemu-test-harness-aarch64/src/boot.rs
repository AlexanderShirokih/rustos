//! Linux ARM64 boot protocol header.
//!
//! Тестовый бинарь подключает `linux_arm64_boot_header!()` один раз;
//! линкер-скрипт должен класть секцию `.head` первой и определять
//! `_kernel_size`.

#[macro_export]
macro_rules! linux_arm64_boot_header {
    () => {
        ::core::arch::global_asm!(
            r#"
                .section .head, "ax"
                .balign 8
                .global __start
            __start:
                b _start
                .word 0
                .quad 0
                .quad _kernel_size
                .quad 0xa
                .quad 0
                .quad 0
                .quad 0
                .word 0x644D5241
                .word 0
            "#
        );
    };
}
