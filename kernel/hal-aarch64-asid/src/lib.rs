//! Аллокатор Address Space Identifier'ов (ASID) для AArch64.
//!
//! Выдаёт per-AS 16-битный ASID с monotonic generation в одном 64-битном теге.
//! Тег хранится во владеющем `AtomicU64` и обновляется с помощью CAS, чтобы
//! hot-path активации AS был lock-free для уже активированных пространств.
//! При переполнении набора свободных ASID generation увеличивается, bitmap
//! обнуляется и вызывается callback (на железе - full-flush TLB во всём
//! inner-shareable домене).

#![cfg_attr(not(test), no_std)]

use core::sync::atomic::{AtomicU64, Ordering};

use collections::{BitSet, LockCell, MutexCell, bits_to_words};

/// Ширина ASID, поддерживаемая железом.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AsidWidth {
    Bits8,
    Bits16,
}

impl AsidWidth {
    /// Максимальное значение ASID (`0` зарезервирован под kernel-only).
    #[must_use]
    pub const fn max_asid(self) -> u16 {
        match self {
            Self::Bits8 => u8::MAX as u16,
            Self::Bits16 => u16::MAX,
        }
    }
}

/// Ёмкость bitmap по числу `u64`-слов: достаточна для 16-bit ASID (65536 значений).
const BITMAP_WORDS: usize = bits_to_words(u16::MAX as usize + 1);

type AsidBitmap = BitSet<BITMAP_WORDS>;

/// Внутреннее состояние, защищаемое лок-секцией.
#[derive(Debug)]
pub struct AsidAllocatorState {
    next_asid: u32,
    generation: u32,
    in_use: AsidBitmap,
}

impl AsidAllocatorState {
    pub const fn new() -> Self {
        Self {
            next_asid: 1,
            generation: 1,
            in_use: AsidBitmap::new(),
        }
    }

    pub const fn generation(&self) -> u32 {
        self.generation
    }
}

impl Default for AsidAllocatorState {
    fn default() -> Self {
        Self::new()
    }
}

const ASID_MASK: u64 = 0xFFFF_FFFF;

#[must_use]
#[inline]
pub const fn pack_tag(generation: u32, asid: u16) -> u64 {
    ((generation as u64) << 32) | (asid as u64)
}

#[must_use]
#[inline]
pub const fn unpack_generation(tag: u64) -> u32 {
    (tag >> 32) as u32
}

#[must_use]
#[inline]
pub const fn unpack_asid(tag: u64) -> u16 {
    (tag & ASID_MASK) as u16
}

/// Верхний предел числа CPU. Каждый CPU занимает один слот в `active`-таблице.
pub const MAX_CPUS: usize = 16;

/// Глобально-инициализируемый аллокатор. Ширина ASID становится известна
/// на boot (после чтения feature-регистра), поэтому ёмкость задаётся через
/// `init` уже после конструирования.
pub struct GlobalAsidAllocator {
    state: MutexCell<AsidAllocatorState>,
    max_asid: AtomicU64,
    on_rollover: fn(),
    initialized: AtomicU64,
    /// Последний выданный тег для каждого CPU (`0` - CPU не активировал ни
    /// один user-AS). Rollover читает эти слоты и резервирует их ASID-ы в
    /// новой generation, чтобы не переиспользовать ASID, который сейчас
    /// активен на другом CPU (TTBR0 которого ещё указывает на старый root).
    active: [AtomicU64; MAX_CPUS],
}

