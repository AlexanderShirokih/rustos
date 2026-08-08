//! Куча процесса поверх анонимных регионов ядра.
//!
//! Мелкие блоки обслуживают списки свободных по классам размера, крупные идут прямо к ядру.
//! Класс подбирается по большему из размера и выравнивания. Блок класса всегда выровнен по
//! своей границе.
//!
//! Чанк под класс ядру не возвращается: освобождённый блок уходит в список своего класса и
//! ждёт следующей аллокации. Ядру возвращаются только крупные блоки.

use syscall::{PAGE_SIZE, UserMemFlags};

use crate::{
    lock::Lock,
    svc::{memory_allocate, memory_free},
};

const CLASS_COUNT: usize = 8;

const MIN_CLASS: usize = 16;

const MAX_CLASS: usize = MIN_CLASS << (CLASS_COUNT - 1);

const CHUNK: usize = 64 * 1024;

static HEAP: Lock<FreeLists> = Lock::new(FreeLists::new());

struct FreeLists {
    heads: [*mut u8; CLASS_COUNT],
}

// SAFETY: доступ к спискам сериализован `Lock`, а блоки принадлежат процессу целиком.
unsafe impl Send for FreeLists {}

impl FreeLists {
    const fn new() -> Self {
        Self {
            heads: [core::ptr::null_mut(); CLASS_COUNT],
        }
    }

    fn pop(&mut self, class: usize) -> *mut u8 {
        let head = self.heads[class];
        if head.is_null() {
            return head;
        }
        // SAFETY: свободный блок хранит указатель на следующий в своих первых 8 байтах;
        // блок не меньше MIN_CLASS и выровнен по своему классу, чтение корректно.
        #[allow(clippy::cast_ptr_alignment)]
        unsafe {
            self.heads[class] = head.cast::<*mut u8>().read();
        }
        head
    }

    fn push(&mut self, class: usize, block: *mut u8) {
        // SAFETY: блок вышел из этого же класса, свободен и не меньше MIN_CLASS.
        #[allow(clippy::cast_ptr_alignment)]
        unsafe {
            block.cast::<*mut u8>().write(self.heads[class]);
        }
        self.heads[class] = block;
    }

    fn refill(&mut self, class: usize) -> bool {
        let size = class_size(class);
        let Some(base) = request_bytes(CHUNK) else {
            return false;
        };
        for offset in (0..CHUNK).step_by(size) {
            // SAFETY: offset не выходит за пределы только что выделенного чанка.
            self.push(class, unsafe { base.add(offset) });
        }
        true
    }
}

/// Выделяет блок под `size` байт с выравниванием `align`. Ноль означает отказ.
///
/// # Safety
///
/// Контракт `GlobalAlloc`: `size` ненулевой, `align` — степень двойки.
pub unsafe fn alloc(size: usize, align: usize) -> *mut u8 {
    let Some(class) = class_of(size, align) else {
        return large_alloc(size, align);
    };

    let mut heap = HEAP.lock();
    let block = heap.pop(class);
    if !block.is_null() {
        return block;
    }
    if !heap.refill(class) {
        return core::ptr::null_mut();
    }
    heap.pop(class)
}

/// Возвращает блок, выданный [`alloc`] с теми же `size` и `align`.
///
/// # Safety
///
/// `block` выдан [`alloc`] с этими `size` и `align` и ещё не освобождался.
pub unsafe fn dealloc(block: *mut u8, size: usize, align: usize) {
    match class_of(size, align) {
        Some(class) => HEAP.lock().push(class, block),
        None => {
            let _ = memory_free(block as usize as u64, page_aligned(size) as u64);
        }
    }
}

fn large_alloc(size: usize, align: usize) -> *mut u8 {
    if align > PAGE_SIZE {
        return core::ptr::null_mut();
    }
    request_bytes(page_aligned(size)).unwrap_or_default()
}

fn request_bytes(bytes: usize) -> Option<*mut u8> {
    let resource = crate::process::metering_handle()?;
    let va = memory_allocate(resource, bytes as u64, UserMemFlags::ReadWrite.raw());
    let address = usize::try_from(va).ok().filter(|value| *value != 0)?;
    Some(address as *mut u8)
}

const fn page_aligned(size: usize) -> usize {
    size.next_multiple_of(PAGE_SIZE)
}

const fn class_size(class: usize) -> usize {
    MIN_CLASS << class
}

fn class_of(size: usize, align: usize) -> Option<usize> {
    let want = size.max(align).max(MIN_CLASS);
    if want > MAX_CLASS {
        return None;
    }
    Some((want.next_power_of_two() / MIN_CLASS).trailing_zeros() as usize)
}
