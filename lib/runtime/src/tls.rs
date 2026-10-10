//! Локальные данные потока по ключам.
//!
//! Ключ - индекс слота в блоке потока, ноль зарезервирован под "ключа нет". Блок создаётся при
//! первой записи, и его адрес хранится в указателе локальных данных потока.
//!
//! [`run_destructors`] обязан позвать тот, кто закрывает поток, в том числе рантайм программы
//! при завершении её главного потока. Прогон обнуляет только слоты ключей с деструктором, и
//! значение ключа без деструктора доступно деструкторам других ключей до освобождения блока.
//! Ключевой TLS стандартной библиотеки рассчитывает на этот прогон и без него не отработают
//! ни `Drop` у `thread_local!`, ни её собственная уборка потока.
//!
//! Указатель локальных данных есть только под RustOS на aarch64. На другом таргете блок потока
//! не запоминается, и крейт собирается для хостовых тестов зависимых крейтов.

#[cfg(all(target_os = "molecular", target_arch = "aarch64"))]
use core::arch::asm;
use core::{
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

/// Ключ локальных данных потока. Ноль означает отсутствие ключа.
pub type Key = usize;

/// Деструктор значения, зарегистрированного под ключом.
pub type Dtor = unsafe extern "C" fn(*mut u8);

const MAX_KEYS: usize = 130;

const DESTRUCTOR_ROUNDS: usize = 4;

static NEXT_KEY: AtomicUsize = AtomicUsize::new(1);

static DESTRUCTORS: [AtomicUsize; MAX_KEYS] = [const { AtomicUsize::new(0) }; MAX_KEYS];

struct Block {
    slots: [*mut u8; MAX_KEYS],
}

/// Заводит ключ с деструктором `dtor`. Ноль означает, что ключи исчерпаны.
pub fn create(dtor: Option<Dtor>) -> Key {
    let key = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
    if key >= MAX_KEYS {
        // Счётчик упирается в потолок: дальше расти ему незачем, а переполнение вернуло бы
        // индексы, которые уже раздали.
        NEXT_KEY.store(MAX_KEYS, Ordering::Relaxed);
        return 0;
    }
    if let Some(dtor) = dtor
        && let Some(cell) = DESTRUCTORS.get(key)
    {
        cell.store(dtor as usize, Ordering::Release);
    }
    key
}

/// Значение, записанное под ключом в текущем потоке.
///
/// # Safety
///
/// `key` выдан [`create`].
pub unsafe fn get(key: Key) -> *mut u8 {
    match block() {
        // SAFETY: ключ меньше MAX_KEYS по контракту create, блок жив до конца потока.
        Some(block) => unsafe { (*block).slots.get(key).copied().unwrap_or(ptr::null_mut()) },
        None => ptr::null_mut(),
    }
}

/// Записывает значение под ключом в текущем потоке.
///
/// # Safety
///
/// `key` выдан [`create`].
pub unsafe fn set(key: Key, value: *mut u8) {
    let Some(block) = installed_block() else {
        return;
    };
    // SAFETY: блок жив до конца потока, а слот берётся проверенным доступом.
    unsafe {
        if let Some(slot) = (*block).slots.get_mut(key) {
            *slot = value;
        }
    }
}

/// Снимает ключ с учёта. Слоты живых потоков остаются на месте до их ухода.
///
/// # Safety
///
/// `key` выдан [`create`] и больше не используется.
pub unsafe fn destroy(key: Key) {
    if key == 0 || key >= MAX_KEYS {
        return;
    }
    if let Some(cell) = DESTRUCTORS.get(key) {
        cell.store(0, Ordering::Release);
    }
}

/// Гоняет деструкторы локальных данных уходящего потока и освобождает его блок.
pub fn run_destructors() {
    let Some(block) = block() else {
        return;
    };

    for _ in 0..DESTRUCTOR_ROUNDS {
        let mut pending = false;
        for (key, destructor) in DESTRUCTORS.iter().enumerate().skip(1) {
            let raw = destructor.load(Ordering::Acquire);
            if raw == 0 {
                continue;
            }

            // SAFETY: блок жив, а слот берётся проверенным доступом.
            let Some(value) = (unsafe { (*block).slots.get(key).copied() }) else {
                continue;
            };
            if value.is_null() {
                continue;
            }
            // SAFETY: см. выше. Слот освобождается до вызова деструктора, и повторный заход
            // не увидит то же значение.
            unsafe {
                if let Some(slot) = (*block).slots.get_mut(key) {
                    *slot = ptr::null_mut();
                }
            }

            // SAFETY: в слоте лежит указатель на `Dtor`, положенный `create`.
            let dtor: Dtor = unsafe { core::mem::transmute::<usize, Dtor>(raw) };
            // SAFETY: значение выдано владельцем ключа под его же деструктор.
            unsafe { dtor(value) };
            pending = true;
        }
        if !pending {
            break;
        }
    }

    set_thread_pointer(0);
    // SAFETY: блок выдан `alloc` с этим layout и больше никому не виден.
    unsafe { crate::heap::dealloc(block.cast::<u8>(), size_of::<Block>(), align_of::<Block>()) };
}

fn block() -> Option<*mut Block> {
    match thread_pointer() {
        0 => None,
        raw => Some(raw as *mut Block),
    }
}

fn installed_block() -> Option<*mut Block> {
    if let Some(block) = block() {
        return Some(block);
    }

    // SAFETY: layout `Block` ненулевого размера; выравнивание - степень двойки.
    let raw = unsafe { crate::heap::alloc(size_of::<Block>(), align_of::<Block>()) };
    if raw.is_null() {
        return None;
    }
    let block: *mut Block = raw.cast();
    // SAFETY: память выдана `alloc` под размер и выравнивание `Block` и принадлежит только
    // этому блоку.
    unsafe {
        block.write(Block {
            slots: [ptr::null_mut(); MAX_KEYS],
        });
    }
    set_thread_pointer(block as usize as u64);
    Some(block)
}

#[cfg(all(target_os = "molecular", target_arch = "aarch64"))]
fn thread_pointer() -> u64 {
    let value: u64;
    // SAFETY: указатель локальных данных читается на EL0 и принадлежит текущему потоку.
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
fn set_thread_pointer(value: u64) {
    // SAFETY: указатель локальных данных пишется на EL0 и принадлежит текущему потоку;
    // его владелец - этот модуль.
    unsafe {
        asm!(
            "msr tpidr_el0, {value}",
            value = in(reg) value,
            options(nomem, nostack, preserves_flags),
        );
    }
}

#[cfg(not(all(target_os = "molecular", target_arch = "aarch64")))]
fn thread_pointer() -> u64 {
    0
}

#[cfg(not(all(target_os = "molecular", target_arch = "aarch64")))]
fn set_thread_pointer(_value: u64) {}
