OUTPUT_ARCH(aarch64);
ENTRY(_start);

STACK_SIZE = 2M;

_stack_bottom = 0x40000000;
_stack_top    = _stack_bottom + STACK_SIZE;

_kernel_start = _stack_top;

SECTIONS {
  . = _kernel_start;
  .text : { *(.text*) }
  .rodata : { *(.rodata*) }
  .data : { *(.data*) }
  .bss :   { *(.bss*) }
}