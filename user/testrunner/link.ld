ENTRY(_start)

SECTIONS
{
  . = 0x00400000;

  .text ALIGN(0x1000) : {
    *(.text .text.*)
  }

  .rodata ALIGN(0x1000) : {
    . = ALIGN(8);
    __tests_kernel_start = .;
    KEEP(*(.tests.kernel))
    __tests_kernel_end = .;

    *(.rodata .rodata.*)
  }

  .data ALIGN(0x1000) : {
    *(.data .data.*)
  }

  .bss ALIGN(0x1000) (NOLOAD) : {
    *(.bss .bss.*)
    *(COMMON)
  }

  /DISCARD/ : {
    *(.eh_frame*)
    *(.comment*)
    *(.note*)
  }
}
