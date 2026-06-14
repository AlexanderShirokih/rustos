use core::cmp::Ordering;

use crate::vec::Vec;

extern crate alloc;

use alloc::vec::Vec as AllocVec;

/// Полуоткрытый интервал [start, end).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interval<T> {
    /// Начало интервала (включительно).
    pub start: T,

    /// Конец интервала (исключительно).
    pub end: T,
}

impl<T: Ord + Copy> Interval<T> {
    #[inline]
    pub fn new(start: T, end: T) -> Option<Self> {
        if start < end {
            Some(Self { start, end })
        } else {
            None
        }
    }

    /// Проверяет пересечение или смежность с другим интервалом.
    #[inline]
    fn overlaps_or_touches(&self, other: &Self) -> bool {
        !(self.end < other.start || other.end < self.start)
    }

    /// Проверяет пересечение с другим интервалом.
    #[inline]
    fn overlaps(&self, other: &Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    /// Расширяет интервал, объединяя с другим.
    #[inline]
    fn merge_with(&mut self, other: &Self) {
        if other.start < self.start {
            self.start = other.start;
        }
        if other.end > self.end {
            self.end = other.end;
        }
    }
}

/// Внутренний трейт хранения интервалов.
trait IntervalStorage<T>: Sized {
    type Iter<'a>: Iterator<Item = &'a T>
    where
        Self: 'a,
        T: 'a;

    fn new() -> Self;
    fn len(&self) -> usize;
    fn get(&self, index: usize) -> Option<&T>;
    fn push(&mut self, value: T) -> Option<()>;
    fn iter(&self) -> Self::Iter<'_>;
}

impl<T, const N: usize> IntervalStorage<T> for Vec<T, N> {
    type Iter<'a>
        = crate::vec::VecIter<'a, T>
    where
        T: 'a;

    fn new() -> Self {
        Vec::new()
    }

    fn len(&self) -> usize {
        Vec::len(self)
    }

    fn get(&self, index: usize) -> Option<&T> {
        Vec::get(self, index)
    }

    fn push(&mut self, value: T) -> Option<()> {
        Vec::push(self, value)
    }

    fn iter(&self) -> Self::Iter<'_> {
        Vec::iter(self)
    }
}

impl<T> IntervalStorage<T> for AllocVec<T> {
    type Iter<'a>
        = core::slice::Iter<'a, T>
    where
        T: 'a;

    fn new() -> Self {
        AllocVec::new()
    }

    fn len(&self) -> usize {
        AllocVec::len(self)
    }

    fn get(&self, index: usize) -> Option<&T> {
        self.as_slice().get(index)
    }

    fn push(&mut self, value: T) -> Option<()> {
        AllocVec::push(self, value);
        Some(())
    }

    fn iter(&self) -> Self::Iter<'_> {
        self.as_slice().iter()
    }
}

/// Множество непересекающихся полуоткрытых интервалов.
///
/// Интервалы хранятся отсортированными. При добавлении смежные
/// и пересекающиеся интервалы автоматически объединяются.
///
/// `N` - максимальное количество интервалов.
#[derive(Clone, PartialEq, Eq)]
pub struct StaticIntervalSet<T, const N: usize> {
    ranges: Vec<Interval<T>, N>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct IntervalSet<T> {
    ranges: AllocVec<Interval<T>>,
}

impl<T, const N: usize> StaticIntervalSet<T, N> {
    pub const fn new() -> Self {
        Self { ranges: Vec::new() }
    }
}

impl<T> IntervalSet<T> {
    pub fn new() -> Self {
        Self {
            ranges: AllocVec::new(),
        }
    }
}

impl<T, const N: usize> Default for StaticIntervalSet<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Default for IntervalSet<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Ord + Copy, const N: usize> StaticIntervalSet<T, N> {
    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// Возвращает интервал по индексу.
    pub fn get(&self, index: usize) -> Option<&Interval<T>> {
        self.ranges.get(index)
    }