impl core::fmt::Debug for GlobalAsidAllocator {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("GlobalAsidAllocator")
            .field("max_asid", &self.max_asid.load(Ordering::Relaxed))
            .field("initialized", &self.initialized.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl GlobalAsidAllocator {
    pub const fn new(on_rollover: fn()) -> Self {
        Self {
            state: MutexCell::new(AsidAllocatorState::new()),
            max_asid: AtomicU64::new(0),
            on_rollover,
            initialized: AtomicU64::new(0),
            active: [const { AtomicU64::new(0) }; MAX_CPUS],
        }
    }

    /// Конфигурирует аллокатор после runtime-detect ширины ASID.
    /// Многократный вызов с тем же значением допустим (idempotent).
    pub fn init(&self, width: AsidWidth) {
        let max = u64::from(width.max_asid());
        // Release: пара к Acquire в `acquire`/`max_asid()`.
        self.max_asid.store(max, Ordering::Release);
        self.initialized.store(1, Ordering::Release);
    }

    fn max_asid(&self) -> u16 {
        debug_assert!(
            self.initialized.load(Ordering::Acquire) != 0,
            "GlobalAsidAllocator used before init"
        );
        self.max_asid.load(Ordering::Acquire) as u16
    }

    /// `tag_slot` - `AtomicU64` владельца AS, `cpu_id` - индекс текущего CPU.
    /// Возвращает (asid, raw_tag).
    ///
    /// Hot-path и slow-path выполняются под общим lock'ом: помимо state'а
    /// апдейтится `active[cpu_id]`, и две операции должны быть атомарны
    /// относительно конкурентного rollover'а на другом CPU.
    pub fn acquire(&self, tag_slot: &AtomicU64, cpu_id: u16) -> (u16, u64) {
        debug_assert!((cpu_id as usize) < MAX_CPUS, "cpu_id out of MAX_CPUS range");
        let active_slot = &self.active[cpu_id as usize];
        loop {
            let cached = tag_slot.load(Ordering::Acquire);
            let outcome = self.state.with_lock(|s| {
                let cached_gen = unpack_generation(cached);
                let cached_asid = unpack_asid(cached);
                if cached_asid != 0 && cached_gen == s.generation {
                    active_slot.store(cached, Ordering::Release);
                    AcquireOutcome::Hit(cached)
                } else {
                    let new_tag = self.allocate_locked(s);
                    active_slot.store(new_tag, Ordering::Release);
                    AcquireOutcome::Allocated(new_tag)
                }
            });
            match outcome {
                AcquireOutcome::Hit(tag) => return (unpack_asid(tag), tag),
                AcquireOutcome::Allocated(new_tag) => {
                    if tag_slot
                        .compare_exchange(cached, new_tag, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        return (unpack_asid(new_tag), new_tag);
                    }
                    // CAS провален - параллельный acquire поставил другой тег.
                    // Наш `new_tag` уже записан в active[cpu]; на следующей
                    // итерации мы перепишем его актуальным значением. Бит в
                    // bitmap осядет до ближайшего rollover'а - приемлемая
                    // плата за редкий contention.
                }
            }
        }
    }

    /// Освобождает ASID, описанный `raw_tag`. Не вызывает TLB-flush - записи
    /// будут переиспользованы на следующем rollover'е (lazy reclaim).
    pub fn release(&self, raw_tag: u64) {
        if raw_tag == 0 {
            return;
        }
        let tag_gen = unpack_generation(raw_tag);
        let tag_asid = unpack_asid(raw_tag);
        if tag_asid == 0 {
            return;
        }
        self.state.with_lock(|s| {
            // Освобождать имеет смысл только если поколение не сменилось.
            if s.generation == tag_gen {
                // Снимать бит безопасно даже без TLB-flush: AS, который раньше
                // владел этим тегом, уже мёртв, а MMU не поднимет stale-запись
                // - мы не отдаём этот ASID никому до сброса bitmap или ручной
                // выдачи через next_asid.
                s.in_use.clear(tag_asid as usize);
            }
        });
    }

    fn allocate_locked(&self, s: &mut AsidAllocatorState) -> u64 {
        let max = u32::from(self.max_asid());
        while s.next_asid <= max {
            let idx = s.next_asid as usize;
            if !s.in_use.is_set(idx) {
                let asid = s.next_asid as u16;
                s.in_use.set(idx);
                s.next_asid += 1;
                return pack_tag(s.generation, asid);
            }
            s.next_asid += 1;
        }
        // Rollover. Generation == 0 зарезервирован для "никогда не активированного"
        // тега, поэтому пропускаем его при wrap-around.
        s.generation = s.generation.wrapping_add(1);
        if s.generation == 0 {
            s.generation = 1;
        }
        s.in_use.clear_all();
        // Резервируем ASID-ы, которые сейчас активны на других CPU: их TLB
        // и TTBR0 ещё ссылаются на старый root, и переиспользование разорвало
        // бы изоляцию AS до того, как этот CPU сделает следующий context-switch.
        for slot in &self.active {
            let active_raw = slot.load(Ordering::Acquire);
            let active_asid = unpack_asid(active_raw);
            if active_asid != 0 {
                s.in_use.set(active_asid as usize);
            }
        }
        s.next_asid = 1;
        (self.on_rollover)();
        // Ищем первый свободный после reservation'а active-ASID-ов. Поскольку
        // активных CPU не больше MAX_CPUS << max_asid, свободный гарантированно
        // существует.
        while s.next_asid <= max && s.in_use.is_set(s.next_asid as usize) {
            s.next_asid += 1;
        }
        debug_assert!(
            s.next_asid <= max,
            "all ASIDs reserved by active CPUs after rollover"
        );
        let asid = s.next_asid as u16;
        s.in_use.set(asid as usize);
        s.next_asid += 1;
        pack_tag(s.generation, asid)
    }
}

enum AcquireOutcome {
    Hit(u64),
    Allocated(u64),
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicU64, AtomicUsize, Ordering},
        },
        thread,
    };

    use super::*;

    fn make(width: AsidWidth, on_rollover: fn()) -> GlobalAsidAllocator {
        let alloc = GlobalAsidAllocator::new(on_rollover);
        alloc.init(width);
        alloc
    }

    fn noop() {}

    static ROLLOVER_COUNT: AtomicUsize = AtomicUsize::new(0);

    fn count_rollover() {
        ROLLOVER_COUNT.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn acquires_distinct_within_generation() {
        let alloc = make(AsidWidth::Bits8, noop);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..100 {
            let slot = AtomicU64::new(0);
            let (asid, _tag) = alloc.acquire(&slot, 0);
            assert!(asid >= 1);
            assert!(asid <= AsidWidth::Bits8.max_asid());
            assert!(seen.insert(asid), "duplicate asid {asid}");
        }
    }

    #[test]
    fn same_handle_returns_same_asid_within_generation() {
        let alloc = make(AsidWidth::Bits8, noop);
        let slot = AtomicU64::new(0);
        let (asid_a, tag_a) = alloc.acquire(&slot, 0);
        let (asid_b, tag_b) = alloc.acquire(&slot, 0);
        assert_eq!(asid_a, asid_b);
        assert_eq!(tag_a, tag_b);
    }

    #[test]
    fn rollover_bumps_generation_and_calls_callback() {
        ROLLOVER_COUNT.store(0, Ordering::SeqCst);
        let alloc = make(AsidWidth::Bits8, count_rollover);
        let max = AsidWidth::Bits8.max_asid() as usize;
        let gen_before = alloc.state.with_lock(|s| s.generation);
        for _ in 0..max {
            let slot = AtomicU64::new(0);
            let _ = alloc.acquire(&slot, 0);
        }
        assert_eq!(ROLLOVER_COUNT.load(Ordering::SeqCst), 0);
        let slot = AtomicU64::new(0);
        let (_asid, _tag) = alloc.acquire(&slot, 0);
        assert_eq!(ROLLOVER_COUNT.load(Ordering::SeqCst), 1);
        let gen_after = alloc.state.with_lock(|s| s.generation);
        assert_ne!(gen_before, gen_after);
    }

    #[test]
    fn rollover_invalidates_old_handles() {
        let alloc = make(AsidWidth::Bits8, noop);
        let slot = AtomicU64::new(0);
        let (_asid_old, tag_old) = alloc.acquire(&slot, 0);
        // Доводим аллокатор до rollover: первый acquire уже занял asid=1,
        // нужно ещё `max` acquire'ов через другие slot'ы, чтобы next_asid
        // переполнился и сработал rollover.
        let max = AsidWidth::Bits8.max_asid() as usize;
        for _ in 0..max {
            let s = AtomicU64::new(0);
            let _ = alloc.acquire(&s, 0);
        }
        // Следующий вызов через тот же slot должен заметить смену поколения.
        let (_asid_new, tag_new) = alloc.acquire(&slot, 0);
        assert_ne!(tag_old, tag_new);
        assert_ne!(unpack_generation(tag_old), unpack_generation(tag_new));
    }

    #[test]
    fn release_frees_slot_for_reuse() {
        let alloc = make(AsidWidth::Bits8, noop);
        // Наберём ASID, отпустим один, потом убедимся, что free-bit снят.
        let slot = AtomicU64::new(0);
        let (asid, tag) = alloc.acquire(&slot, 0);
        alloc.release(tag);
        let in_use = alloc.state.with_lock(|s| s.in_use.is_set(asid as usize));
        assert!(!in_use, "release must clear bitmap bit");
    }

    #[test]
    fn rollover_preserves_active_asids_per_cpu() {
        // CPU0 удерживает AS_A; CPU1 заполняет диапазон ASID и упирается в
        // rollover. После rollover'а ASID, активный на CPU0, не должен быть
        // выдан другому AS - иначе два разных root окажутся под одним ASID.
        let alloc = make(AsidWidth::Bits8, noop);
        let slot_a = AtomicU64::new(0);
        let (asid_active_on_cpu0, _) = alloc.acquire(&slot_a, 0);

        let max = AsidWidth::Bits8.max_asid() as usize;
        let mut other_tags = Vec::with_capacity(max);
        for _ in 0..max {
            let s = AtomicU64::new(0);
            let (asid, _) = alloc.acquire(&s, 1);
            other_tags.push(asid);
        }
        let post_rollover_gen = alloc.state.with_lock(|s| s.generation);
        assert!(post_rollover_gen >= 2, "rollover must have happened");

        // Бит, удерживающий active-ASID CPU0, должен быть выставлен в новой
        // generation, чтобы будущие выдачи этот ASID не задели.
        let active_reserved = alloc
            .state
            .with_lock(|s| s.in_use.is_set(asid_active_on_cpu0 as usize));
        assert!(
            active_reserved,
            "ASID {asid_active_on_cpu0} активен на CPU0, но не зарезервирован после rollover"
        );

        // Свежие acquire'ы на CPU1 не должны выдать тот же ASID.
        for _ in 0..max - 1 {
            let s = AtomicU64::new(0);
            let (asid, _) = alloc.acquire(&s, 1);
            assert_ne!(
                asid, asid_active_on_cpu0,
                "ASID {asid_active_on_cpu0} реиспользован после rollover"
            );
        }
    }

    #[test]
    fn concurrent_acquire_no_duplicates() {
        const THREADS: usize = 4;
        const PER_THREAD: usize = 64;
        let alloc = Arc::new(make(AsidWidth::Bits16, noop));
        let mut handles = Vec::new();
        for thread_idx in 0..THREADS {
            let a = alloc.clone();
            let cpu_id = (thread_idx as u16) % (MAX_CPUS as u16);
            handles.push(thread::spawn(move || {
                let mut got = Vec::new();
                for _ in 0..PER_THREAD {
                    let slot = AtomicU64::new(0);
                    let (asid, _) = a.acquire(&slot, cpu_id);
                    got.push(asid);
                }
                got
            }));
        }
        let mut all = Vec::new();
        for h in handles {
            all.extend(h.join().unwrap());
        }
        let mut sorted = all.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), all.len(), "duplicate ASID across threads");
    }
}
