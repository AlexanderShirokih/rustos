//! Lock-free SPSC-кольцо в разделяемой памяти.
//!
//! [`SpscRing`] - кольцо фиксированной ёмкости поверх внешнего региона (сырой
//! указатель, трактуемый как атомики). Оно не владеет памятью и не аллоцирует.
//! Регион оборачивают ровно один producer и один consumer, все операции - по
//! `&self` через атомики.
//!
//! Раскладка: заголовок `head`/`tail` (`AtomicU32`), затем `capacity` слотов
//! (`len: AtomicU32` + payload `[AtomicU8; slot_payload]`). Индексы свободно
//! бегущие, номер слота - `idx & (capacity - 1)`. `capacity` - наибольшая
//! степень двойки числа влезающих слотов (иначе `from_raw` даёт `None`).

// Кольцо отображает внешний регион разделяемой памяти на атомик-вьюшки, что
// требует единственного raw-pointer перехода в `from_raw`. Дальше вся логика -
// безопасные операции над атомик-слайсами.
#![allow(unsafe_code)]

use core::{
    mem::{align_of, size_of},
    sync::atomic::{AtomicU8, AtomicU32, Ordering, fence},
};

/// Ошибка операции с кольцом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RingError {
    /// Кольцо полно - нет свободного слота для вставки.
    Full,
    /// Кольцо пусто - нечего извлекать.
    Empty,
    /// Кадр больше `slot_payload` (при push) либо буфер получателя короче
    /// кадра (при pop).
    TooLarge,
}

/// Результат успешной вставки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushOutcome {
    /// На момент публикации кадра других кадров в кольце не осталось: потребитель
    /// мог уже уснуть и обязан быть разбужен.
    pub wake_consumer: bool,
}

/// Результат успешного извлечения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PopOutcome {
    /// Длина извлечённого кадра в байтах.
    pub len: usize,
}

/// SPSC-кольцо фиксированной ёмкости поверх внешнего региона разделяемой памяти.
///
/// Producer-методы ([`try_push`](SpscRing::try_push)) вызывает только
/// производитель, consumer-методы ([`try_pop`](SpscRing::try_pop)) - только
/// потребитель. Это гарантирует SPSC-инвариант: `head` пишет ровно один поток,
/// `tail` - ровно один поток.
pub struct SpscRing {
    head: *const AtomicU32,
    tail: *const AtomicU32,
    slots: *const AtomicU32,
    slot_stride: usize,
    slot_payload: usize,
    capacity: usize,
}

// SAFETY: `SpscRing` хранит сырые указатели на регион разделяемой памяти; все
// доступы к нему идут через атомики с явным порядком памяти (Acquire/Release/
// Relaxed). `head` пишет только producer, `tail` - только consumer, payload -
// через атомарные операции слотов, поэтому передача значения в другой поток
// безопасна при соблюдении SPSC-инварианта.
unsafe impl Send for SpscRing {}
// SAFETY: все операции по `&self` синхронизированы атомиками с корректным
// порядком памяти (Release на публикации индекса, Acquire на наблюдении), что
// даёт happens-before для payload между производителем и потребителем.
unsafe impl Sync for SpscRing {}

impl SpscRing {
    /// Размер заголовка региона в байтах (`head` + `tail`).
    #[must_use]
    pub const fn header_bytes() -> usize {
        2 * size_of::<AtomicU32>()
    }

    /// Размер одного слота в байтах для заданного `slot_payload`: `len`-поле
    /// плюс payload, выровненные вверх до `align_of::<AtomicU32>()`, чтобы поле
    /// `len` каждого слота оставалось выровненным при любом `slot_payload`.
    #[must_use]
    pub const fn slot_bytes(slot_payload: usize) -> usize {
        let raw = size_of::<AtomicU32>() + slot_payload;
        let align = align_of::<AtomicU32>();
        (raw + align - 1) & !(align - 1)
    }