    /// Итератор по интервалам.
    pub fn iter(&self) -> impl Iterator<Item = &Interval<T>> {
        self.ranges.iter()
    }

    /// Добавляет интервал [start, end).
    ///
    /// Возвращает `None` при нехватке ёмкости.
    pub fn add(&mut self, start: T, end: T) -> Option<()> {
        add_impl(&mut self.ranges, start, end)
    }

    /// Удаляет интервал [start, end).
    ///
    /// Возвращает `None` при нехватке ёмкости (разрез может увеличить число интервалов).
    pub fn remove(&mut self, start: T, end: T) -> Option<()> {
        remove_impl(&mut self.ranges, start, end)
    }
}

impl<T: Ord + Copy> IntervalSet<T> {
    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// Возвращает интервал по индексу.
    pub fn get(&self, index: usize) -> Option<&Interval<T>> {
        self.ranges.get(index)
    }

    /// Итератор по интервалам.
    pub fn iter(&self) -> impl Iterator<Item = &Interval<T>> {
        self.ranges.iter()
    }

    /// Добавляет интервал [start, end).
    pub fn add(&mut self, start: T, end: T) -> Option<()> {
        add_impl(&mut self.ranges, start, end)
    }

    /// Удаляет интервал [start, end).
    pub fn remove(&mut self, start: T, end: T) -> Option<()> {
        remove_impl(&mut self.ranges, start, end)
    }
}

fn binary_search_first_ge<T, S, F>(ranges: &S, mut predicate: F) -> usize
where
    S: IntervalStorage<Interval<T>>,
    F: FnMut(&Interval<T>) -> Ordering,
{
    let mut lo = 0;
    let mut hi = ranges.len();

    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let item = ranges.get(mid).expect("index in bounds");

        match predicate(item) {
            Ordering::Less => lo = mid + 1,
            Ordering::Equal | Ordering::Greater => hi = mid,
        }
    }

    lo
}

fn add_impl<T, S>(ranges: &mut S, start: T, end: T) -> Option<()>
where
    T: Ord + Copy,
    S: IntervalStorage<Interval<T>>,
{
    let Some(mut incoming) = Interval::new(start, end) else {
        return Some(());
    };

    // Поиск первого диапазона, который может пересекаться или касаться: range.end >= incoming.start
    let mut i = binary_search_first_ge(ranges, |r| {
        if r.end < incoming.start {
            Ordering::Less
        } else {
            Ordering::Greater
        }
    });

    // Проверка необходимости смещения на шаг влево при касании слева
    if i > 0 {
        let prev = ranges.get(i - 1).expect("index in bounds");
        if prev.end >= incoming.start {
            i -= 1;
        }
    }

    // Сборка результата в новый вектор
    let mut out = S::new();

    // Копирование интервалов до i
    for idx in 0..i {
        let item = *ranges.get(idx).expect("index in bounds");
        out.push(item)?;
    }

    // Слияние всех пересекающихся/касающихся диапазонов в incoming
    while i < ranges.len() {
        let item = *ranges.get(i).expect("index in bounds");
        if item.overlaps_or_touches(&incoming) {
            incoming.merge_with(&item);
            i += 1;
        } else {
            break;
        }
    }

    out.push(incoming)?;

    // Копирование оставшихся интервалов
    while i < ranges.len() {
        let item = *ranges.get(i).expect("index in bounds");
        out.push(item)?;
        i += 1;
    }

    *ranges = out;
    Some(())
}

fn remove_impl<T, S>(ranges: &mut S, start: T, end: T) -> Option<()>
where
    T: Ord + Copy,
    S: IntervalStorage<Interval<T>>,
{
    let Some(cut) = Interval::new(start, end) else {
        return Some(());
    };

    let mut out = S::new();

    for r in ranges.iter().copied() {
        if !r.overlaps(&cut) {
            out.push(r)?;
            continue;
        }

        // Левый остаток: [r.start, cut.start)
        if r.start < cut.start
            && let Some(left) = Interval::new(r.start, cut.start)
        {
            out.push(left)?;
        }

        // Правый остаток: [cut.end, r.end)
        if cut.end < r.end
            && let Some(right) = Interval::new(cut.end, r.end)
        {
            out.push(right)?;
        }
    }

    *ranges = out;
    Some(())
}

