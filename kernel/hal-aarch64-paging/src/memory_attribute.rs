//! Таблица слотов MAIR_EL1 и их индексов в записях таблицы страниц.
//!
//! Индекс слота, его кодировка и порядок обхода объявлены здесь одним
//! перечислением, из которого выводятся поле `AttrIndx` leaf-записи и значение
//! регистра MAIR_EL1.

const SLOT_BITS: u32 = 8;

/// Слот атрибутов памяти MAIR_EL1.
///
/// Дискриминант варианта - индекс слота, попадающий в поле `AttrIndx`
/// записи таблицы страниц.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum MemoryAttribute {
    /// Normal, Inner/Outer Write-Back, Read/Write-Allocate.
    Cached = 0,
    /// Device-nGnRE.
    Device = 1,
    /// Normal, Inner/Outer Non-Cacheable.
    WriteCombining = 2,
}

impl MemoryAttribute {
    const FIRST: Self = MemoryAttribute::Cached;

    /// Индекс слота в MAIR_EL1 и в поле `AttrIndx` записи таблицы страниц.
    pub const fn index(self) -> u8 {
        self as u8
    }

    /// Кодировка слота - байт `Attr<n>` регистра MAIR_EL1.
    pub const fn encoding(self) -> u8 {
        match self {
            MemoryAttribute::Cached => 0xFF,
            MemoryAttribute::Device => 0x04,
            MemoryAttribute::WriteCombining => 0x44,
        }
    }

    const fn next_slot(self) -> Option<Self> {
        match self {
            MemoryAttribute::Cached => Some(MemoryAttribute::Device),
            MemoryAttribute::Device => Some(MemoryAttribute::WriteCombining),
            MemoryAttribute::WriteCombining => None,
        }
    }

    /// Значение регистра MAIR_EL1, собранное из всех объявленных слотов.
    pub const fn mair_value() -> u64 {
        let mut value: u64 = 0;
        let mut slot = Self::FIRST;
        loop {
            value |= (slot.encoding() as u64) << (slot.index() as u32 * SLOT_BITS);
            match slot.next_slot() {
                Some(next) => slot = next,
                None => return value,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared_slots() -> impl Iterator<Item = MemoryAttribute> {
        core::iter::successors(Some(MemoryAttribute::FIRST), |slot| slot.next_slot())
    }

    #[test]
    fn slot_indices_are_dense_and_unique() {
        for (position, slot) in declared_slots().enumerate() {
            assert_eq!(slot.index() as usize, position);
        }
    }

    // Regression guard: индекс шире трёх бит не представим полем `AttrIndx`
    // и молча выбирает чужой слот.
    #[test]
    fn slot_indices_fit_the_attr_index_field() {
        for slot in declared_slots() {
            assert!(slot.index() < 8);
        }
    }

    #[test]
    fn mair_value_places_each_slot_at_its_index() {
        let value = MemoryAttribute::mair_value();
        for slot in declared_slots() {
            let byte = (value >> (u32::from(slot.index()) * SLOT_BITS)) & 0xFF;
            assert_eq!(byte as u8, slot.encoding());
        }
    }

    // Regression guard: незапрограммированный слот читается как 0x00 -
    // Device-nGnRnE, и опечатка в индексе не даёт ни отказа, ни исключения.
    #[test]
    fn declared_slots_are_never_zero() {
        for slot in declared_slots() {
            assert_ne!(slot.encoding(), 0x00);
        }
    }

    #[test]
    fn mair_value_matches_the_expected_encoding() {
        assert_eq!(MemoryAttribute::mair_value(), 0x0044_04FF);
    }
}
