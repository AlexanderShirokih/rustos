OUTPUT_ARCH(aarch64);
ENTRY(_header_start);

KERNEL_BASE = 0xe5800000;

SECTIONS {
    . = KERNEL_BASE;

    .head ALIGN(8) : {
        KEEP(*(.head))
    }

    . = KERNEL_BASE + 0x80000;

    _kernel_start = .;

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

    .bss : ALIGN(4K) {
        *(.bss*)
        *(COMMON)
        . = ALIGN(16);
        __stack_start = .;
        *(.bss.stack)
    } =0

    _kernel_end = .;

    PROVIDE(_kernel_size = _kernel_end - _kernel_start);
}
