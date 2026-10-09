//! Последовательность элементов фиксированного размера ёмкостью `CAP` без кучи.

#[derive(Debug, Clone)]
pub(crate) struct Packed<const ITEM: usize, const CAP: usize> {
    items: [[u8; ITEM]; CAP],
    len: usize,
}

impl<const ITEM: usize, const CAP: usize> Packed<ITEM, CAP> {
    pub(crate) const fn new() -> Self {
        Self {
            items: [[0; ITEM]; CAP],
            len: 0,
        }
    }

    pub(crate) fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let mut packed = Self::new();
        packed.fill(bytes).then_some(packed)
    }

    pub(crate) fn fill(&mut self, bytes: &[u8]) -> bool {
        let (items, rest) = bytes.as_chunks();
        if !rest.is_empty() {
            return false;
        }

        let Some(slots) = self.items.get_mut(..items.len()) else {
            return false;
        };
        slots.copy_from_slice(items);
        self.len = items.len();
        true
    }

    pub(crate) fn push(&mut self, item: [u8; ITEM]) -> bool {
        let Some(slot) = self.items.get_mut(self.len) else {
            return false;
        };

        *slot = item;
        self.len += 1;
        true
    }

    pub(crate) const fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn clear(&mut self) {
        self.len = 0;
    }

    pub(crate) fn items(&self) -> &[[u8; ITEM]] {
        self.items.get(..self.len).unwrap_or_default()
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.items().as_flattened()
    }
}

impl<const ITEM: usize, const CAP: usize> PartialEq for Packed<ITEM, CAP> {
    fn eq(&self, other: &Self) -> bool {
        self.items() == other.items()
    }
}

impl<const ITEM: usize, const CAP: usize> Eq for Packed<ITEM, CAP> {}
