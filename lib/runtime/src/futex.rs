//! Ожидание по адресу и адресное пробуждение поверх сигнального объекта.
//!
//! Адрес отображается в бакет фиксированной таблицы, бит бакетного сигнала
//! отдаётся ждущему в личное владение на время парковки. Пробуждение, пришедшее
//! в окно между проверкой значения и парковкой, остаётся на липком бите и
//! снимает парковку сразу.
//!
//! Пробуждение поднимает биты всех слотов своего адреса, и ждущие на других
//! адресах того же бакета остаются в ожидании.
//!
//! Слот освобождает его владелец. Слот потока, снятого во время парковки, занят
//! до конца процесса. Бакет без свободных слотов переводит ожидание в опрос
//! значения.

use core::{
    ptr,
    sync::atomic::{
        AtomicU32, AtomicU64,
        Ordering::{AcqRel, Acquire, Relaxed, Release, SeqCst},
        fence,
    },
    time::Duration,
};

use syscall::{Handle, SYSCALL_RETURN_TIMEOUT, Timeout, WakeCount};

use crate::{
    clock::Instant,
    svc::{handle_close, signal_create, signal_set, signal_wait_one, thread_yield},
};

/// Слово, по адресу которого ждут и будят.
pub type Futex = AtomicU32;

/// Значение [`Futex`].
pub type Primitive = u32;

/// Слово компактного примитива синхронизации.
pub type SmallFutex = AtomicU32;

/// Значение [`SmallFutex`].
pub type SmallPrimitive = u32;

const BUCKETS: usize = 8;

const SLOTS: u32 = 31;

const IDLE_BIT: u32 = 1 << SLOTS;

const SLOT_FREE: u64 = 0;

const SLOT_OWNED: u64 = 1;

const SLOTLESS_PAUSE: Timeout = Timeout::from_micros(200);

const GOLDEN_RATIO: u64 = 0x9E37_79B9_7F4A_7C15;

const _: () = assert!(BUCKETS.is_power_of_two());

struct Bucket {
    signal: AtomicU32,
    slots: [AtomicU64; SLOTS as usize],
}

struct Parked<'a> {
    bucket: &'static Bucket,
    signal: Handle,
    slot: u32,
    address: u64,
    futex: &'a Futex,
    expected: Primitive,
}

enum Deadline {
    Infinite,
    Bounded { start: Instant, budget: u64 },
}

static TABLE: [Bucket; BUCKETS] = [const { Bucket::new() }; BUCKETS];

impl Bucket {
    const fn new() -> Self {
        Self {
            signal: AtomicU32::new(0),
            slots: [const { AtomicU64::new(SLOT_FREE) }; SLOTS as usize],
        }
    }

    fn signal(&self) -> Option<Handle> {
        Handle::new(self.signal.load(Acquire))
    }

    fn signal_installed(&self) -> Option<Handle> {
        if let Some(handle) = self.signal() {
            return Some(handle);
        }

        let created = signal_create().ok()?;
        match self
            .signal
            .compare_exchange(0, created.raw(), AcqRel, Acquire)
        {
            Ok(_) => Some(created),
            Err(existing) => {
                // Гонку выиграл другой поток: свежий хэндл лишний.
                let _ = handle_close(created);
                Handle::new(existing)
            }
        }
    }

    fn claim(&self) -> Option<u32> {
        for (index, slot) in self.slots.iter().enumerate() {
            if take_slot(slot, SLOT_FREE) {
                return u32::try_from(index).ok();
            }
        }
        None
    }

    fn detach(&self, address: u64) -> u32 {
        let mut mask = 0;
        for (index, slot) in self.slots.iter().enumerate() {
            if take_slot(slot, address) {
                mask |= 1 << index;
            }
        }
        mask
    }
}

impl Deadline {
    fn new(timeout: Option<Duration>) -> Self {
        match timeout {
            None => Self::Infinite,
            Some(limit) => Self::Bounded {
                start: Instant::now(),
                budget: u64::try_from(limit.as_nanos()).unwrap_or(u64::MAX),
            },
        }
    }

    fn remaining(&self) -> Option<Timeout> {
        match *self {
            Self::Infinite => Some(Timeout::INFINITE),
            Self::Bounded { start, budget } => budget
                .checked_sub(Instant::now().since(start).raw())
                .filter(|left| *left > 0)
                .map(Timeout::from_ns),
        }
    }
}

/// Ждёт, пока значение `futex` не перестанет быть `expected` или не придёт
/// пробуждение по его адресу. `None` - ждать бессрочно. `false` - истёк
/// `timeout`, `true` - любой другой исход (в том числе ложное пробуждение).
pub fn futex_wait(futex: &Futex, expected: Primitive, timeout: Option<Duration>) -> bool {
    wait_until(futex, expected, &Deadline::new(timeout), || {})
}

/// Тестовый разрез [`futex_wait`]: `between` вызывается в окне между проверкой
/// значения и парковкой.
#[doc(hidden)]
pub fn futex_wait_interleaved(
    futex: &Futex,
    expected: Primitive,
    timeout: Option<Duration>,
    between: impl FnOnce(),
) -> bool {
    wait_until(futex, expected, &Deadline::new(timeout), between)
}

