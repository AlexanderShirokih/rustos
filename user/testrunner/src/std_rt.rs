//! E2E платформенного слоя из EL0: куча по классам размера, futex-взаимоисключение и
//! локальные данные потока с деструкторами.
//!
//! Слой обслуживает стандартную библиотеку и вызывающих внутри дерева не имеет. Его исполняют
//! эти тесты.

use alloc::vec::Vec;
use core::{
    ptr,
    sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed},
};

use kernel_tests::kernel_test;
use runtime::{Mutex, Priority, Timeout, alloc as rt_alloc, dealloc as rt_dealloc, spawn};

const STACK_SIZE: u64 = 0x4000;

const JOIN_TIMEOUT: Timeout = Timeout::from_secs(5);

const CLASS_SIZES: [usize; 8] = [16, 32, 64, 128, 256, 512, 1024, 2048];

/// Блок каждого класса выдаётся, полностью пишется и возвращается в список.
#[kernel_test]
fn std_rt_heap_serves_every_size_class() {
    for size in CLASS_SIZES {
        for round in 0..3u8 {
            // SAFETY: размер ненулевой, выравнивание - степень двойки.
            let block = unsafe { rt_alloc(size, 16) };
            kernel_tests::kassert!(!block.is_null());

            // SAFETY: блок выдан под `size` байт и принадлежит только этому кадру.
            unsafe { ptr::write_bytes(block, round, size) };
            // SAFETY: см. выше, читаем ровно записанное.
            let first = unsafe { block.read() };
            // SAFETY: последний байт лежит внутри блока.
            let last = unsafe { block.add(size - 1).read() };
            kernel_tests::kassert_eq!(first, round);
            kernel_tests::kassert_eq!(last, round);

            // SAFETY: блок выдан `rt_alloc` с теми же size и align и ещё не освобождался.
            unsafe { rt_dealloc(block, size, 16) };
        }
    }
}

const LIVE_BLOCKS: usize = 64;

/// Одновременно живые блоки одного класса не пересекаются.
#[kernel_test]
fn std_rt_heap_blocks_do_not_overlap() {
    const SIZE: usize = 64;

    let mut blocks: Vec<*mut u8> = Vec::with_capacity(LIVE_BLOCKS);
    for index in 0..LIVE_BLOCKS {
        // SAFETY: размер ненулевой, выравнивание - степень двойки.
        let block = unsafe { rt_alloc(SIZE, 16) };
        kernel_tests::kassert!(!block.is_null());
        // SAFETY: блок выдан под SIZE байт и пока принадлежит только этому кадру.
        unsafe { ptr::write_bytes(block, u8::try_from(index).unwrap_or(0), SIZE) };
        blocks.push(block);
    }

    for (index, block) in blocks.iter().enumerate() {
        // SAFETY: блок жив, значение писали выше.
        let stored = unsafe { block.read() };
        kernel_tests::kassert_eq!(stored, u8::try_from(index).unwrap_or(0));
    }

    for block in blocks {
        // SAFETY: блок выдан `rt_alloc` с теми же size и align и ещё не освобождался.
        unsafe { rt_dealloc(block, SIZE, 16) };
    }
}

/// Запрос сверх крупнейшего класса уходит прямо к ядру и возвращается ему же.
#[kernel_test]
fn std_rt_heap_serves_large_allocations() {
    for size in [4096usize, 8192, 64 * 1024] {
        // SAFETY: размер ненулевой, выравнивание - степень двойки.
        let block = unsafe { rt_alloc(size, 16) };
        kernel_tests::kassert!(!block.is_null());
        // SAFETY: блок выдан под `size` байт и принадлежит только этому кадру.
        unsafe { ptr::write_bytes(block, 0xC3, size) };
        // SAFETY: последний байт лежит внутри блока.
        let last = unsafe { block.add(size - 1).read() };
        kernel_tests::kassert_eq!(last, 0xC3);
        // SAFETY: блок выдан `rt_alloc` с теми же size и align и ещё не освобождался.
        unsafe { rt_dealloc(block, size, 16) };
    }
}

/// Блок выровнен по запрошенной границе, даже когда она больше размера.
#[kernel_test]
fn std_rt_heap_honours_alignment() {
    for align in [16usize, 32, 64, 128, 256, 512, 1024, 2048] {
        // SAFETY: размер ненулевой, выравнивание - степень двойки.
        let block = unsafe { rt_alloc(8, align) };
        kernel_tests::kassert!(!block.is_null());
        kernel_tests::kassert_eq!(block as usize % align, 0);
        // SAFETY: блок выдан `rt_alloc` с теми же size и align и ещё не освобождался.
        unsafe { rt_dealloc(block, 8, align) };
    }
}

const LOCK_WORKERS: u64 = 4;

const LOCK_ITERS: u64 = 500;

static COUNTER: Mutex<u64> = Mutex::new(0);

