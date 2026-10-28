//! Раскладка программы userspace: загрузчик образа читает её сегменты и запись манифеста.
//!
//! Секция `.component.manifest` стоит вне загружаемых. Её читает сборщик образа из ELF, а в
//! адресное пространство процесса она не попадает. Таблица тестов, наоборот, лежит в `.rodata`
//! между `__tests_start` и `__tests_end`: её читает сам процесс.
//!
//! Массивы конструкторов и деструкторов C лежат в начале `.data` в раскладке picolibc: в
//! `.fini_array` сначала идут деструкторы по приоритету, затем `.fini_array_onexit` с
//! обработчиками `atexit`, последней - `.fini_thread_local` с деструкторами `thread_local`.
//! Массив обходится с конца. Таблица адресов `.got` C-кода лежит там же, в `.data`.
//!
//! Шаблон блока TLS потока - `.tdata` между `__tdata_start` и `__tdata_end` и обнуляемый `.tbss`
//! до `__tbss_end`. Общее выравнивание секций - адрес символа `__tls_align`.
//!
//! Таблицы раскрутки стека `.eh_frame`, их индекс `.eh_frame_hdr` и таблицы обработчиков
//! `.gcc_except_table` лежат в сегменте только для чтения. Раскрутчик C++ находит таблицы по
//! символам `__eh_frame_start`/`__eh_frame_end` и `__eh_frame_hdr_start`/`__eh_frame_hdr_end`.
//! Без индекса границы `__eh_frame_hdr_*` нулевые.

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
    *(.gcc_except_table .gcc_except_table.*)
  }

  .eh_frame_hdr : {
    KEEP(*(.eh_frame_hdr))
  }
  __eh_frame_hdr_start = SIZEOF(.eh_frame_hdr) > 0 ? ADDR(.eh_frame_hdr) : 0;
  __eh_frame_hdr_end = SIZEOF(.eh_frame_hdr) > 0 ? ADDR(.eh_frame_hdr) + SIZEOF(.eh_frame_hdr) : 0;

  .eh_frame : {
    __eh_frame_start = .;
    KEEP(*(.eh_frame))
    __eh_frame_end = .;
  }

  .tdata ALIGN(0x1000) : {
    __tdata_start = .;
    *(.tdata .tdata.*)
    __tdata_end = .;
  }

  .tbss : {
    *(.tbss .tbss.*)
    __tbss_end = .;
  }
  __tls_align = MAX(ALIGNOF(.tdata), ALIGNOF(.tbss));

  .data : {
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
    KEEP(*(.fini_thread_local))
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
    *(.comment*)
    *(.note*)
  }
}
";

const GENERATED_NAME: &str = "userland.ld";

/// Кладёт в сборку стандартный linker script программы userspace и передаёт его компоновщику
/// вместе с запросом индекса `.eh_frame_hdr`.
pub fn emit_linker_script() -> Result<(), Error> {
    let linker_script = PathBuf::from(cargo_variable("OUT_DIR")?).join(GENERATED_NAME);
    fs::write(&linker_script, LINKER_SCRIPT).map_err(|source| Error::Write {
        path: linker_script.clone(),
        source,
    })?;
    println!("cargo:rustc-link-arg=-T{}", linker_script.display());
    println!("cargo:rustc-link-arg=--eh-frame-hdr");
    // Программу не загружает динамический загрузчик, и RELRO ей не нужен.
    println!("cargo:rustc-link-arg=-znorelro");
    Ok(())
}
