//! Раздаёт адресным пространствам AArch64 идентификаторы ASID и отслеживает их
//! переиспользование.
//!
//! [`GlobalAsidAllocator`] выдаёт по [`AsidWidth`] 8- либо 16-битный ASID вместе с номером
//! поколения. Пара упакована в один 64-битный тег, который владелец адресного пространства
//! держит в своём `AtomicU64`.
//!
//! `acquire` берёт общий лок аллокатора на каждом вызове. Под локом тег текущего поколения
//! принимается как есть, а устаревший заменяется свежевыделенным. Свежий тег публикуется в
//! слоте владельца по CAS. Проигранный CAS означает чужой параллельный `acquire`, и вызов
//! повторяется с новым значением слота.
//!
//! Когда свободные ASID кончаются, номер поколения растёт, набор свободных обнуляется и
//! вызывается заданный callback. На железе этот callback сбрасывает TLB во всём
//! inner-shareable домене.
//!
//! Ширину ASID определяет платформенный слой и передаёт её в `init` до первого `acquire`.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(test, allow(clippy::std_instead_of_core))]
#![allow(clippy::cast_possible_truncation)]

use core::sync::atomic::{AtomicU64, Ordering};

use kbase::{BitSet, LockCell, MutexCell, bits_to_words};

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

/// Глобально-инициализируемый аллокатор. Ширина ASID становится известна
/// на boot (после чтения feature-регистра), и ёмкость задаётся через
/// `init` уже после конструирования.
pub struct GlobalAsidAllocator<const CPUS: usize> {
    state: MutexCell<AsidAllocatorState>,
    max_asid: AtomicU64,
    on_rollover: fn(),
    initialized: AtomicU64,
    active: [AtomicU64; CPUS],
}

