//! Параметры, которые boot CPU передаёт стартующему.
//!
//! Стартующий CPU входит с выключенными MMU и кэшами: он читает эту память
//! мимо кэша и по физическому адресу. Boot CPU публикует записи, вытесняя их
//! до точки когерентности.

use core::sync::atomic::{AtomicU64, Ordering};

use hal_common::smp::OnlineCpus;
use scheduler::MAX_CPUS;

use crate::memory::regs::{common::EL1, dcache::DataCache};

/// Размер загрузочного стека вторичного CPU. Через него проходит путь от входа
/// до idle и обработчик прерывания на нём же.
pub(super) const SECONDARY_STACK_SIZE: usize = 16 * 1024;

/// Загрузочный стек вторичного CPU. Границы кратны наибольшей строке кэша
/// среди целевых ядер: стартующий CPU инвалидирует стек `dc ivac`, а эта
/// операция разрушающая и действует на строку целиком.
#[repr(C, align(128))]
pub(super) struct SecondaryStack([u8; SECONDARY_STACK_SIZE]);

/// Предел числа вторичных CPU: boot CPU идёт по стеку из линкер-скрипта и
/// стека отсюда не занимает.
pub(super) const MAX_SECONDARIES: usize = MAX_CPUS - 1;

/// Загрузочные стеки вторичных CPU. Живут в `.bss`: стартующий CPU находит
/// свой стек PC-относительно, до включения MMU и до всякой аллокации.
///
/// Индексируются порядком старта, а не идентификатором CPU: идентификатор boot
/// CPU задаёт топология, и нулю он равен не обязательно.
pub(super) static mut SECONDARY_STACKS: [SecondaryStack; MAX_SECONDARIES] =
    [const { SecondaryStack([0; SECONDARY_STACK_SIZE]) }; MAX_SECONDARIES];

/// Номер загрузочного стека в [`SECONDARY_STACKS`]. Существование значения
/// означает, что стек с таким номером есть.
#[derive(Clone, Copy)]
pub(super) struct SecondaryStackIndex(usize);

impl SecondaryStackIndex {
    /// `None` - стеков меньше, чем запрошенный номер.
    pub(super) fn new(index: usize) -> Option<Self> {
        (index < MAX_SECONDARIES).then_some(Self(index))
    }

    pub(super) fn raw(self) -> usize {
        self.0
    }
}

/// Параметры очередного стартующего CPU. Слот один на всех: CPU поднимаются по
/// одному, и следующий не стартует, пока предыдущий не дошёл до post-MMU кода.
///
/// Порядок полей повторяет разбор в `entry.rs`: стартующий CPU читает их
/// смещениями от базы блока, до включения MMU и вне типов Rust.
#[repr(C, align(128))]
pub(super) struct Handoff {
    /// Логический идентификатор: по нему CPU берёт свой per-CPU блок.
    pending_id: AtomicU64,
    /// Номер загрузочного стека в [`SECONDARY_STACKS`].
    pending_stack: AtomicU64,
    lower_root: AtomicU64,
    higher_root: AtomicU64,
}

pub(super) static HANDOFF: Handoff = Handoff {
    pending_id: AtomicU64::new(0),
    pending_stack: AtomicU64::new(0),
    lower_root: AtomicU64::new(0),
    higher_root: AtomicU64::new(0),
};

pub(super) static ONLINE: OnlineCpus = OnlineCpus::new();

/// Физические адреса корней трансляции.
pub(super) struct TranslationRoots {
    pub lower: u64,
    pub higher: u64,
}

/// Публикует корни трансляции. Вызывается boot CPU один раз, до старта первого
/// вторичного CPU.
pub(super) fn publish_roots(lower_root: u64, higher_root: u64) {
    HANDOFF.lower_root.store(lower_root, Ordering::Relaxed);
    HANDOFF.higher_root.store(higher_root, Ordering::Relaxed);
    flush();
}

/// Назначает очередному стартующему CPU идентификатор и загрузочный стек.
/// Вызывается перед командой старта и вступает в силу до неё.
///
/// `false` - номер стека вне [`SECONDARY_STACKS`]: под таким номером
/// стартующий CPU взял бы под стек чужую память.
pub(super) fn publish_pending(id: u64, stack: SecondaryStackIndex) {
    HANDOFF.pending_id.store(id, Ordering::Relaxed);
    HANDOFF
        .pending_stack
        .store(stack.raw() as u64, Ordering::Relaxed);
    flush();
}

/// Корни трансляции, опубликованные boot CPU. Читается стартующим CPU до
/// включения MMU.
pub(super) fn roots() -> TranslationRoots {
    TranslationRoots {
        lower: HANDOFF.lower_root.load(Ordering::Relaxed),
        higher: HANDOFF.higher_root.load(Ordering::Relaxed),
    }
}

fn flush() {
    let start = core::ptr::from_ref(&HANDOFF) as usize;
    DataCache::<EL1>::clean_range_to_coherency(start, start + size_of::<Handoff>());
}