impl<A> FromIterator<Interval<A>> for IntervalSet<A> {
    fn from_iter<T: IntoIterator<Item = Interval<A>>>(iter: T) -> Self {
        Self {
            ranges: AllocVec::from_iter(iter),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_add_remove() {
        let mut s = StaticIntervalSet::<i32, 16>::new();
        s.add(1, 10).unwrap();
        s.remove(3, 7).unwrap();

        let expected = [
            Interval { start: 1, end: 3 },
            Interval { start: 7, end: 10 },
        ];
        assert_eq!(s.len(), 2);
        assert_eq!(*s.get(0).unwrap(), expected[0]);
        assert_eq!(*s.get(1).unwrap(), expected[1]);
    }

    #[test]
    fn add_merges_adjacent() {
        let mut s = StaticIntervalSet::<i32, 16>::new();
        s.add(1, 3).unwrap();
        s.add(3, 7).unwrap();
        s.add(7, 10).unwrap();

        assert_eq!(s.len(), 1);
        assert_eq!(*s.get(0).unwrap(), Interval { start: 1, end: 10 });
    }

    #[test]
    fn remove_middle_splits() {
        let mut s = StaticIntervalSet::<i32, 16>::new();
        s.add(0, 10).unwrap();
        s.remove(4, 6).unwrap();

        let expected = [
            Interval { start: 0, end: 4 },
            Interval { start: 6, end: 10 },
        ];
        assert_eq!(s.len(), 2);
        assert_eq!(*s.get(0).unwrap(), expected[0]);
        assert_eq!(*s.get(1).unwrap(), expected[1]);
    }

    #[test]
    fn remove_full_clears() {
        let mut s = StaticIntervalSet::<i32, 16>::new();
        s.add(0, 10).unwrap();
        s.remove(0, 10).unwrap();
        assert!(s.is_empty());
    }

    #[test]
    fn reservations_subtract_overlapping_and_out_of_set() {
        let mut s = StaticIntervalSet::<usize, 16>::new();
        s.add(0x1000, 0x9000).unwrap();
        s.remove(0x2000, 0x4000).unwrap();
        s.remove(0x9000, 0xA000).unwrap();
        s.remove(0x2_0000, 0x2_1000).unwrap();

        let expected = [
            Interval {
                start: 0x1000,
                end: 0x2000,
            },
            Interval {
                start: 0x4000,
                end: 0x9000,
            },
        ];
        assert_eq!(s.len(), 2);
        assert_eq!(*s.get(0).unwrap(), expected[0]);
        assert_eq!(*s.get(1).unwrap(), expected[1]);
    }

    #[test]
    fn capacity_overflow() {
        let mut s = StaticIntervalSet::<i32, 2>::new();
        assert!(s.add(0, 10).is_some());
        assert!(s.add(20, 30).is_some());
        // Третий интервал не поместится
        assert!(s.add(40, 50).is_none());
    }

    #[test]
    fn dyn_add_remove() {
        let mut s = IntervalSet::<i32>::new();
        s.add(5, 12).unwrap();
        s.remove(7, 9).unwrap();

        let expected = [
            Interval { start: 5, end: 7 },
            Interval { start: 9, end: 12 },
        ];
        assert_eq!(s.len(), 2);
        assert_eq!(*s.get(0).unwrap(), expected[0]);
        assert_eq!(*s.get(1).unwrap(), expected[1]);
    }

    #[test]
    fn dyn_merges_adjacent() {
        let mut s = IntervalSet::<i32>::new();
        s.add(1, 3).unwrap();
        s.add(3, 8).unwrap();
        s.add(8, 9).unwrap();

        assert_eq!(s.len(), 1);
        assert_eq!(*s.get(0).unwrap(), Interval { start: 1, end: 9 });
    }
}