    /// Минимальный размер региона (в байтах), необходимый под кольцо с заданными
    /// `capacity` (число слотов) и `slot_payload`. Позволяет вызывающему
    /// корректно выделить регион перед оборачиванием.
    ///
    /// `capacity` должна быть степенью двойки - это инвариант кольца. Для
    /// нестепеней двойки результат всё равно вычисляется арифметически, но
    /// [`from_raw`](SpscRing::from_raw) округлит фактическую ёмкость вниз.
    #[must_use]
    pub const fn region_bytes(capacity: usize, slot_payload: usize) -> usize {
        Self::header_bytes() + capacity * Self::slot_bytes(slot_payload)
    }

    /// Оборачивает регион разделяемой памяти в SPSC-кольцо.
    ///
    /// Аргументы:
    ///  - `base` - начало региона,
    ///  - `region_len` - длина региона в байтах,
    ///  - `slot_payload` - полезная ёмкость одного слота.
    ///
    /// Фактическая `capacity` - наибольшая степень двойки от числа слотов, влезающих в
    /// регион после заголовка.
    ///
    /// Возвращает `None`, если регион не вмещает заголовок и хотя бы один слот,
    /// либо если `base` недостаточно выровнен под [`AtomicU32`].
    ///
    /// # Safety
    ///
    /// Вызывающий гарантирует, что:
    /// - `base` валиден на `region_len` байт и замаплен на чтение-запись в
    ///   текущем адресном пространстве, и остаётся замаплен на всё время жизни
    ///   возвращённого `SpscRing`;
    /// - один и тот же регион оборачивают **ровно один** producer и **ровно
    ///   один** consumer (SPSC-инвариант);
    /// - выравнивание `base` достаточно для [`AtomicU32`] (регион страничный, что
    ///   этому удовлетворяет). Функция дополнительно проверяет это и возвращает
    ///   `None` при недостаточном выравнивании.
    #[must_use]
    pub unsafe fn from_raw(base: *mut u8, region_len: usize, slot_payload: usize) -> Option<Self> {
        if base.is_null() {
            return None;
        }
        // head/tail и поля len слотов читаются как AtomicU32, поэтому база (и, как
        // следствие, кратные ей смещения) должна быть выровнена. slot_stride кратен
        // размеру AtomicU32, что сохраняет выравнивание полей len во всех слотах.
        let head: *mut AtomicU32 = base.cast();
        if !head.is_aligned() {
            return None;
        }

        let header = Self::header_bytes();
        if region_len < header {
            return None;
        }

        let slot_stride = Self::slot_bytes(slot_payload);
        if slot_stride == 0 {
            return None;
        }
        let slots_fit = (region_len - header) / slot_stride;
        if slots_fit == 0 {
            return None;
        }
        let capacity = prev_power_of_two(slots_fit);
        if capacity == 0 {
            return None;
        }

        // SAFETY: проверено `region_len >= header`, поэтому смещение на один
        // AtomicU32 от выровненной базы остаётся в пределах заголовка
        // (`header == 2 * size_of::<AtomicU32>()`) и сохраняет выравнивание.
        let tail = unsafe { head.add(1) };
        // SAFETY: смещение на два AtomicU32 - это `header` байт от базы, и оно лежит
        // в пределах региона (region_len >= header + slot_stride, т.к. slots_fit >= 1);
        // там начинается массив слотов.
        // Регион замаплен RW на всё время жизни `SpscRing` (контракт
        // вызывающего), доступ к нему далее идёт только через атомик-указатели.
        let slots = unsafe { head.add(2) };

        Some(Self {
            head: head.cast_const(),
            tail: tail.cast_const(),
            slots: slots.cast_const(),
            slot_stride,
            slot_payload,
            capacity,
        })
    }

    /// Число слотов кольца (степень двойки).
    #[must_use]
    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Полезная ёмкость payload одного слота.
    #[must_use]
    #[inline]
    pub fn slot_payload(&self) -> usize {
        self.slot_payload
    }

