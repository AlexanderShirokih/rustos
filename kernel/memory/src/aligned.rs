/// Трейт адреса памяти: единая абстракция для `MemoryRange<A: Address>` и подобного
/// generic-кода над физическими и виртуальными адресами.
pub trait Address: Copy + Clone + PartialOrd {
    fn as_usize(self) -> usize;

    fn as_u64(self) -> u64 {
        self.as_usize() as u64
    }
}

/// Трейт типов с фиксированным выравниванием `ALIGNMENT`. Для адресных типов крейта то
/// же значение служит размером страницы/фрейма и шагом между соседними адресами.
pub trait Aligned {
    /// Размер выравнивания в байтах.
    const ALIGNMENT: usize;
}
