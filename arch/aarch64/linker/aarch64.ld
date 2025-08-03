OUTPUT_ARCH(aarch64);
ENTRY(_boot);

STACK_SIZE = 2M;

SECTIONS {
    . = 0x00008000;

    .head ALIGN(8) : {
        KEEP(*(.head))
    }

    _kernel_start = .;

    .text : ALIGN(4K) {
        *(.text*)
    }

    .rodata : ALIGN(4K) {
        *(.rodata*)
    }

    .data : ALIGN(4K) {
        *(.data*)
    }

    .bss : ALIGN(4K) {
        *(.bss*)
        *(COMMON)
    }

    _stack_bottom = .;
    . += STACK_SIZE;
    _stack_top = .;

    _kernel_end = .;

    PROVIDE(_kernel_size = _kernel_end - _kernel_start);
}
