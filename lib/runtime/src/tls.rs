//! Локальные данные потока по ключам.
//!
//! Ключ — индекс слота в блоке потока, ноль зарезервирован под «ключа нет». Блок заводится на
//! первой записи и живёт по указателю локальных данных потока.
//!
//! [`run_destructors`] обязан позвать тот, кто закрывает поток: для потоков `lib/runtime` это
//! трамплин запуска, для главного потока программы на `std` — платформенный `cleanup`.
//! Ключевой TLS стандартной библиотеки рассчитывает на этот прогон и без него не отработают
//! ни `Drop` у `thread_local!`, ни её собственная уборка потока.

use core::{
    arch::asm,
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

/// Ключ локальных данных потока. Ноль означает отсутствие ключа.
pub type Key = usize;

/// Деструктор значения, зарегистрированного под ключом.
pub type Dtor = unsafe extern "C" fn(*mut u8);

const MAX_KEYS: usize = 64;

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
    if let Some(dtor) = dtor {
        DESTRUCTORS[key].store(dtor as usize, Ordering::Release);
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
        Some(block) => unsafe { (*block).slots[key] },
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
    // SAFETY: ключ меньше MAX_KEYS по контракту create, блок жив до конца потока.
    unsafe { (*block).slots[key] = value };
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
    DESTRUCTORS[key].store(0, Ordering::Release);
}

/// Гоняет деструкторы локальных данных уходящего потока и освобождает его блок.
pub fn run_destructors() {
    let Some(block) = block() else {
        return;
    };

    for _ in 0..DESTRUCTOR_ROUNDS {
        let mut pending = false;
        for (key, destructor) in DESTRUCTORS.iter().enumerate().skip(1) {
            // SAFETY: блок жив, индекс в границах массива слотов.
            let value = unsafe { (*block).slots[key] };
            if value.is_null() {
                continue;
            }
            // SAFETY: см. выше; слот освобождается до вызова деструктора, чтобы повторный
            // заход не увидел то же значение.
            unsafe { (*block).slots[key] = ptr::null_mut() };

            let raw = destructor.load(Ordering::Acquire);
            if raw != 0 {
                // SAFETY: в слоте лежит указатель на `Dtor`, положенный `create`.
                let dtor: Dtor = unsafe { core::mem::transmute::<usize, Dtor>(raw) };
                // SAFETY: значение выдано владельцем ключа под его же деструктор.
                unsafe { dtor(value) };
                pending = true;
            }
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

    // SAFETY: layout `Block` ненулевого размера; выравнивание — степень двойки.
    let raw = unsafe { crate::heap::alloc(size_of::<Block>(), align_of::<Block>()) };
    if raw.is_null() {
        return None;
    }
    // Память запрошена под `align_of::<Block>()`, поэтому каст выравнивание не нарушает.
    #[allow(clippy::cast_ptr_alignment)]
    let block = raw.cast::<Block>();
    // SAFETY: свежая память под `Block`, других владельцев нет.
    unsafe {
        block.write(Block {
            slots: [ptr::null_mut(); MAX_KEYS],
        });
    }
    set_thread_pointer(block as usize as u64);
    Some(block)
}

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

fn set_thread_pointer(value: u64) {
    // SAFETY: указатель локальных данных пишется на EL0 и принадлежит текущему потоку;
    // его владелец — этот модуль.
    unsafe {
        asm!(
            "msr tpidr_el0, {value}",
            value = in(reg) value,
            options(nomem, nostack, preserves_flags),
        );
    }
}
