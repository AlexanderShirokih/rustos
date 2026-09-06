//! Раскладка программы userspace: загрузчик образа читает её сегменты и запись манифеста.
//!
//! Секция `.component.manifest` стоит вне загружаемых. Её читает сборщик образа из ELF, а в
//! адресное пространство процесса она не попадает. Таблица тестов, наоборот, лежит в `.rodata`
//! между `__tests_kernel_start` и `__tests_kernel_end`: её читает сам процесс.

use std::{env, fs, path::PathBuf};

const LINKER_SCRIPT: &str = "\
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

  .component.manifest 0 (INFO) : {
    KEEP(*(.component.manifest))
  }

  /DISCARD/ : {
    *(.eh_frame*)
    *(.comment*)
    *(.note*)
  }
}
";

const GENERATED_NAME: &str = "userland.ld";

/// Кладёт в сборку стандартный linker script программы userspace и передаёт его компоновщику.
#[allow(clippy::expect_used)]
pub fn emit_linker_script() {
    // Отказ окружения или файловой системы прекращает сборку, и `cargo` читает именно
    // панику скрипта.
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("out dir"));
    let linker_script = out_dir.join(GENERATED_NAME);
    fs::write(&linker_script, LINKER_SCRIPT).expect("write the linker script");
    println!("cargo:rustc-link-arg=-T{}", linker_script.display());
}
