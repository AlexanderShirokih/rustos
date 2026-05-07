//! Фиксированный bit-set на стеке.
//!
//! Ёмкость задаётся const-параметром `WORDS` (число `u64`-слов). Для расчёта
//! числа слов под нужное количество бит используйте [`bits_to_words`].

/// Возвращает количество `u64`-слов, достаточное для хранения `bits` бит.
#[must_use]
#[inline]
pub const fn bits_to_words(bits: usize) -> usize {
    bits.div_ceil(u64::BITS as usize)
}

/// Bit-set фиксированной ёмкости (`WORDS * 64` бит).
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct BitSet<const WORDS: usize> {
    words: [u64; WORDS],
}

impl<const WORDS: usize> BitSet<WORDS> {
    pub const CAPACITY: usize = WORDS * u64::BITS as usize;

    #[must_use]
    pub const fn new() -> Self {
        Self { words: [0; WORDS] }
    }

    #[must_use]
    #[inline]
    pub const fn capacity(&self) -> usize {
        Self::CAPACITY
    }

    #[inline]
    pub fn set(&mut self, idx: usize) {
        let (w, b) = Self::split(idx);
        self.words[w] |= 1u64 << b;
    }

    #[inline]
    pub fn clear(&mut self, idx: usize) {
        let (w, b) = Self::split(idx);
        self.words[w] &= !(1u64 << b);
    }

    #[must_use]
    #[inline]
    pub fn is_set(&self, idx: usize) -> bool {
        let (w, b) = Self::split(idx);
        (self.words[w] >> b) & 1 == 1
    }

    #[inline]
    pub fn clear_all(&mut self) {
        self.words = [0; WORDS];
    }

    #[inline]
    fn split(idx: usize) -> (usize, u32) {
        debug_assert!(idx < Self::CAPACITY, "BitSet index out of range");
        (idx / u64::BITS as usize, (idx % u64::BITS as usize) as u32)
    }
}

impl<const WORDS: usize> Default for BitSet<WORDS> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_to_words_rounds_up() {
        assert_eq!(bits_to_words(0), 0);
        assert_eq!(bits_to_words(1), 1);
        assert_eq!(bits_to_words(64), 1);
        assert_eq!(bits_to_words(65), 2);
        assert_eq!(bits_to_words(65536), 1024);
    }

    #[test]
    fn new_is_empty() {
        let bs: BitSet<4> = BitSet::new();
        for i in 0..bs.capacity() {
            assert!(!bs.is_set(i));
        }
    }

    #[test]
    fn set_and_clear_individual_bits() {
        let mut bs: BitSet<2> = BitSet::new();
        bs.set(0);
        bs.set(63);
        bs.set(64);
        bs.set(127);
        assert!(bs.is_set(0));
        assert!(bs.is_set(63));
        assert!(bs.is_set(64));
        assert!(bs.is_set(127));
        assert!(!bs.is_set(1));
        assert!(!bs.is_set(62));

        bs.clear(63);
        assert!(!bs.is_set(63));
        assert!(bs.is_set(64), "clearing 63 must not affect 64 (next word)");
    }

    #[test]
    fn clear_all_resets_every_bit() {
        let mut bs: BitSet<3> = BitSet::new();
        for i in 0..bs.capacity() {
            bs.set(i);
        }
        bs.clear_all();
        for i in 0..bs.capacity() {
            assert!(!bs.is_set(i));
        }
    }

    #[test]
    fn capacity_matches_words() {
        assert_eq!(BitSet::<1>::CAPACITY, 64);
        assert_eq!(BitSet::<1024>::CAPACITY, 65_536);
    }

    #[test]
    #[should_panic]
    fn out_of_range_set_panics_in_debug() {
        let mut bs: BitSet<1> = BitSet::new();
        bs.set(64);
    }
}
