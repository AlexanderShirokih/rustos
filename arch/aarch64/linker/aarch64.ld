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

        *(.rodata*)
    }

    _rodata_end = ALIGN(4K);
    _rw_start = ALIGN(4K);

    . = ALIGN(4K);
    .data : AT(. - KERNEL_VIRT_BASE) {
        *(.data*)
    }

    /* NOLOAD-секции: LMA не важна, заполнение нулями обеспечивает загрузчик/BSS-очистка */
    .bss (NOLOAD) : {
        _bss_start = .;
        *(.bss*)
        *(COMMON)
        _bss_end = .;
    }

    .stack (NOLOAD) : {
        _stack_bottom = .;
        . += 64K;
        _stack_top = .;
    }

    _rw_end = .;
    _kernel_end = .;

    PROVIDE(_kernel_size = _kernel_end - _kernel_start);
    PROVIDE(_kernel_virt_base = KERNEL_VIRT_BASE);
}
