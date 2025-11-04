use core::mem::{MaybeUninit, align_of, size_of};
use core::ptr::addr_of_mut;

// Встроенная мини-куча для хранения объектов до момента настройки полноценной кучи
#[unsafe(link_section = ".bss.heap")]
static mut EMBEDDED_BUMP_ALLOCATOR: MaybeUninit<BumpAllocator> = MaybeUninit::uninit();

pub struct BumpAllocator {
    buffer: [u8; 8192],
    offset: usize,
}

impl BumpAllocator {
    pub fn init(&mut self) {
        self.offset = 0;
    }

    #[inline]
    fn align_up(address: usize, align: usize) -> usize {
        (address + (align - 1)) & !(align - 1)
    }

    pub fn alloc_uninit<T>(&mut self) -> Result<&mut MaybeUninit<T>, BumpAllocError> {
        let align = align_of::<T>();
        let size = size_of::<T>();

        let base = self.buffer.as_mut_ptr() as usize;
        let start = base + self.offset;
        let aligned = Self::align_up(start, align);

        let new_offset = (aligned - base)
            .checked_add(size)
            .ok_or(BumpAllocError::AddressOverflow)?;

        if new_offset > self.buffer.len() {
            return Err(BumpAllocError::OutOfMemory {
                required_size: size,
                available_size: self.buffer.len() - self.offset,
            });
        }

        let ptr = aligned as *mut MaybeUninit<T>;
        self.offset = new_offset;

        unsafe { Ok(&mut *ptr) }
    }

    /// Выделяет память и инициализирует значением, возвращая сырой указатель.
    /// Полезно, когда нужно избежать проблем с заимствованием
    #[inline]
    pub fn alloc_ptr<T>(&mut self, value: T) -> Result<*const T, BumpAllocError> {
        let uninit = self.alloc_uninit::<T>()?;
        let ptr = uninit.as_ptr();
        uninit.write(value);
        Ok(ptr)
    }
}

pub fn bump_allocator() -> &'static mut BumpAllocator {
    unsafe { &mut *addr_of_mut!(EMBEDDED_BUMP_ALLOCATOR).cast::<BumpAllocator>() }
}

#[derive(Debug)]
pub enum BumpAllocError {
    AddressOverflow,

    OutOfMemory {
        required_size: usize,
        available_size: usize,
    },
}
