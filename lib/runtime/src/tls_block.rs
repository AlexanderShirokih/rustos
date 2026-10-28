//! Блок TLS потока в раскладке ELF TLS для AArch64. `TPIDR_EL0` указывает на заголовок из двух
//! слов, за ним с выравниванием шаблона лежат переменные `#[thread_local]`. Смещения переменных
//! от `TPIDR_EL0` компоновщик вычисляет при сборке. Первое слово заголовка хранит указатель на
//! блок ключей [`crate::tls`], второе - адрес точки входа порождённого потока.
//!
//! Шаблон переменных задают символы linker script программы: начальные значения `.tdata`,
//! обнуляемый `.tbss` и их общее выравнивание.
//!
//! Модуль устанавливает указатель потока под RustOS на aarch64. Для хостовых тестов зависимых
//! крейтов на других таргетах используется пустой шаблон без установки указателя потока.

#[cfg(all(target_os = "molecular", target_arch = "aarch64"))]
use core::arch::{asm, naked_asm};
use core::ptr;

const HEADER_BYTES: usize = 2 * size_of::<usize>();
const ENTRY_OFFSET: usize = size_of::<usize>();

#[derive(Debug, Clone, Copy)]
pub(crate) struct BlockLayout {
    init: &'static [u8],
    image_offset: usize,
    image_bytes: usize,
    align: usize,
}

struct Template {
    start: usize,
    init: &'static [u8],
    bytes: usize,
    align: usize,
}

impl BlockLayout {
    pub(crate) fn of_program() -> Self {
        let template = template();
        let align = template.align.max(1);
        // Компоновщик отсчитывает переменные от указателя потока той же формулой, и адрес
        // образа в блоке сравним с адресом шаблона по модулю выравнивания.
        let image_offset = HEADER_BYTES + (template.start.wrapping_sub(HEADER_BYTES) & (align - 1));

        Self {
            init: template.init,
            image_offset,
            image_bytes: template.bytes,
            align: align.max(HEADER_BYTES),
        }
    }

    pub(crate) const fn bytes(&self) -> usize {
        self.image_offset + self.image_bytes
    }

    pub(crate) const fn align(&self) -> usize {
        self.align
    }

    pub(crate) fn write(&self, block: &mut [u8]) {
        block.fill(0);
        if let Some(image) = block
            .get_mut(self.image_offset..)
            .and_then(|image| image.get_mut(..self.init.len()))
        {
            image.copy_from_slice(self.init);
        }
    }
}

pub(crate) fn install(block: &mut [u8]) {
    set_thread_pointer(block.as_mut_ptr().expose_provenance());
}

pub(crate) fn keys_word() -> Option<*mut *mut u8> {
    match thread_pointer() {
        0 => None,
        raw => Some(ptr::with_exposed_provenance_mut(raw)),
    }
}

pub(crate) fn entry_through(
    block: &mut [u8],
    entry: extern "C" fn(usize) -> !,
) -> extern "C" fn(usize) -> ! {
    if let Some(word) = block.get_mut(ENTRY_OFFSET..HEADER_BYTES) {
        word.copy_from_slice(&(entry as usize).to_ne_bytes());
    }

    start_through_header(entry)
}

#[cfg(all(target_os = "molecular", target_arch = "aarch64"))]
const fn start_through_header(_entry: extern "C" fn(usize) -> !) -> extern "C" fn(usize) -> ! {
    thread_entry
}

#[cfg(not(all(target_os = "molecular", target_arch = "aarch64")))]
const fn start_through_header(entry: extern "C" fn(usize) -> !) -> extern "C" fn(usize) -> ! {
    entry
}

#[cfg(all(target_os = "molecular", target_arch = "aarch64"))]
#[unsafe(naked)]
extern "C" fn thread_entry(_arg: usize) -> ! {
    // `thread::create` задаёт `sp` на начало блока TLS и записывает адрес `entry` во второе
    // слово заголовка. `x0` содержит аргумент `entry` и сохраняется при переходе.
    naked_asm!(
        "mov x9, sp",
        "msr tpidr_el0, x9",
        "ldr x9, [x9, #{entry}]",
        "br x9",
        entry = const ENTRY_OFFSET,
    )
}

#[cfg(all(target_os = "molecular", target_arch = "aarch64"))]
fn template() -> Template {
    unsafe extern "C" {
        static __tdata_start: u8;
        static __tdata_end: u8;
        static __tbss_end: u8;
        static __tls_align: u8;
    }

    let start = &raw const __tdata_start;
    let init_bytes = (&raw const __tdata_end).addr() - start.addr();

    // SAFETY: linker script кладёт `.tdata` между `__tdata_start` и `__tdata_end` в загруженный
    // сегмент программы. Переменные потоков лежат в их блоках, и байты шаблона не меняются.
    let init = unsafe { core::slice::from_raw_parts(start, init_bytes) };

    Template {
        start: start.addr(),
        init,
        bytes: (&raw const __tbss_end).addr() - start.addr(),
        align: (&raw const __tls_align).addr(),
    }
}

#[cfg(not(all(target_os = "molecular", target_arch = "aarch64")))]
fn template() -> Template {
    Template {
        start: 0,
        init: &[],
        bytes: 0,
        align: 1,
    }
}

#[cfg(all(target_os = "molecular", target_arch = "aarch64"))]
fn thread_pointer() -> usize {
    let value: usize;
    // SAFETY: указатель потока читается на EL0 и принадлежит текущему потоку.
    unsafe {
        asm!(
            "mrs {value}, tpidr_el0",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags),
        );
    }

    value
}

#[cfg(all(target_os = "molecular", target_arch = "aarch64"))]
fn set_thread_pointer(value: usize) {
    // SAFETY: указатель потока пишется на EL0 и принадлежит текущему потоку; его владелец -
    // этот модуль.
    unsafe {
        asm!(
            "msr tpidr_el0, {value}",
            value = in(reg) value,
            options(nomem, nostack, preserves_flags),
        );
    }
}

#[cfg(not(all(target_os = "molecular", target_arch = "aarch64")))]
fn thread_pointer() -> usize {
    0
}

#[cfg(not(all(target_os = "molecular", target_arch = "aarch64")))]
fn set_thread_pointer(_value: usize) {}
