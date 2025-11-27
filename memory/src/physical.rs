pub trait AddressType: Copy + Clone + PartialOrd {
    fn as_usize(self) -> usize;
    fn as_physical_address(self) -> PhysicalAddress;
}

pub trait Aligned {
    fn alignment() -> usize;
}

impl AddressType for PhysicalAddress {
    #[inline]
    fn as_usize(self) -> usize {
        self.0
    }

    #[inline]
    fn as_physical_address(self) -> PhysicalAddress {
        self
    }
}

/// Адрес физической памяти
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhysicalAddress(pub usize);

impl PhysicalAddress {
    pub const fn new(address: usize) -> Self {
        Self(address)
    }

    pub const fn as_usize(self) -> usize {
        self.0
    }

    pub fn align_down(self, frame_size: usize) -> PageAlignedAddress {
        debug_assert!(frame_size == PageAlignedAddress::alignment());

        PageAlignedAddress::aligned_down(self)
    }

    pub fn align_up(self, frame_size: usize) -> PageAlignedAddress {
        debug_assert!(frame_size == PageAlignedAddress::alignment());

        PageAlignedAddress::aligned_up(self)
    }

    #[inline]
    pub const fn add(&self, x: usize) -> Self {
        Self(self.0 + x)
    }

    /// Добавляет значение с проверкой переполнения
    #[inline]
    pub fn checked_add(&self, x: usize) -> Option<Self> {
        self.0.checked_add(x).map(Self)
    }

    /// Вычитает значение с проверкой переполнения
    #[inline]
    pub fn checked_sub(&self, x: usize) -> Option<Self> {
        self.0.checked_sub(x).map(Self)
    }

    #[inline]
    pub fn sub(self, x: usize) -> Self {
        Self(self.0 - x)
    }
}

impl From<usize> for PhysicalAddress {
    fn from(v: usize) -> Self {
        Self(v)
    }
}

/// Выровненный физический адрес
/// Гарантирует, что адрес выровнен на заданную границу
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct AlignedPhysicalAddress<const ALIGNMENT: usize>(PhysicalAddress);

impl<const ALIGNMENT: usize> AlignedPhysicalAddress<ALIGNMENT> {
    pub const fn zero() -> Self {
        Self(PhysicalAddress(0))
    }

    pub fn new(address: PhysicalAddress) -> Option<Self> {
        if address.0 % ALIGNMENT == 0 {
            Some(Self(address))
        } else {
            None
        }
    }

    pub fn from_usize(address: usize) -> Option<Self> {
        if address % ALIGNMENT == 0 {
            Some(Self(PhysicalAddress(address)))
        } else {
            None
        }
    }

    /// Создаёт выровненный адрес без проверки выравнивания
    pub const fn new_unchecked(address: PhysicalAddress) -> Self {
        Self(address)
    }

    /// Создаёт выровненный адрес из usize без проверки выравнивания
    pub const fn from_usize_unchecked(address: usize) -> Self {
        Self(PhysicalAddress(address))
    }

    /// Создаёт выровненный адрес, выравнивая вниз
    pub fn aligned_down(address: PhysicalAddress) -> Self {
        let aligned = (address.0 / ALIGNMENT) * ALIGNMENT;
        Self(PhysicalAddress(aligned))
    }

    /// Создаёт выровненный адрес, выравнивая вверх
    pub fn aligned_up(address: PhysicalAddress) -> Self {
        let aligned = address.0.div_ceil(ALIGNMENT) * ALIGNMENT;
        Self(PhysicalAddress(aligned))
    }

    /// Получить внутренний PhysicalAddress
    pub const fn as_physical_address(self) -> PhysicalAddress {
        self.0
    }

    pub const fn as_usize(self) -> usize {
        self.0.as_usize()
    }

    pub const fn next_aligned(&self) -> Self {
        Self(self.0.add(ALIGNMENT))
    }

    /// Получить размер выравнивания
    pub const fn alignment() -> usize {
        ALIGNMENT
    }
}

impl<const ALIGNMENT: usize> AddressType for AlignedPhysicalAddress<ALIGNMENT> {
    #[inline]
    fn as_usize(self) -> usize {
        self.0.as_usize()
    }

    #[inline]
    fn as_physical_address(self) -> PhysicalAddress {
        self.0
    }
}

impl<const ALIGNMENT: usize> Aligned for AlignedPhysicalAddress<ALIGNMENT> {
    fn alignment() -> usize {
        ALIGNMENT
    }
}

// Автоматическое преобразование в PhysicalAddress
impl<const ALIGNMENT: usize> From<AlignedPhysicalAddress<ALIGNMENT>> for PhysicalAddress {
    fn from(addr: AlignedPhysicalAddress<ALIGNMENT>) -> Self {
        addr.0
    }
}

// Псевдоним для частного случая
pub type PageAlignedAddress = AlignedPhysicalAddress<4096>; // 4KB страницы

/// Фрейм физической памяти
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Frame(pub usize);

impl Frame {
    pub const fn new(number: usize) -> Self {
        Self(number)
    }

    pub const fn add(&self, frames_offset: usize) -> Self {
        Self::new(self.0 + frames_offset)
    }

    pub const fn sub(&self, frames_offset: usize) -> Self {
        Self::new(self.0 - frames_offset)
    }

    pub fn page_address(&self) -> PageAlignedAddress {
        PageAlignedAddress::from_usize_unchecked(self.0 * PageAlignedAddress::alignment())
    }

    #[inline]
    pub fn containing_address<A: AddressType>(address: A) -> Self {
        Self(address.as_usize() / PageAlignedAddress::alignment())
    }

    pub const fn number(&self) -> usize {
        self.0
    }
}

impl From<PageAlignedAddress> for Frame {
    fn from(addr: PageAlignedAddress) -> Self {
        Frame::new(addr.as_usize() / PageAlignedAddress::alignment())
    }
}
