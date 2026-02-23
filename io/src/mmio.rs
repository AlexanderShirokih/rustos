use core::marker::PhantomData;

/// Типы, допустимые для volatile-доступа (целые 8/16/32/64).
///
/// # Safety
///
/// Реализации должны гарантировать, что тип:
/// - Имеет размер и выравнивание, поддерживаемые для volatile операций (1/2/4/8 байт)
/// - Безопасен для побитового чтения/записи (не содержит padding или инвариантов)
pub unsafe trait VolatileInt: Copy {}
unsafe impl VolatileInt for u8 {}
unsafe impl VolatileInt for u16 {}
unsafe impl VolatileInt for u32 {}
unsafe impl VolatileInt for u64 {}

/// Простая обёртка над базовым адресом MMIO.
#[derive(Copy, Clone)]
pub struct Mmio {
    base: *mut u8, // базовый указатель на регистры
}

// Mmio использует только volatile операции, которые безопасны для многопоточного доступа
unsafe impl Sync for Mmio {}

impl Mmio {
    /// Создаёт хэндлер по виртуальному адресу базы.
    pub const fn new(base_addr: usize) -> Self {
        Self {
            base: base_addr as *mut u8,
        }
    }

    /// Volatile-чтение типа `T` по смещению (в байтах).
    /// Требует корректного выравнивания и валидного диапазона.
    #[inline(always)]
    pub fn read<T: VolatileInt>(&self, offset: usize) -> T {
        unsafe {
            let p = self.base.add(offset) as *const T;
            core::ptr::read_volatile(p)
        }
    }

    /// Volatile-запись типа `T` по смещению (в байтах).
    /// Требует корректного выравнивания и валидного диапазона.
    #[inline(always)]
    pub fn write<T: VolatileInt>(&self, offset: usize, value: T) {
        unsafe {
            let p = self.base.add(offset) as *mut T;
            core::ptr::write_volatile(p, value)
        }
    }

    /// Читает значение из регистра.
    #[inline(always)]
    pub fn read_reg<T: VolatileInt>(&self, reg: Reg<T>) -> T {
        self.read::<T>(reg.offset)
    }

    /// Записывает значение в регистр.
    #[inline(always)]
    pub fn write_reg<T: VolatileInt>(&self, reg: Reg<T>, v: T) {
        self.write::<T>(reg.offset, v)
    }
}

impl From<Mmio> for usize {
    fn from(val: Mmio) -> Self {
        val.base as usize
    }
}

/// Типобезопасный дескриптор регистра со смещением и ожидаемым типом.
#[derive(Copy, Clone)]
pub struct Reg<T> {
    /// Смещение регистра относительно базового адреса MMIO (в байтах).
    pub offset: usize,

    /// Маркер типа для compile-time проверки размера регистра.
    _t: PhantomData<T>,
}

impl<T> Reg<T> {
    pub const fn new(offset: usize) -> Self {
        Self {
            offset,
            _t: PhantomData,
        }
    }

    pub const fn with_offset(&self, additional: usize) -> Self {
        Self::new(self.offset + additional)
    }
}
