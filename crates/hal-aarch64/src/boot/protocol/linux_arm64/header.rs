//! Заголовок формата Linux ARM64 для совместимости с загрузчиками, работающими с binary форматом.

use core::arch::global_asm;

// .pushsection/.popsection: секция .head не утекает в последующие global_asm-блоки.
global_asm!(
    r#"
        .pushsection .head, "ax"
        .balign 8
        .global __start
    __start:
        b _start                            // code0: branch to _start
        .word 0                             // code1
        .quad 0                             // text_offset
        .quad _kernel_size                  // image_size
        .quad 0xa                           // flags: LE, 4K pages, anywhere
        .quad 0                             // res2
        .quad 0                             // res3
        .quad 0                             // res4
        .word 0x644D5241                    // magic "ARM\x64"
        .word 0                             // res5
        .popsection
    "#
);
