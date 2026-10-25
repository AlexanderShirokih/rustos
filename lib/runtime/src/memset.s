// Copyright (c) 2012-2022, Arm Limited.
// SPDX-License-Identifier: MIT
//
// memset: заполнение памяти байтом, Arm Optimized Routines в редакции picolibc 1.8.12.

    dup v0.16b, w1
    add x4, x0, x2
    cmp x2, 96
    b.hi 17f
    cmp x2, 16
    b.hs 14f
    mov x1, v0.d[0]
    // 8..15 байт.
    tbz x2, 3, 11f
    str x1, [x0]
    str x1, [x4, -8]
    ret
    .p2align 4
    // 4..7 байт.
11:
    tbz x2, 2, 12f
    str w1, [x0]
    str w1, [x4, -4]
    ret
    // 0..3 байта.
12:
    cbz x2, 13f
    strb w1, [x0]
    tbz x2, 1, 13f
    strh w1, [x4, -2]
13:
    ret
    // 16..96 байт.
14:
    str q0, [x0]
    tbnz x2, 6, 16f
    str q0, [x4, -16]
    tbz x2, 5, 15f
    str q0, [x0, 16]
    str q0, [x4, -32]
15:
    ret
    .p2align 4
    // 64..96 байт: 64 от начала и 32 от конца.
16:
    str q0, [x0, 16]
    stp q0, q0, [x0, 32]
    stp q0, q0, [x4, -32]
    ret
    .p2align 4
    // Больше 96 байт.
17:
    and w1, w1, 255
    bic x3, x0, 15
    str q0, [x0]
    cmp x2, 160
    ccmp w1, 0, 0, hs
    b.ne 19f
    // Нулевое заполнение от 160 байт идёт `dc zva` блоками по 64 байта, если DCZID_EL0
    // разрешает инструкцию на EL0 и сообщает такой размер блока.
    mrs x5, dczid_el0
    and x5, x5, 31
    cmp x5, 4
    b.ne 19f
    str q0, [x3, 16]
    stp q0, q0, [x3, 32]
    bic x3, x3, 63
    sub x2, x4, x3
    sub x2, x2, 128
    .p2align 4
18:
    add x3, x3, 64
    dc zva, x3
    subs x2, x2, 64
    b.hi 18b
    stp q0, q0, [x4, -64]
    stp q0, q0, [x4, -32]
    ret
19:
    sub x2, x4, x3
    sub x3, x3, 16
    sub x2, x2, 64 + 16
20:
    stp q0, q0, [x3, 32]
    stp q0, q0, [x3, 64]!
    subs x2, x2, 64
    b.hi 20b
    stp q0, q0, [x4, -64]
    stp q0, q0, [x4, -32]
    ret
