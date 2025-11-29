//! Заголовок формата Linux ARM64 для совместимости с загрузчиками работающих c binary форматом

use core::arch::global_asm;

global_asm!(
    r#"
        .section .head, "ax"
        .balign 8
        .global __start
    __start:
        b _start                            // code0: branch to _start
        .word 0                             // code1
        .quad 0                             // text_offset
        .quad _kernel_size                  // image_size
        .quad 0                             // flags
        .quad 0                             // res2
        .quad 0                             // res3
        .quad 0                             // res4
        .word 0x644D5241                    // magic "ARM\x64"
        .word 0                             // res5
    "#
);
