OUTPUT_ARCH(aarch64);
ENTRY(_header_start);

SECTIONS {
    . = 0;

    .head ALIGN(8) : {
        KEEP(*(.head))
    }

    _kernel_start = .;

    . = 0x80000;

    .text : ALIGN(4K) {
        *(.text*)
    }

    .text.vectors : ALIGN(2K) {
        KEEP(*(.text.vectors))
    }

    .rodata : ALIGN(4K) {
        *(.rodata*)
    }

    .data : ALIGN(4K) {
        *(.data*)
    }

    .bss (NOLOAD) : ALIGN(4K) {
        __bss_start = .;
        *(.bss*)
        *(COMMON)
        __bss_end = .;
    }

    .bss.stack (NOLOAD) : ALIGN(16) {
            *(.bss.stack)
    }

    _kernel_end = .;

    PROVIDE(_kernel_size = _kernel_end - _kernel_start);
}
