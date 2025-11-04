OUTPUT_ARCH(aarch64);
ENTRY(_header_start);

# Будем передавать KERNEL_OFFSET через флаги линкера, чтобы менять его динамечески
PROVIDE(KERNEL_OFFSET = 0);

SECTIONS {
    . = KERNEL_OFFSET;

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
        . = ALIGN(16);
        *(.bss.stack)
        *(.bss.heap)
    } =0

    _kernel_end = .;

    PROVIDE(_kernel_size = _kernel_end - _kernel_start);
}