    /// Пусто ли кольцо в текущий момент.
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.head_ref().load(Ordering::Acquire) == self.tail_ref().load(Ordering::Acquire)
    }

    /// Пусто ли кольцо для потребителя, готового уснуть: барьер перед чтением
    /// парен барьеру производителя в [`try_push`](Self::try_push).
    #[must_use]
    #[inline]
    pub fn is_empty_before_park(&self) -> bool {
        fence(Ordering::SeqCst);
        self.is_empty()
    }

    /// Полно ли кольцо в текущий момент.
    #[must_use]
    #[inline]
    pub fn is_full(&self) -> bool {
        let head = self.head_ref().load(Ordering::Acquire);
        let tail = self.tail_ref().load(Ordering::Acquire);
        head.wrapping_sub(tail) as usize == self.capacity
    }

    /// Вставляет кадр в кольцо. Вызывается **только производителем**.
    ///
    /// # Ошибки
    ///
    /// - [`RingError::TooLarge`] - `frame.len() > slot_payload`;
    /// - [`RingError::Full`] - нет свободного слота.
    pub fn try_push(&self, frame: &[u8]) -> Result<PushOutcome, RingError> {
        self.push_interleaved(frame, || {})
    }

    fn push_interleaved(
        &self,
        frame: &[u8],
        between: impl FnOnce(),
    ) -> Result<PushOutcome, RingError> {
        if frame.len() > self.slot_payload {
            return Err(RingError::TooLarge);
        }

        // `head` пишет только производитель, поэтому Relaxed-чтение собственного
        // индекса корректно. `tail` публикуется потребителем через Release -
        // наблюдаем через Acquire, чтобы увидеть освобождённые слоты.
        let head = self.head_ref().load(Ordering::Relaxed);
        let tail = self.tail_ref().load(Ordering::Acquire);
        if head.wrapping_sub(tail) as usize == self.capacity {
            return Err(RingError::Full);
        }

        let slot = self.slot_index(head);
        let len_field = self.slot_len_ref(slot);
        let payload = self.slot_payload_ref(slot);

        // Запись payload и len - Relaxed: их видимость потребителю обеспечит
        // последующий Release-store `head` (публикация). До публикации слот
        // потребителю недоступен.
        for (cell, &byte) in payload.iter().zip(frame.iter()) {
            cell.store(byte, Ordering::Relaxed);
        }
        // frame.len() <= slot_payload, а slot_payload вычисляется из region_len /
        // slot_stride, что не превышает u32::MAX на практике; кадр длиннее диапазона
        // не публикуется вовсе.
        let Ok(len) = u32::try_from(frame.len()) else {
            return Err(RingError::TooLarge);
        };
        len_field.store(len, Ordering::Relaxed);

        between();

        // Release-публикация индекса создаёт happens-before между записями выше и
        // Acquire-чтением `head` потребителем, поэтому тот гарантированно увидит
        // payload и len этого кадра.
        self.head_ref()
            .store(head.wrapping_add(1), Ordering::Release);

        // Предикат пробуждения считается по перечитанному `tail` уже после
        // публикации и отделён от неё SeqCst-барьером, парным барьеру
        // потребителя в `is_empty_before_park`.
        fence(Ordering::SeqCst);
        let wake_consumer = self.tail_ref().load(Ordering::Acquire) == head;

        Ok(PushOutcome { wake_consumer })
    }

    /// Извлекает кадр из кольца в `out`. Вызывается **только потребителем**.
    ///
    /// # Ошибки
    ///
    /// - [`RingError::Empty`] - кольцо пусто;
    /// - [`RingError::TooLarge`] - `out` короче извлекаемого кадра.
    pub fn try_pop(&self, out: &mut [u8]) -> Result<PopOutcome, RingError> {
        // `head` публикуется производителем через Release - наблюдаем через
        // Acquire, чтобы увидеть опубликованные payload/len. `tail` пишет только
        // потребитель - Relaxed-чтение собственного индекса корректно.
        let head = self.head_ref().load(Ordering::Acquire);
        let tail = self.tail_ref().load(Ordering::Relaxed);
        if head == tail {
            return Err(RingError::Empty);
        }

        let slot = self.slot_index(tail);
        let len_field = self.slot_len_ref(slot);
        let payload = self.slot_payload_ref(slot);

        // `len` пишет производитель (возможно, в другом процессе) - не доверяем
        // ему и клампим к фактическому размеру payload-слота, чтобы возвращаемая
        // длина не превышала число реально скопированных байт.
        let len = (len_field.load(Ordering::Relaxed) as usize).min(payload.len());
        if len > out.len() {
            return Err(RingError::TooLarge);
        }

        // Чтение payload - Relaxed: happens-before уже установлен Acquire-чтением
        // `head` выше, которое наблюдало Release-публикацию производителя.
        for (dst, cell) in out.iter_mut().zip(payload.iter()).take(len) {
            *dst = cell.load(Ordering::Relaxed);
        }

        // Release-store `tail`: публикует освобождение слота производителю.
        // Гарантирует, что производитель не перезапишет слот, пока мы не дочитали
        // его (его Acquire-чтение `tail` увидит это значение).
        self.tail_ref()
            .store(tail.wrapping_add(1), Ordering::Release);

        Ok(PopOutcome { len })
    }

    #[inline]
    fn head_ref(&self) -> &AtomicU32 {
        // SAFETY: `self.head` сформирован в `from_raw` из валидного выровненного
        // региона, замапленного RW на время жизни `SpscRing`; указатель не null
        // и указывает на корректно выровненный AtomicU32 в пределах заголовка.
        unsafe { &*self.head }
    }

    #[inline]
    fn tail_ref(&self) -> &AtomicU32 {
        // SAFETY: см. `head_ref`; `self.tail` - соседний AtomicU32 в заголовке.
        unsafe { &*self.tail }
    }

    #[inline]
    fn slot_index(&self, idx: u32) -> usize {
        // capacity - степень двойки, поэтому маска корректна и на границе
        // переполнения 32-битного счётчика.
        (idx as usize) & (self.capacity - 1)
    }

    #[inline]
    fn slot_len_ref(&self, slot: usize) -> &AtomicU32 {
        // SAFETY: slot < capacity, поэтому смещение `slot * slot_stride` от
        // начала массива слотов лежит в пределах региона. slot_stride кратен
        // size_of::<AtomicU32>(), а массив слотов выровнен под AtomicU32
        // (проверка в `from_raw`) - значит поле len каждого слота тоже выровнено.
        let ptr = unsafe { self.slots.byte_add(slot * self.slot_stride) };
        // SAFETY: `ptr` валиден, выровнен под AtomicU32 и указывает в регион,
        // замапленный RW на время жизни `SpscRing`.
        unsafe { &*ptr }
    }

    #[inline]
    fn slot_payload_ref(&self, slot: usize) -> &[AtomicU8] {
        // SAFETY: payload идёт сразу за полем len слота; диапазон
        // `[base_слота + size_of::<AtomicU32>(), .. + slot_payload)` лежит в
        // пределах региона (slot_stride >= size_of::<AtomicU32>() + slot_payload,
        // а сам слот целиком влезает в регион).
        let ptr = unsafe {
            self.slots
                .byte_add(slot * self.slot_stride + size_of::<AtomicU32>())
        };
        // SAFETY: `ptr` указывает на `slot_payload` последовательных байт региона,
        // замапленного RW на время жизни `SpscRing`; AtomicU8 имеет выравнивание 1.
        unsafe { core::slice::from_raw_parts(ptr.cast::<AtomicU8>(), self.slot_payload) }
    }
}