/// Тестовый разрез [`futex_wait`]: ожидание без слота в бакете - путь, на
/// который сходит waiter, когда слоты бакета заняты.
#[doc(hidden)]
pub fn futex_wait_without_slot(
    futex: &Futex,
    expected: Primitive,
    timeout: Option<Duration>,
) -> bool {
    let bucket = bucket_for(address_of(futex));
    let deadline = Deadline::new(timeout);

    loop {
        if let Some(outcome) = slotless_round(futex, expected, bucket, &deadline) {
            return outcome;
        }
    }
}

/// Тестовый разрез: индекс бакета, обслуживающего адрес `futex`.
#[doc(hidden)]
pub fn futex_bucket(futex: &Futex) -> usize {
    bucket_index(address_of(futex))
}

/// Будит ждущих по адресу `futex`. `true`, если пробуждение кому-то адресовано.
pub fn futex_wake(futex: &Futex) -> bool {
    wake_published(futex) != 0
}

/// Будит всех ждущих по адресу `futex`.
pub fn futex_wake_all(futex: &Futex) {
    wake_published(futex);
}

fn wait_until(
    futex: &Futex,
    expected: Primitive,
    deadline: &Deadline,
    between: impl FnOnce(),
) -> bool {
    let address = address_of(futex);
    let bucket = bucket_for(address);
    let mut between = Some(between);

    loop {
        if let Some(signal) = bucket.signal_installed()
            && let Some(slot) = bucket.claim()
        {
            let parked = Parked {
                bucket,
                signal,
                slot,
                address,
                futex,
                expected,
            };
            let woken = park(&parked, deadline, &mut between);
            if let Some(taken) = bucket.slots.get(slot as usize) {
                taken.store(SLOT_FREE, Release);
            }
            return woken;
        }

        if let Some(outcome) = slotless_round(futex, expected, bucket, deadline) {
            return outcome;
        }
    }
}

fn slotless_round(
    futex: &Futex,
    expected: Primitive,
    bucket: &Bucket,
    deadline: &Deadline,
) -> Option<bool> {
    if futex.load(SeqCst) != expected {
        return Some(true);
    }

    let Some(remaining) = deadline.remaining() else {
        return Some(false);
    };

    match bucket.signal() {
        Some(signal) => {
            // Никто не поднимает IDLE_BIT, поэтому ожидание на нём истекает только по тайм-ауту.
            let pause = remaining.raw().min(SLOTLESS_PAUSE.raw());
            let _ = signal_wait_one(signal, IDLE_BIT, Timeout::from_ns(pause));
        }
        None => thread_yield(),
    }
    None
}

fn park(parked: &Parked<'_>, deadline: &Deadline, between: &mut Option<impl FnOnce()>) -> bool {
    let bit = 1 << parked.slot;
    let Some(slot) = parked.bucket.slots.get(parked.slot as usize) else {
        return false;
    };

    loop {
        // Слот в SLOT_OWNED невиден будящим, и бит снимает только его владелец.
        let _ = signal_set(parked.signal, 0, bit, WakeCount::None);
        // Адрес futex ненулевой и выровнен на 4: со служебными значениями слота
        // он не совпадает.
        slot.store(parked.address, Release);
        fence(SeqCst);

        if parked.futex.load(SeqCst) != parked.expected {
            take_slot(slot, parked.address);
            return true;
        }
        if let Some(run) = between.take() {
            run();
        }

        let Some(remaining) = deadline.remaining() else {
            return !take_slot(slot, parked.address);
        };
        let outcome = signal_wait_one(parked.signal, bit, remaining);
        if !take_slot(slot, parked.address) {
            return true;
        }
        if outcome == SYSCALL_RETURN_TIMEOUT {
            return false;
        }
    }
}

fn take_slot(slot: &AtomicU64, expected: u64) -> bool {
    // Чтение до CAS: скан чужих слотов не пишет в их строки кэша.
    slot.load(Acquire) == expected
        && slot
            .compare_exchange(expected, SLOT_OWNED, AcqRel, Relaxed)
            .is_ok()
}

fn wake_published(futex: &Futex) -> u32 {
    let address = address_of(futex);
    let bucket = bucket_for(address);

    // Ждущий публикует слот до чтения значения, будящий меняет значение до
    // чтения слотов. Общий порядок SeqCst не даёт разойтись обеим парам.
    fence(SeqCst);
    let mask = bucket.detach(address);
    if mask != 0
        && let Some(signal) = bucket.signal()
    {
        let _ = signal_set(signal, mask, 0, WakeCount::All);
    }
    mask
}

fn address_of(futex: &Futex) -> u64 {
    ptr::from_ref(futex) as usize as u64
}

#[allow(clippy::indexing_slicing)]
fn bucket_for(address: u64) -> &'static Bucket {
    // Сдвиг в `bucket_index` оставляет ровно `log2(BUCKETS)` бит, и индекс лежит в таблице.
    &TABLE[bucket_index(address)]
}

fn bucket_index(address: u64) -> usize {
    ((address >> 2).wrapping_mul(GOLDEN_RATIO) >> (u64::BITS - BUCKETS.ilog2())) as usize
}
