use crate::{Driver, MmioAddress, MmioBound};
use alloc::alloc::{alloc, dealloc};
use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::alloc::Layout;
use core::fmt::{Display, Formatter};
use core::ptr::NonNull;
use interrupts::{IrqBound, IrqHandler, IrqNumber, IrqRegistrationError};
use klog::debug;
use memory::MemFlags;
use memory::aligned::Aligned;
use memory::mem_flags::{DeviceMemoryPermission, Owners};
use memory::memory_mapper::MemoryMapper;
use memory::physical_address::PageAlignedAddress;
use memory::virtual_address::{PageAlignedVirtualAddress, VirtualAddress};

#[derive(Clone)]
pub struct MmioBoundError(String);
impl Display for MmioBoundError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "MmioBoundError: {}", self.0)
    }
}

/// Операции runtime-фазы инициализации драйверов.
pub trait InitOps {
    /// Применяет маппинг MMIO-региона.
    fn register_mmio(
        &self,
        address: MmioAddress,
        permissions: Owners<DeviceMemoryPermission>,
    ) -> Result<MmioBound, MmioBoundError>;

    /// Регистрирует IRQ-обработчик.
    fn register_irq(
        &self,
        irq: IrqNumber,
        handler: &'static dyn IrqHandler,
    ) -> Result<IrqBound, IrqRegistrationError>;
}

/// Runtime-реализация операций инициализации, применяющая все запросы немедленно.
pub struct RuntimeRequestApplier {
    pub memory_mapper: &'static dyn MemoryMapper,
}

impl<'a> InitOps for RuntimeRequestApplier {
    fn register_mmio(
        &self,
        address: MmioAddress,
        permissions: Owners<DeviceMemoryPermission>,
    ) -> Result<MmioBound, MmioBoundError> {
        let alignment = PageAlignedVirtualAddress::ALIGNMENT;
        let layout = Layout::from_size_align(address.size(), alignment)
            .map_err(|err| MmioBoundError(format!("Layout Error: {err}")))?;

        let ptr: *mut u8 = unsafe { alloc(layout) };
        let ptr = NonNull::new(ptr).ok_or(MmioBoundError("Allocation failed".to_string()))?;

        let va: VirtualAddress = ptr.into();
        let source_address = PageAlignedVirtualAddress::new(va).unwrap();
        let target_address = PageAlignedAddress::from_usize(address.base()).ok_or_else(|| {
            MmioBoundError("Mmio address is not aligned to 4K boundary".to_string())
        })?;

        let mapper = self.memory_mapper;

        debug!("Mapping {source_address:#x} -> {target_address:#x} with perm: {permissions:?}");

        mapper
            .map_exact(
                source_address,
                target_address,
                address.size(),
                MemFlags::Device(permissions),
            )
            .map_err(|err| MmioBoundError(format!("Mapping error: {err}")))?;

        let cleanup = Box::new(
            move |ptr: NonNull<u8>,
                  layout: Layout,
                  virtual_address: PageAlignedVirtualAddress,
                  size: usize| unsafe {
                let _ = mapper.unmap(virtual_address, size);

                dealloc(ptr.as_ptr(), layout)
            },
        );

        Ok(MmioBound::new(
            ptr,
            layout,
            source_address,
            address.size(),
            cleanup,
        ))
    }

    fn register_irq(
        &self,
        _irq: IrqNumber,
        _handler: &'static dyn IrqHandler,
    ) -> Result<IrqBound, IrqRegistrationError> {
        // self.interrupt_controller()
        todo!()
    }
}

pub struct RuntimeDriverRegistry {
    _drivers: Vec<Box<dyn Driver>>,
}

impl RuntimeDriverRegistry {
    pub fn new() -> Self {
        Self {
            _drivers: Vec::new(),
        }
    }
}
