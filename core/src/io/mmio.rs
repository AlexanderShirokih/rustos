use core::marker::PhantomData;

/// Типы, допустимые для volatile-доступа (целые 8/16/32/64).
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
    /// Создаёт хэндлер по физ. адресу базы.
    #[inline(always)]
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

    /// Барьер памяти DMB SY: упорядочивает все load/store по всей системе.
    #[inline(always)]
    pub fn dmb_system() {
        unsafe { core::arch::asm!("dmb sy", options(nostack, preserves_flags)) }
    }

    #[inline(always)]
    pub fn read_reg<T: VolatileInt>(&self, reg: Register<T>) -> T {
        self.read::<T>(reg.offset)
    }
    #[inline(always)]
    pub fn write_reg<T: VolatileInt>(&self, reg: Register<T>, v: T) {
        self.write::<T>(reg.offset, v)
    }
}

/// Типобезопасный дескриптор регистра со смещением и ожидаемым типом.
pub struct Register<T> {
    pub offset: usize,
    _t: PhantomData<T>,
}

impl<T> Register<T> {
    #[inline(always)]
    pub const fn new(offset: usize) -> Self {
        Self {
            offset,
            _t: PhantomData,
        }
    }
}
