use kernel_objects::MmioMapping;

/// 32-битные регистры окон устройства с ключом окна `W`. Смещение в
/// [`load`](Registers::load) и [`store`](Registers::store) должно быть выровнено по регистру и
/// лежать внутри окна.
pub trait Registers<W> {
    /// Размер окна `window` в байтах.
    fn size(&self, window: W) -> usize;

    /// Читает регистр окна `window` по смещению `offset`.
    fn load(&self, window: W, offset: usize) -> u32;

    /// Записывает регистр окна `window` по смещению `offset`.
    fn store(&self, window: W, offset: usize, value: u32);
}

/// Набор отображённых окон устройства с ключом окна `W`. Набор даёт [`Registers`] над своими
/// окнами.
pub trait MappedWindows<W> {
    /// Отображение окна `window`.
    fn mapping(&self, window: W) -> &MmioMapping;
}

impl MappedWindows<()> for MmioMapping {
    fn mapping(&self, (): ()) -> &MmioMapping {
        self
    }
}

impl<W, T: MappedWindows<W>> Registers<W> for T {
    fn size(&self, window: W) -> usize {
        self.mapping(window).size()
    }

    fn load(&self, window: W, offset: usize) -> u32 {
        self.mapping(window).mmio().read::<u32>(offset)
    }

    fn store(&self, window: W, offset: usize, value: u32) {
        self.mapping(window).mmio().write::<u32>(offset, value);
    }
}