extern "C" fn lock_worker(_arg: usize) -> u32 {
    for _ in 0..LOCK_ITERS {
        let mut counter = COUNTER.lock();
        *counter += 1;
    }
    0
}

/// Инкремент неатомарного счётчика под `Mutex` не теряет ни одного шага.
#[kernel_test]
fn std_rt_lock_serialises_workers() {
    {
        let mut counter = COUNTER.lock();
        *counter = 0;
    }

    let threads: [_; LOCK_WORKERS as usize] = core::array::from_fn(|_| {
        spawn(lock_worker, 0, STACK_SIZE, Priority::new(1)).expect("spawn worker")
    });
    for thread in threads {
        thread
            .join(JOIN_TIMEOUT)
            .expect("worker joins before timeout");
    }

    let counter = COUNTER.lock();
    kernel_tests::kassert_eq!(*counter, LOCK_WORKERS * LOCK_ITERS);
}

/// Стартовый handle доезжает от точки входа до потребителя и уходит первому вызову.
#[kernel_test]
fn std_rt_start_hands_out_the_bootstrap_handle_once() {
    let root = crate::bootstrap_handle();
    runtime::start(root.raw());
    let published = runtime::take_bootstrap().expect("bootstrap handle is published");
    kernel_tests::kassert_eq!(published.raw(), root.raw());
    kernel_tests::kassert!(runtime::take_bootstrap().is_none());
}

const RANDOM_MAX_LEN: usize = 17;

/// Буфер любой длины заполняется целиком, а хвост короче слова не роняет генератор.
#[kernel_test]
fn std_rt_random_fills_buffers_of_any_length() {
    let mut buffer = [0u8; RANDOM_MAX_LEN];
    for len in 0..=RANDOM_MAX_LEN {
        runtime::random::fill_bytes(&mut buffer[..len]);
    }

    // SplitMix64 биективен, а состояние сдвигается на каждый вызов: два подряд идущих слова
    // различаются всегда, поэтому проверка не плавает.
    let mut first = [0u8; 8];
    let mut second = [0u8; 8];
    runtime::random::fill_bytes(&mut first);
    runtime::random::fill_bytes(&mut second);
    kernel_tests::kassert!(first != second);
}

const TLS_WORKERS: u32 = 3;

static TLS_KEY: AtomicU64 = AtomicU64::new(0);

static TLS_MISMATCHES: AtomicU32 = AtomicU32::new(0);

static TLS_NEXT_VALUE: AtomicU64 = AtomicU64::new(1);

static TLS_DESTROYED: AtomicU32 = AtomicU32::new(0);

unsafe extern "C" fn count_destroyed(_value: *mut u8) {
    TLS_DESTROYED.fetch_add(1, Relaxed);
}

extern "C" fn tls_worker(_arg: usize) -> u32 {
    let key = TLS_KEY.load(Relaxed) as usize;
    let value = TLS_NEXT_VALUE.fetch_add(1, Relaxed);

    // SAFETY: ключ выдан `create` в спавнящем потоке до запуска рабочих.
    if !unsafe { runtime::tls::get(key) }.is_null() {
        TLS_MISMATCHES.fetch_add(1, Relaxed);
    }
    // SAFETY: см. выше; значение - непрозрачный для слоя указатель.
    unsafe { runtime::tls::set(key, value as usize as *mut u8) };

    for _ in 0..64 {
        runtime::thread_yield();
        // SAFETY: ключ выдан `create`, слот принадлежит этому потоку.
        if unsafe { runtime::tls::get(key) } as usize as u64 != value {
            TLS_MISMATCHES.fetch_add(1, Relaxed);
        }
    }

    0
}

/// Значение под ключом принадлежит потоку и переживает переключения контекста, а уход потока
/// гоняет деструктор.
#[kernel_test]
fn std_rt_thread_locals_are_private_and_destroyed() {
    let key = runtime::tls::create(Some(count_destroyed));
    kernel_tests::kassert!(key != 0);
    TLS_KEY.store(key as u64, Relaxed);
    TLS_MISMATCHES.store(0, Relaxed);
    TLS_DESTROYED.store(0, Relaxed);

    let threads: [_; TLS_WORKERS as usize] = core::array::from_fn(|_| {
        spawn(tls_worker, 0, STACK_SIZE, Priority::new(1)).expect("spawn worker")
    });
    for thread in threads {
        thread
            .join(JOIN_TIMEOUT)
            .expect("worker joins before timeout");
    }

    kernel_tests::kassert_eq!(TLS_MISMATCHES.load(Relaxed), 0);
    kernel_tests::kassert_eq!(TLS_DESTROYED.load(Relaxed), TLS_WORKERS);

    // SAFETY: ключ выдан `create` выше, рабочие потоки завершены, больше им не пользуются.
    unsafe { runtime::tls::destroy(key) };
}
