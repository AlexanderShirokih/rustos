// Copyright (c) 2012-2022, Arm Limited.
// SPDX-License-Identifier: MIT
//
// memcpy и memmove: копия с перекрытием диапазонов, Arm Optimized Routines в редакции
// picolibc 1.8.12.

    add x4, x1, x2
    add x5, x0, x2
    cmp x2, 128
    b.hi 1f
    cmp x2, 32
    b.hi 2f
    cmp x2, 16
    b.lo 3f
    ldp x6, x7, [x1]
    ldp x12, x13, [x4, -16]
    stp x6, x7, [x0]
    stp x12, x13, [x5, -16]
    ret
    // 8..15 байт.
3:
    tbz x2, 3, 4f
    ldr x6, [x1]
    ldr x7, [x4, -8]
    str x6, [x0]
    str x7, [x5, -8]
    ret
    .p2align 3
    // 4..7 байт.
4:
    tbz x2, 2, 5f
    ldr w6, [x1]
    ldr w8, [x4, -4]
    str w6, [x0]
    str w8, [x5, -4]
    ret
    // 0..3 байта без ветвлений.
5:
    cbz x2, 6f
    lsr x14, x2, 1
    ldrb w6, [x1]
    ldrb w10, [x4, -1]
    ldrb w8, [x1, x14]
    strb w6, [x0]
    strb w8, [x0, x14]
    strb w10, [x5, -1]
6:
    ret
    .p2align 4
    // 33..128 байт.
2:
    ldp x6, x7, [x1]
    ldp x8, x9, [x1, 16]
    ldp x10, x11, [x4, -32]
    ldp x12, x13, [x4, -16]
    cmp x2, 64
    b.hi 7f
    stp x6, x7, [x0]
    stp x8, x9, [x0, 16]
    stp x10, x11, [x5, -32]
    stp x12, x13, [x5, -16]
    ret
    .p2align 4
    // 65..128 байт.
7:
    ldp x14, x15, [x1, 32]
    ldp x16, x17, [x1, 48]
    cmp x2, 96
    b.ls 8f
    ldp x2, x3, [x4, -64]
    ldp x1, x4, [x4, -48]
    stp x2, x3, [x5, -64]
    stp x1, x4, [x5, -48]
8:
    stp x6, x7, [x0]
    stp x8, x9, [x0, 16]
    stp x14, x15, [x0, 32]
    stp x16, x17, [x0, 48]
    stp x10, x11, [x5, -32]
    stp x12, x13, [x5, -16]
    ret
    .p2align 4
    // Больше 128 байт. Приёмник выше источника с перекрытием - копия с конца.
1:
    sub x14, x0, x1
    cbz x14, 6b
    cmp x14, x2
    b.lo 9f
    // Первые 16 байт, затем приёмник выравнивается на 16 байт.
    ldp x12, x13, [x1]
    and x14, x0, 15
    bic x3, x0, 15
    sub x1, x1, x14
    add x2, x2, x14
    ldp x6, x7, [x1, 16]
    stp x12, x13, [x0]
    ldp x8, x9, [x1, 32]
    ldp x10, x11, [x1, 48]
    ldp x12, x13, [x1, 64]!
    subs x2, x2, 128 + 16
    b.ls 10f
    // Программно конвейеризованный цикл по 64 байта.
11:
    stp x6, x7, [x3, 16]
    ldp x6, x7, [x1, 16]
    stp x8, x9, [x3, 32]
    ldp x8, x9, [x1, 32]
    stp x10, x11, [x3, 48]
    ldp x10, x11, [x1, 48]
    stp x12, x13, [x3, 64]!
    ldp x12, x13, [x1, 64]!
    subs x2, x2, 64
    b.hi 11b
    // Последняя итерация и 64 байта от конца.
10:
    ldp x14, x15, [x4, -64]
    stp x6, x7, [x3, 16]
    ldp x6, x7, [x4, -48]
    stp x8, x9, [x3, 32]
    ldp x8, x9, [x4, -32]
    stp x10, x11, [x3, 48]
    ldp x10, x11, [x4, -16]
    stp x12, x13, [x3, 64]
    stp x14, x15, [x5, -64]
    stp x6, x7, [x5, -48]
    stp x8, x9, [x5, -32]
    stp x10, x11, [x5, -16]
    ret
    .p2align 4
    // Копия с конца: последние 16 байт, затем конец приёмника выравнивается на 16 байт.
9:
    ldp x12, x13, [x4, -16]
    and x14, x5, 15
    sub x4, x4, x14
    sub x2, x2, x14
    ldp x6, x7, [x4, -16]
    stp x12, x13, [x5, -16]
    ldp x8, x9, [x4, -32]
    ldp x10, x11, [x4, -48]
    ldp x12, x13, [x4, -64]!
    sub x5, x5, x14
    subs x2, x2, 128
    b.ls 13f
12:
    stp x6, x7, [x5, -16]
    ldp x6, x7, [x4, -16]
    stp x8, x9, [x5, -32]
    ldp x8, x9, [x4, -32]
    stp x10, x11, [x5, -48]
    ldp x10, x11, [x4, -48]
    stp x12, x13, [x5, -64]!
    ldp x12, x13, [x4, -64]!
    subs x2, x2, 64
    b.hi 12b
    // Последняя итерация и 64 байта от начала.
13:
    ldp x2, x3, [x1, 48]
    stp x6, x7, [x5, -16]
    ldp x6, x7, [x1, 32]
    stp x8, x9, [x5, -32]
    ldp x8, x9, [x1, 16]
    stp x10, x11, [x5, -48]
    ldp x10, x11, [x1]
    stp x12, x13, [x5, -64]
    stp x2, x3, [x0, 48]
    stp x6, x7, [x0, 32]
    stp x8, x9, [x0, 16]
    stp x10, x11, [x0]
    ret
