//! Раскладка программы userspace: загрузчик образа читает её сегменты и запись манифеста.
//!
//! Секция `.component.manifest` стоит вне загружаемых. Её читает сборщик образа из ELF, а в
//! адресное пространство процесса она не попадает. Таблица тестов, наоборот, лежит в `.rodata`
//! между `__tests_start` и `__tests_end`: её читает сам процесс.
//!
//! Массивы конструкторов и деструкторов C лежат в начале `.data` в раскладке picolibc: в
//! `.fini_array` сначала идут деструкторы по приоритету, последней - `.fini_array_onexit` с
//! обработчиками `atexit`. Обход массива с конца вызывает обработчики `atexit` раньше
//! деструкторов библиотеки. Таблица адресов `.got` C-кода лежит там же, в `.data`.

use std::{fs, path::PathBuf};

use crate::{Error, error::cargo_variable};

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
    __tests_start = .;
    KEEP(*(.tests))
    __tests_end = .;

    *(.rodata .rodata.*)
  }

  .data ALIGN(0x1000) : {
    . = ALIGN(8);
    PROVIDE_HIDDEN(__preinit_array_start = .);
    PROVIDE_HIDDEN(__bothinit_array_start = .);
    KEEP(*(.preinit_array))
    PROVIDE_HIDDEN(__preinit_array_end = .);
    PROVIDE_HIDDEN(__init_array_start = .);
    KEEP(*(SORT_BY_INIT_PRIORITY(.init_array.*)))
    KEEP(*(.init_array))
    PROVIDE_HIDDEN(__init_array_end = .);
    PROVIDE_HIDDEN(__bothinit_array_end = .);
    PROVIDE_HIDDEN(__fini_array_start = .);
    KEEP(*(SORT_BY_INIT_PRIORITY(.fini_array.*)))
    KEEP(*(.fini_array))
    KEEP(*(.fini_array*))
    PROVIDE_HIDDEN(__fini_array_end = .);

    *(.got .got.*)
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
pub fn emit_linker_script() -> Result<(), Error> {
    let linker_script = PathBuf::from(cargo_variable("OUT_DIR")?).join(GENERATED_NAME);
    fs::write(&linker_script, LINKER_SCRIPT).map_err(|source| Error::Write {
        path: linker_script.clone(),
        source,
    })?;
    println!("cargo:rustc-link-arg=-T{}", linker_script.display());
    Ok(())
}
