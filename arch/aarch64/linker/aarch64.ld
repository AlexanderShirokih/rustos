OUTPUT_ARCH(aarch64);
ENTRY(__start);

PROVIDE(KERNEL_OFFSET = 0);

SECTIONS {
    . = KERNEL_OFFSET;

    .head ALIGN(8) : {
        KEEP(*(.head))
    }

    _kernel_start = .;
    _text_start = .;
    
    .text : ALIGN(4K) {
        *(.text*)
    }
    
    _text_end = ALIGN(4K);
    _rodata_start = ALIGN(4K);

    .rodata : ALIGN(4K) {
        __drivers_early_start = .;
        KEEP(*(.drivers.early))
        __drivers_early_end = .;

        __drivers_kernel_start = .;
        KEEP(*(.drivers.kernel))
        __drivers_kernel_end = .;

        *(.rodata*)
    }
    
    _rodata_end = ALIGN(4K);
    _rw_start = ALIGN(4K);

    .data : ALIGN(4K) {
        *(.data*)
    }

    .bss (NOLOAD) : ALIGN(4K) {
        _bss_start = .;
        *(.bss*)
        *(COMMON)
        _bss_end = .;
    } =0

    .stack (NOLOAD) : ALIGN(4K) {
        _stack_bottom = .;
        . += 16K;
        _stack_top = .;
    } =0

    _rw_end = .;
    _kernel_end = .;

    PROVIDE(_kernel_size = _kernel_end - _kernel_start);
}
