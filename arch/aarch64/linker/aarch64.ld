OUTPUT_ARCH(aarch64);
ENTRY(_start);

STACK_SIZE = 2M;

_device_memory_start = 0x08000000; /* GIC, UART, etc. */
_device_memory_end   = 0x40000000;

_ram_start = 0x40000000;
_ram_size =  0x08000000;
_ram_end = _ram_start + _ram_size; /* 128MB */

_stack_bottom = _ram_start;
_stack_top    = _stack_bottom + STACK_SIZE;

_kernel_start = ALIGN(_stack_top, 0x200000);

SECTIONS {
    . = _kernel_start;

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

    _kernel_end = .;

    ASSERT(_stack_bottom < _stack_top, "Invalid stack boundaries")

    /* Ensure we don't overflow into stack */
    ASSERT(_kernel_end <= _stack_bottom || _kernel_start >= _stack_top, "Kernel overlaps with stack!")
}