impl<const CPUS: usize> core::fmt::Debug for GlobalAsidAllocator<CPUS> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("GlobalAsidAllocator")
            .field("max_asid", &self.max_asid.load(Ordering::Relaxed))
            .field("initialized", &self.initialized.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl<const CPUS: usize> GlobalAsidAllocator<CPUS> {
    /// `on_rollover` выполняется под локом аллокатора, на CPU, чей `acquire` довёл диапазон
    /// ASID до исчерпания. Вызов внутри него `acquire`/`release` этого же аллокатора
    /// повторно берёт тот же лок и приводит к deadlock.
    pub const fn new(on_rollover: fn()) -> Self {
        Self {
            state: MutexCell::new(AsidAllocatorState::new()),
            max_asid: AtomicU64::new(0),
            on_rollover,
            initialized: AtomicU64::new(0),
            active: [const { AtomicU64::new(0) }; CPUS],
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

    /// Отдаёт CPU `cpu_id` действующий ASID адресного пространства и отмечает его активным.
    /// Аргументы:
    ///  - `tag_slot` - `AtomicU64`, в котором владелец AS хранит свой тег,
    ///  - `cpu_id` - индекс текущего CPU.
    ///
    /// Тег из `tag_slot` переиспользуется, пока его поколение совпадает с текущим. Иначе
    /// выделяется свежий ASID. Возвращается пара `(asid, raw_tag)`.
    ///
    /// Оба пути идут под общим локом. Вместе с состоянием аллокатора вызов меняет и
    /// `active[cpu_id]`, а эти две записи обязаны быть атомарны относительно rollover,
    /// идущего на другом CPU.
    ///
    /// Вызов с `cpu_id` вне диапазона `0..CPUS` паникует в debug-сборке (`debug_assert`). В
    /// release он возвращает `(0, 0)` вместо выделения ASID. Ноль здесь - тот же резерв, что
    /// и в [`AsidWidth::max_asid`], и успешный вызов его не отдаёт.
    pub fn acquire(&self, tag_slot: &AtomicU64, cpu_id: u16) -> (u16, u64) {
        let Some(active_slot) = self.active.get(cpu_id as usize) else {
            debug_assert!(false, "cpu_id is outside the per-CPU table");
            return (0, 0);
        };
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
                    // bitmap осядет до ближайшего rollover - приемлемая
                    // плата за редкий contention.
                }
            }
        }
    }

    /// Освобождает ASID, описанный `raw_tag`. Не вызывает TLB-flush - записи
    /// будут переиспользованы на следующем rollover (lazy reclaim).
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
            // Тег `0` в слоте - CPU ещё не активировал ни одного user-AS.
            if active_asid != 0 {
                s.in_use.set(active_asid as usize);
            }
        }
        s.next_asid = 1;
        (self.on_rollover)();
        // Ищем первый свободный после резервирования active-ASID. Поскольку
        // активных CPU не больше `CPUS` << max_asid, свободный гарантированно
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

    const TEST_CPUS: usize = 16;

    type TestAsidAllocator = GlobalAsidAllocator<TEST_CPUS>;

    fn make(width: AsidWidth, on_rollover: fn()) -> TestAsidAllocator {
        let alloc = TestAsidAllocator::new(on_rollover);
        alloc.init(width);
        alloc
    }

    fn noop() {}

    static ROLLOVER_COUNT: AtomicUsize = AtomicUsize::new(0);

    fn count_rollover() {
        ROLLOVER_COUNT.fetch_add(1, Ordering::SeqCst);
    }

    const RACE_HOLDERS: usize = 3;
    const RACE_MIN_ITERS: usize = 4_000;
    const RACE_MAX_ITERS: usize = 200_000;
    const RACE_ROLLOVER_TARGET: usize = 8;
    const RACE_CHURN_LIMIT: usize = 1_000_000;

    static RACE_ROLLOVERS: AtomicUsize = AtomicUsize::new(0);

    fn count_race_rollover() {
        RACE_ROLLOVERS.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn race_rollover_never_reuses_an_asid_active_on_another_cpu() {
        // Узкий диапазон: rollover наступает за сотни выдач, а не за 65 тысяч,
        // и держатели проходят его многократно за прогон.
        let alloc = Arc::new(make(AsidWidth::Bits8, count_race_rollover));
        RACE_ROLLOVERS.store(0, Ordering::SeqCst);

        // Активные ASID держателей: слот на держателя, `0` - между выдачами.
        let active: Arc<Vec<AtomicU64>> = Arc::new(
            (0..RACE_HOLDERS)
                .map(|_| AtomicU64::new(0))
                .collect::<Vec<_>>(),
        );
        let stop = Arc::new(AtomicU64::new(0));
        let mut holders = Vec::with_capacity(RACE_HOLDERS);

        for index in 0..RACE_HOLDERS {
            let alloc = Arc::clone(&alloc);
            let active = Arc::clone(&active);
            holders.push(thread::spawn(move || {
                let slot = AtomicU64::new(0);
                let cpu_id = index as u16;
                let mut reused = Vec::new();
                for iteration in 0..RACE_MAX_ITERS {
                    // Своя отметка снимается до выдачи, поэтому после неё прежний ASID
                    // держателем не занят, и чужая выдача его законно берёт.
                    active[index].store(0, Ordering::Release);
                    let (asid, _tag) = alloc.acquire(&slot, cpu_id);

                    active[index].store(u64::from(asid), Ordering::Release);

                    // Rollover обязан зарезервировать ASID, активные на чужих
                    // CPU: снятый бит означает, что ASID ушёл в свободные и
                    // будет выдан второму адресному пространству. Отметку
                    // сосед ставит после своей выдачи, а аллокатор учитывает её с
                    // самой выдачи, поэтому опубликованный ASID активен.
                    alloc.state.with_lock(|state| {
                        for (other, slot) in active.iter().enumerate() {
                            if other == index {
                                continue;
                            }
                            let foreign = slot.load(Ordering::Acquire) as u16;
                            if foreign != 0 && !state.in_use.is_set(foreign as usize) {
                                reused.push(foreign);
                            }
                        }
                    });

                    // Держатель работает, пока диапазон не пройден нужное
                    // число раз. На медленной машине это дольше, а не реже.
                    if iteration >= RACE_MIN_ITERS
                        && RACE_ROLLOVERS.load(Ordering::SeqCst) >= RACE_ROLLOVER_TARGET
                    {
                        break;
                    }
                }
                active[index].store(0, Ordering::Release);
                reused
            }));
        }

        let churn = {
            let alloc = Arc::clone(&alloc);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                // Свежий слот на каждой выдаче гонит диапазон к исчерпанию:
                // rollover идут всё время, пока держатели крутят свои слоты.
                let mut spins = 0;
                while stop.load(Ordering::Acquire) == 0 && spins < RACE_CHURN_LIMIT {
                    let slot = AtomicU64::new(0);
                    let _ = alloc.acquire(&slot, RACE_HOLDERS as u16);
                    spins += 1;
                }
            })
        };

        let mut reused = Vec::new();
        for holder in holders {
            reused.extend(holder.join().expect("holder joins"));
        }
        stop.store(1, Ordering::Release);
        churn.join().expect("churn joins");

        assert!(
            reused.is_empty(),
            "ASIDs {reused:?} were taken from a live holder across a rollover"
        );
        // Прогон без единого rollover проверил бы только обычную выдачу.
        assert!(
            RACE_ROLLOVERS.load(Ordering::SeqCst) >= 2,
            "the run must cross a rollover at least twice"
        );
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
        // Доводим аллокатор до rollover, крутя до фактической смены generation
        // (а не магическим `0..max`).
        let _ = drive_to_rollover(&alloc, 0);
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
        // rollover. После rollover ASID, активный на CPU0, не должен быть
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
            "ASID {asid_active_on_cpu0} active on CPU0 must be reserved after rollover"
        );

        // Свежие acquire на CPU1 не должны выдать тот же ASID.
        for _ in 0..max - 1 {
            let s = AtomicU64::new(0);
            let (asid, _) = alloc.acquire(&s, 1);
            assert_ne!(
                asid, asid_active_on_cpu0,
                "ASID {asid_active_on_cpu0} must not be reused after rollover"
            );
        }
    }

    fn drive_to_rollover(alloc: &TestAsidAllocator, cpu_id: u16) -> (u32, u32) {
        let gen_before = alloc.state.with_lock(|s| s.generation);
        let max = alloc.max_asid() as usize;
        // Запас итераций: не больше двух полных проходов по диапазону.
        for _ in 0..(max * 2 + 4) {
            let slot = AtomicU64::new(0);
            let _ = alloc.acquire(&slot, cpu_id);
            let gen_now = alloc.state.with_lock(|s| s.generation);
            if gen_now != gen_before {
                return (gen_before, gen_now);
            }
        }
        panic!("rollover did not occur within {} iterations", max * 2 + 4);
    }

    #[test]
    fn rollover_changes_generation_robustly() {
        let alloc = make(AsidWidth::Bits8, noop);
        let (before, after) = drive_to_rollover(&alloc, 0);
        assert_ne!(before, after, "generation must change on rollover");
    }

    #[test]
    fn release_does_not_grant_immediate_reuse_before_rollover() {
        // Лениво-освобождающая семантика: release снимает bit, но next_asid
        // продолжает монотонно расти, поэтому отпущенный ASID не выдаётся
        // повторно до rollover.
        let alloc = make(AsidWidth::Bits8, noop);

        let slot0 = AtomicU64::new(0);
        let (asid0, tag0) = alloc.acquire(&slot0, 0);

        // Берём ещё несколько ASID, чтобы next_asid ушёл вперёд.
        for _ in 0..3 {
            let s = AtomicU64::new(0);
            let _ = alloc.acquire(&s, 0);
        }

        // Отпускаем самый первый ASID.
        alloc.release(tag0);

        // Следующая выдача не должна сразу переиспользовать asid0 (до rollover).
        let s = AtomicU64::new(0);
        let (asid_next, _) = alloc.acquire(&s, 0);
        assert_ne!(
            asid_next, asid0,
            "released ASID must not be reused immediately before rollover (lazy reclaim)"
        );

        // After rollover the bitmap is cleared; asid0 is available for
        // reuse. The lazy-reclaim invariant (no immediate reuse before
        // rollover) is verified by assert_ne above.
        drive_to_rollover(&alloc, 0);
    }

    #[test]
    fn release_of_stale_tag_does_not_clear_current_generation_bit() {
        // release тега из старого поколения не должен задеть текущую generation.
        let alloc = make(AsidWidth::Bits8, noop);
        let slot = AtomicU64::new(0);
        let (asid, tag_old) = alloc.acquire(&slot, 0);

        // Доводим до rollover, чтобы generation сменилась.
        let (_b, gen_after) = drive_to_rollover(&alloc, 1);
        assert_ne!(unpack_generation(tag_old), gen_after);

        // Заново активируем slot в новой generation -> получаем свежий tag.
        let (asid_new, _tag_new) = alloc.acquire(&slot, 0);
        let set_before = alloc
            .state
            .with_lock(|s| s.in_use.is_set(asid_new as usize));
        assert!(set_before, "the freshly acquired ASID must be in use");

        // release устаревшего тега (старая generation) не должен снять bit.
        alloc.release(tag_old);
        let set_after = alloc
            .state
            .with_lock(|s| s.in_use.is_set(asid_new as usize));
        assert!(
            set_after,
            "releasing a stale tag must not clear a bit of the current generation"
        );
        let _ = asid;
    }

    #[test]
    fn release_zero_is_noop() {
        let alloc = make(AsidWidth::Bits8, noop);
        // Занимаем один ASID, фиксируем состояние.
        let slot = AtomicU64::new(0);
        let (asid, _tag) = alloc.acquire(&slot, 0);
        let before = alloc.state.with_lock(|s| s.in_use.is_set(asid as usize));
        assert!(before);

        // release(0) и release тега с asid=0 - no-op, ничего не падает/не меняется.
        alloc.release(0);
        alloc.release(pack_tag(1, 0));

        let after = alloc.state.with_lock(|s| s.in_use.is_set(asid as usize));
        assert_eq!(before, after, "release(0) must not change the state");
    }

    #[test]
    fn bits16_max_asid_boundary() {
        let alloc = make(AsidWidth::Bits16, noop);
        assert_eq!(alloc.max_asid(), u16::MAX);
        // Выданные ASID не должны быть нулевыми (0 зарезервирован).
        for _ in 0..200 {
            let slot = AtomicU64::new(0);
            let (asid, _) = alloc.acquire(&slot, 0);
            assert!(asid >= 1, "ASID 0 is reserved");
        }
    }

    #[test]
    fn pack_unpack_round_trip() {
        for &(generation, asid) in &[
            (1u32, 1u16),
            (0, 0),
            (0xDEAD_BEEF, 0xABCD),
            (u32::MAX, u16::MAX),
        ] {
            let tag = pack_tag(generation, asid);
            assert_eq!(unpack_generation(tag), generation);
            assert_eq!(unpack_asid(tag), asid);
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
            let cpu_id = (thread_idx as u16) % (TEST_CPUS as u16);
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
