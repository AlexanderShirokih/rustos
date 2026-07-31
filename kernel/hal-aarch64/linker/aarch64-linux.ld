OUTPUT_ARCH(aarch64);
ENTRY(__start);

PROVIDE(KERNEL_OFFSET = 0);

KERNEL_VIRT_BASE = 0xFFFFFF8000000000;

SECTIONS {
    . = KERNEL_VIRT_BASE + KERNEL_OFFSET;

    /* Загрузочный заголовок: LMA = KERNEL_OFFSET, VMA = KERNEL_VIRT_BASE + KERNEL_OFFSET */
    .head : AT(. - KERNEL_VIRT_BASE) {
        KEEP(*(.head))
    }

    _kernel_start = .;
    _text_start = .;

    . = ALIGN(4K);
    .text : AT(. - KERNEL_VIRT_BASE) {
        *(.text*)
    }

    _text_end = ALIGN(4K);
    _rodata_start = ALIGN(4K);

    . = ALIGN(4K);
    .rodata : AT(. - KERNEL_VIRT_BASE) {
        __drivers_kernel_start = .;
        KEEP(*(.drivers.kernel))
        __drivers_kernel_end = .;

        . = ALIGN(8);
        __tests_kernel_start = .;
        KEEP(*(.tests.kernel))
        __tests_kernel_end = .;

        *(.rodata*)
    }

    _rodata_end = ALIGN(4K);
    _rw_start = ALIGN(4K);

    . = ALIGN(4K);
    .data : AT(. - KERNEL_VIRT_BASE) {
        *(.data*)
    }

    /* NOLOAD-секции: LMA не важна, заполнение нулями обеспечивает загрузчик/BSS-очистка */
    /* Границы кратны 8: очистка BSS в `_start` идёт словами по 8 байт. */
    .bss (NOLOAD) : ALIGN(8) {
        _bss_start = .;
        *(.bss*)
        *(COMMON)
        . = ALIGN(8);
        _bss_end = .;
    }

    /* Стек pre-MMU фазы. Через него по значению проходит `MemorySetup`, чей
       размер растёт вместе с числом регионов, поэтому `_start` заливает
       область маркером, а post-MMU фаза измеряет по нему высшую отметку.
       SP обязан быть выровнен на 16 (AAPCS64); без ALIGN вершина стека
       наследует произвольную границу конца .bss. */
    .stack (NOLOAD) : ALIGN(16) {
        _stack_bottom = .;
        . += 128K;
        _stack_top = .;
    }

    _rw_end = .;
    _kernel_end = .;

    PROVIDE(_kernel_size = _kernel_end - _kernel_start);
    PROVIDE(_kernel_virt_base = KERNEL_VIRT_BASE);
}