#[inline]
const fn prev_power_of_two(n: usize) -> usize {
    if n == 0 {
        0
    } else {
        // Старший установленный бит.
        1usize << (usize::BITS - 1 - n.leading_zeros())
    }
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
    use std::{boxed::Box, sync::Arc, thread, vec::Vec};

    use super::*;

    const REGION_BYTES: usize = 8 * 1024;

    #[repr(C, align(4096))]
    struct Region {
        bytes: [u8; REGION_BYTES],
    }

    impl Region {
        fn new() -> Box<Region> {
            Box::new(Region {
                bytes: [0u8; REGION_BYTES],
            })
        }
    }

    struct OwnedRing {
        ring: SpscRing,
        _region: Box<Region>,
    }

    impl OwnedRing {
        fn new(region_len: usize, slot_payload: usize) -> Self {
            assert!(region_len <= REGION_BYTES, "region must hold the ring");
            let mut region = Region::new();
            let base = region.bytes.as_mut_ptr();
            // SAFETY: регион принадлежит значению и освобождается после кольца,
            // `Box` держит его по одному адресу, а страничное выравнивание
            // удовлетворяет AtomicU32. Регион оборачивается только здесь, роли
            // producer и consumer распределяет тест.
            let ring = unsafe { SpscRing::from_raw(base, region_len, slot_payload) }
                .expect("ring must construct");
            Self {
                ring,
                _region: region,
            }
        }
    }

    impl core::ops::Deref for OwnedRing {
        type Target = SpscRing;

        fn deref(&self) -> &SpscRing {
            &self.ring
        }
    }

    #[test]
    fn from_raw_rejects_too_small_region() {
        let mut region = Region::new();
        let base = region.bytes.as_mut_ptr();
        // Заголовок есть, но ни одного слота.
        let len = SpscRing::header_bytes();
        // SAFETY: регион остаётся выделенным до конца теста, а кольцо над ним не создаётся.
        assert!(unsafe { SpscRing::from_raw(base, len, 16) }.is_none());
        // Меньше заголовка.
        // SAFETY: регион остаётся выделенным до конца теста, а кольцо над ним не создаётся.
        assert!(unsafe { SpscRing::from_raw(base, 1, 16) }.is_none());
    }

    #[test]
    fn from_raw_rejects_misaligned_base() {
        let mut region = Region::new();
        // Сдвигаем базу на 1 байт - нарушаем выравнивание под AtomicU32.
        // SAFETY: смещение на один байт остаётся в пределах региона.
        let base = unsafe { region.bytes.as_mut_ptr().add(1) };
        // SAFETY: регион остаётся выделенным до конца теста, а кольцо над ним не создаётся.
        assert!(unsafe { SpscRing::from_raw(base, 4096, 16) }.is_none());
    }

    #[test]
    fn capacity_is_floored_power_of_two() {
        let slot_payload = 16usize;
        let stride = SpscRing::slot_bytes(slot_payload);
        // Дадим место ровно под 5 слотов -> capacity должна стать 4.
        let len = SpscRing::header_bytes() + 5 * stride;
        let ring = OwnedRing::new(len, slot_payload);
        assert_eq!(ring.capacity(), 4);
        assert_eq!(ring.slot_payload(), slot_payload);
    }

    #[test]
    fn unaligned_payload_keeps_slot_len_aligned() {
        // slot_payload не кратен align_of::<AtomicU32>(), поэтому страйд должен
        // допаддиться, иначе поле len слота 1+ окажется невыровненным и доступ
        // к нему как к AtomicU32 паникует в debug. Прогоняем через слоты 1..3.
        let ring = OwnedRing::new(4096, 1);
        for i in 0..16u8 {
            ring.try_push(&[i]).expect("push");
            let mut buf = [0u8; 1];
            assert_eq!(ring.try_pop(&mut buf).expect("pop").len, 1);
            assert_eq!(buf[0], i);
        }
    }

    #[test]
    fn push_pop_round_trip_preserves_content_and_len() {
        let ring = OwnedRing::new(4096, 32);
        assert!(ring.is_empty());

        let frame = b"hello spsc ring";
        let out = ring.try_push(frame).expect("push");
        assert!(out.wake_consumer);
        assert!(!ring.is_empty());

        let mut buf = [0u8; 64];
        let popped = ring.try_pop(&mut buf).expect("pop");
        assert_eq!(popped.len, frame.len());
        assert_eq!(&buf[..popped.len], frame);
        assert!(ring.is_empty());
    }

    #[test]
    fn empty_frame_round_trips() {
        let ring = OwnedRing::new(4096, 8);
        ring.try_push(&[]).expect("push empty");
        let mut buf = [0xAAu8; 4];
        let popped = ring.try_pop(&mut buf).expect("pop empty");
        assert_eq!(popped.len, 0);
    }

    #[test]
    fn too_large_frame_is_rejected() {
        let ring = OwnedRing::new(4096, 8);
        let frame = [0u8; 9];
        assert_eq!(ring.try_push(&frame), Err(RingError::TooLarge));
        assert!(ring.is_empty());
    }

    #[test]
    fn pop_into_short_buffer_is_rejected() {
        let ring = OwnedRing::new(4096, 16);
        ring.try_push(&[1, 2, 3, 4, 5]).expect("push");
        let mut buf = [0u8; 4];
        assert_eq!(ring.try_pop(&mut buf), Err(RingError::TooLarge));
        // Кадр остался в кольце.
        let mut big = [0u8; 8];
        assert_eq!(ring.try_pop(&mut big).expect("pop").len, 5);
    }

    #[test]
    fn pop_on_empty_is_rejected() {
        let ring = OwnedRing::new(4096, 16);
        let mut buf = [0u8; 16];
        assert_eq!(ring.try_pop(&mut buf), Err(RingError::Empty));
    }

    #[test]
    fn push_on_full_is_rejected() {
        let slot_payload = 8usize;
        let stride = SpscRing::slot_bytes(slot_payload);
        // Ровно 4 слота.
        let len = SpscRing::header_bytes() + 4 * stride;
        let ring = OwnedRing::new(len, slot_payload);
        assert_eq!(ring.capacity(), 4);

        for i in 0..4u8 {
            assert!(ring.try_push(&[i]).is_ok());
        }
        assert!(ring.is_full());
        assert_eq!(ring.try_push(&[42]), Err(RingError::Full));
    }

    #[test]
    fn wraparound_over_many_elements() {
        let slot_payload = 8usize;
        let stride = SpscRing::slot_bytes(slot_payload);
        let len = SpscRing::header_bytes() + 4 * stride;
        let ring = OwnedRing::new(len, slot_payload);
        assert_eq!(ring.capacity(), 4);

        // Прогоняем заметно больше capacity элементов попеременно.
        let total = 4u32 * 1000;
        for i in 0..total {
            let value = i.to_le_bytes();
            ring.try_push(&value).expect("push");
            let mut buf = [0u8; 4];
            let popped = ring.try_pop(&mut buf).expect("pop");
            assert_eq!(popped.len, 4);
            assert_eq!(u32::from_le_bytes(buf), i);
        }
        assert!(ring.is_empty());
    }

    #[test]
    fn wraparound_with_partial_fills() {
        let slot_payload = 4usize;
        let stride = SpscRing::slot_bytes(slot_payload);
        let len = SpscRing::header_bytes() + 4 * stride;
        let ring = OwnedRing::new(len, slot_payload);

        let mut next_push = 0u32;
        let mut next_pop = 0u32;
        // Чередуем "налить 3 - вычерпать 2", много раз, чтобы индексы
        // проворачивались относительно границы слотов.
        for _ in 0..2000 {
            for _ in 0..3 {
                if ring.try_push(&next_push.to_le_bytes()).is_ok() {
                    next_push += 1;
                }
            }
            for _ in 0..2 {
                let mut buf = [0u8; 4];
                if ring.try_pop(&mut buf).is_ok() {
                    assert_eq!(u32::from_le_bytes(buf), next_pop);
                    next_pop += 1;
                }
            }
        }
        // Досушиваем остаток.
        let mut buf = [0u8; 4];
        while ring.try_pop(&mut buf).is_ok() {
            assert_eq!(u32::from_le_bytes(buf), next_pop);
            next_pop += 1;
        }
        assert_eq!(next_pop, next_push);
        assert!(ring.is_empty());
    }

    #[test]
    fn wake_consumer_flag_only_on_push_into_drained_ring() {
        let ring = OwnedRing::new(4096, 8);

        assert!(ring.try_push(&[1]).unwrap().wake_consumer);
        assert!(!ring.try_push(&[2]).unwrap().wake_consumer);
        assert!(!ring.try_push(&[3]).unwrap().wake_consumer);

        let mut buf = [0u8; 8];
        ring.try_pop(&mut buf).unwrap();
        ring.try_pop(&mut buf).unwrap();
        ring.try_pop(&mut buf).unwrap();
        assert!(ring.is_empty());

        // Снова в пустое кольцо - флаг опять истинен.
        assert!(ring.try_push(&[4]).unwrap().wake_consumer);
    }

    #[test]
    fn ring_drained_during_publication_still_requests_wake() {
        let ring = OwnedRing::new(4096, 8);

        ring.try_push(&[1]).expect("first push");

        // Роль потребителя отыгрывается тем же потоком внутри окна. Снимок
        // индексов уже взят, кадр ещё не опубликован, и кольцо пустеет ровно
        // здесь. Предикат обязан учесть это, иначе потребитель уснёт навсегда.
        let outcome = ring
            .push_interleaved(&[2], || {
                let mut buf = [0u8; 8];
                ring.try_pop(&mut buf).expect("drain inside the window");
                assert!(ring.is_empty_before_park());
            })
            .expect("second push");

        assert!(outcome.wake_consumer);
    }

    #[test]
    fn ring_kept_full_during_publication_does_not_request_wake() {
        let ring = OwnedRing::new(4096, 8);

        ring.try_push(&[1]).expect("first push");

        let outcome = ring.push_interleaved(&[2], || {}).expect("second push");

        assert!(!outcome.wake_consumer);
    }

    #[test]
    fn race_producer_and_consumer_preserve_order_and_completeness() {
        const N: u32 = 100_000;

        let slot_payload = 4usize;
        let stride = SpscRing::slot_bytes(slot_payload);
        // Небольшое кольцо - намеренно заставляем producer/consumer бороться.
        let len = SpscRing::header_bytes() + 16 * stride;

        // Producer и consumer делят одно кольцо: `Arc` держит регион, пока не
        // завершится последний из потоков.
        let producer = Arc::new(OwnedRing::new(len, slot_payload));
        let consumer = Arc::clone(&producer);

        let producer_handle = thread::spawn(move || {
            let mut i = 0u32;
            while i < N {
                if producer.try_push(&i.to_le_bytes()).is_ok() {
                    i += 1;
                } else {
                    std::thread::yield_now();
                }
            }
        });

        let consumer_handle = thread::spawn(move || {
            let mut received: Vec<u32> = Vec::with_capacity(N as usize);
            let mut buf = [0u8; 4];
            while (received.len() as u32) < N {
                match consumer.try_pop(&mut buf) {
                    Ok(p) => {
                        assert_eq!(p.len, 4);
                        received.push(u32::from_le_bytes(buf));
                    }
                    Err(RingError::Empty) => std::thread::yield_now(),
                    Err(e) => panic!("unexpected error: {e:?}"),
                }
            }
            received
        });

        producer_handle.join().expect("producer joined");
        let received = consumer_handle.join().expect("consumer joined");

        // Все значения получены, в порядке, без потерь и дублей.
        assert_eq!(received.len(), N as usize);
        let expected: Vec<u32> = (0..N).collect();
        assert_eq!(received, expected);
    }

    struct StickySignal {
        signaled: std::sync::Mutex<bool>,
        raised: std::sync::Condvar,
    }

    impl StickySignal {
        fn new() -> Self {
            Self {
                signaled: std::sync::Mutex::new(false),
                raised: std::sync::Condvar::new(),
            }
        }

        fn set(&self) {
            *self.signaled.lock().expect("signal") = true;
            self.raised.notify_all();
        }

        fn clear(&self) {
            *self.signaled.lock().expect("signal") = false;
        }

        fn wait(&self, budget: std::time::Duration) -> bool {
            let mut state = self.signaled.lock().expect("signal");
            while !*state {
                let (next, outcome) = self.raised.wait_timeout(state, budget).expect("signal");
                state = next;
                if outcome.timed_out() && !*state {
                    return false;
                }
            }
            true
        }
    }

    #[test]
    fn race_park_protocol_never_loses_a_wakeup() {
        const N: u32 = 50_000;
        const FRAME_BYTES: usize = 256;
        const WAKE_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

        let stride = SpscRing::slot_bytes(FRAME_BYTES);
        // При двух слотах кольцо вычерпывается досуха на каждом круге.
        let len = SpscRing::header_bytes() + 2 * stride;

        let ring = Arc::new(OwnedRing::new(len, FRAME_BYTES));
        let producer = Arc::clone(&ring);
        let consumer = Arc::clone(&ring);
        let producer_signal = Arc::new(StickySignal::new());
        let consumer_signal = Arc::clone(&producer_signal);

        let producer_handle = thread::spawn(move || {
            let mut frame = [0u8; FRAME_BYTES];
            let mut i = 0u32;
            while i < N {
                frame[..4].copy_from_slice(&i.to_le_bytes());
                match producer.try_push(&frame) {
                    Ok(outcome) => {
                        if outcome.wake_consumer {
                            producer_signal.set();
                        }
                        i += 1;
                    }
                    Err(RingError::Full) => std::thread::yield_now(),
                    Err(e) => panic!("unexpected error: {e:?}"),
                }
            }
        });

        let consumer_handle = thread::spawn(move || {
            let mut buf = [0u8; FRAME_BYTES];
            let mut next = 0u32;
            while next < N {
                // Тот же протокол, что у RingConsumer::pop.
                loop {
                    if !consumer.is_empty() {
                        break;
                    }
                    consumer_signal.clear();
                    if !consumer.is_empty_before_park() {
                        break;
                    }
                    // Исчерпание бюджета означает потерянное пробуждение. Дольше уступки
                    // планировщику производитель не блокируется.
                    if !consumer_signal.wait(WAKE_BUDGET) {
                        return Err(next);
                    }
                }
                match consumer.try_pop(&mut buf) {
                    Ok(popped) => {
                        assert_eq!(popped.len, FRAME_BYTES);
                        assert_eq!(u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]), next);
                        next += 1;
                    }
                    Err(RingError::Empty) => std::thread::yield_now(),
                    Err(e) => panic!("unexpected error: {e:?}"),
                }
            }
            Ok(())
        });

        let outcome = consumer_handle.join();

        // Потеря пробуждения оставляет производителя на полном кольце:
        // дочерпываем за ушедшего потребителя, иначе отказ был бы зависанием.
        let mut drain = [0u8; FRAME_BYTES];
        while !producer_handle.is_finished() {
            let _ = ring.try_pop(&mut drain);
            std::thread::yield_now();
        }
        producer_handle.join().expect("producer joined");

        assert_eq!(outcome.expect("consumer joined"), Ok(()));
    }

    #[test]
    fn region_bytes_matches_layout() {
        assert_eq!(SpscRing::header_bytes(), 8);
        assert_eq!(SpscRing::slot_bytes(16), 4 + 16);
        assert_eq!(SpscRing::region_bytes(4, 16), 8 + 4 * 20);
        // Регион ровно нужного размера конструируется и даёт ровно эту capacity.
        let bytes = SpscRing::region_bytes(8, 12);
        let ring = OwnedRing::new(bytes, 12);
        assert_eq!(ring.capacity(), 8);
    }

    #[test]
    fn header_and_region_bytes_are_const() {
        const _H: usize = SpscRing::header_bytes();
        const _R: usize = SpscRing::region_bytes(4, 16);
    }

    #[test]
    fn prev_power_of_two_basic() {
        assert_eq!(prev_power_of_two(0), 0);
        assert_eq!(prev_power_of_two(1), 1);
        assert_eq!(prev_power_of_two(2), 2);
        assert_eq!(prev_power_of_two(3), 2);
        assert_eq!(prev_power_of_two(5), 4);
        assert_eq!(prev_power_of_two(8), 8);
        assert_eq!(prev_power_of_two(9), 8);
        assert_eq!(prev_power_of_two(1023), 512);
    }
}
