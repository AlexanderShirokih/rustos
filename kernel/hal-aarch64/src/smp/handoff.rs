//! Параметры, которые boot CPU передаёт стартующему.
//!
//! Стартующий CPU входит с выключенными MMU и кэшами: он читает эту память
//! мимо кэша и по физическому адресу. Boot CPU публикует записи, вытесняя их
//! до точки когерентности.

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

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

/// Загрузочные стеки вторичных CPU. Живут в `.bss`: стартующий CPU находит
/// свой стек PC-относительно, до включения MMU и до всякой аллокации.
pub(super) static mut SECONDARY_STACKS: [SecondaryStack; MAX_CPUS] =
    [const { SecondaryStack([0; SECONDARY_STACK_SIZE]) }; MAX_CPUS];

/// Параметры очередного стартующего CPU. Слот один на всех: CPU поднимаются по
/// одному, и следующий не стартует, пока предыдущий не дошёл до post-MMU кода.
#[repr(C, align(128))]
pub(super) struct Handoff {
    pending_id: AtomicU64,
    lower_root: AtomicU64,
    higher_root: AtomicU64,
}

pub(super) static HANDOFF: Handoff = Handoff {
    pending_id: AtomicU64::new(0),
    lower_root: AtomicU64::new(0),
    higher_root: AtomicU64::new(0),
};

static ONLINE: AtomicUsize = AtomicUsize::new(0);

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

/// Назначает идентификатор очередному стартующему CPU. Вызывается перед
/// командой старта и вступает в силу до неё.
pub(super) fn publish_pending_id(id: u64) {
    HANDOFF.pending_id.store(id, Ordering::Relaxed);
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

/// Отмечает, что CPU дошёл до post-MMU кода.
pub(super) fn mark_online() {
    ONLINE.fetch_add(1, Ordering::Release);
}

/// Число вторичных CPU, дошедших до post-MMU кода.
pub(super) fn online_count() -> usize {
    ONLINE.load(Ordering::Acquire)
}

fn flush() {
    let start = core::ptr::from_ref(&HANDOFF) as usize;
    DataCache::<EL1>::clean_range_to_coherency(start, start + size_of::<Handoff>());
}
