use crate::vec::Vec;
use core::cmp::Ordering;

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

/// Множество непересекающихся полуоткрытых интервалов.
///
/// Интервалы хранятся отсортированными. При добавлении смежные
/// и пересекающиеся интервалы автоматически объединяются.
///
/// `N` — максимальное количество интервалов.
#[derive(Clone, PartialEq, Eq)]
pub struct IntervalSet<T, const N: usize> {
    /// Отсортированный список непересекающихся интервалов.
    ranges: Vec<Interval<T>, N>,
}

impl<T, const N: usize> IntervalSet<T, N> {
    pub const fn new() -> Self {
        Self { ranges: Vec::new() }
    }
}

impl<T, const N: usize> Default for IntervalSet<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Ord + Copy, const N: usize> IntervalSet<T, N> {
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

    /// Бинарный поиск первого интервала, удовлетворяющего предикату.
    fn binary_search_first_ge<F>(&self, mut predicate: F) -> usize
    where
        F: FnMut(&Interval<T>) -> Ordering,
    {
        let mut lo = 0;
        let mut hi = self.ranges.len();

        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match predicate(&self.ranges[mid]) {
                Ordering::Less => lo = mid + 1,
                Ordering::Equal | Ordering::Greater => hi = mid,
            }
        }

        lo
    }

    /// Добавляет интервал [start, end).
    ///
    /// Возвращает `None` при нехватке ёмкости.
    pub fn add(&mut self, start: T, end: T) -> Option<()> {
        let Some(mut incoming) = Interval::new(start, end) else {
            return Some(());
        };

        // Find first range that might overlap/touch: range.end >= incoming.start
        let mut i = self.binary_search_first_ge(|r| {
            if r.end < incoming.start {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        });

        // Maybe we need to look one step left if it touches from the left.
        if i > 0 && self.ranges[i - 1].end >= incoming.start {
            i -= 1;
        }

        // Собираем результат в новый вектор
        let mut out: Vec<Interval<T>, N> = Vec::new();

        // Копируем интервалы до i
        for idx in 0..i {
            out.push(self.ranges[idx])?;
        }

        // Merge all overlapping/touching ranges into incoming.
        while i < self.ranges.len() && self.ranges[i].overlaps_or_touches(&incoming) {
            incoming.merge_with(&self.ranges[i]);
            i += 1;
        }

        out.push(incoming)?;

        // Копируем оставшиеся интервалы
        while i < self.ranges.len() {
            out.push(self.ranges[i])?;
            i += 1;
        }

        self.ranges = out;
        Some(())
    }

    /// Удаляет интервал [start, end).
    ///
    /// Возвращает `None` при нехватке ёмкости (разрез может увеличить число интервалов).
    pub fn remove(&mut self, start: T, end: T) -> Option<()> {
        let Some(cut) = Interval::new(start, end) else {
            return Some(());
        };

        let mut out: Vec<Interval<T>, N> = Vec::new();

        for r in self.ranges.iter().copied() {
            if !r.overlaps(&cut) {
                out.push(r)?;
                continue;
            }

            // Left remainder: [r.start, cut.start)
            if r.start < cut.start
                && let Some(left) = Interval::new(r.start, cut.start)
            {
                out.push(left)?;
            }

            // Right remainder: [cut.end, r.end)
            if cut.end < r.end
                && let Some(right) = Interval::new(cut.end, r.end)
            {
                out.push(right)?;
            }
        }

        self.ranges = out;
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_add_remove() {
        let mut s = IntervalSet::<i32, 16>::new();
        s.add(1, 10).unwrap();
        s.remove(3, 7).unwrap();

        let expected = [Interval { start: 1, end: 3 }, Interval { start: 7, end: 10 }];
        assert_eq!(s.len(), 2);
        assert_eq!(*s.get(0).unwrap(), expected[0]);
        assert_eq!(*s.get(1).unwrap(), expected[1]);
    }

    #[test]
    fn add_merges_adjacent() {
        let mut s = IntervalSet::<i32, 16>::new();
        s.add(1, 3).unwrap();
        s.add(3, 7).unwrap();
        s.add(7, 10).unwrap();

        assert_eq!(s.len(), 1);
        assert_eq!(*s.get(0).unwrap(), Interval { start: 1, end: 10 });
    }

    #[test]
    fn remove_middle_splits() {
        let mut s = IntervalSet::<i32, 16>::new();
        s.add(0, 10).unwrap();
        s.remove(4, 6).unwrap();

        let expected = [Interval { start: 0, end: 4 }, Interval { start: 6, end: 10 }];
        assert_eq!(s.len(), 2);
        assert_eq!(*s.get(0).unwrap(), expected[0]);
        assert_eq!(*s.get(1).unwrap(), expected[1]);
    }

    #[test]
    fn remove_full_clears() {
        let mut s = IntervalSet::<i32, 16>::new();
        s.add(0, 10).unwrap();
        s.remove(0, 10).unwrap();
        assert!(s.is_empty());
    }

    #[test]
    fn capacity_overflow() {
        let mut s = IntervalSet::<i32, 2>::new();
        assert!(s.add(0, 10).is_some());
        assert!(s.add(20, 30).is_some());
        // Третий интервал не поместится
        assert!(s.add(40, 50).is_none());
    }
}