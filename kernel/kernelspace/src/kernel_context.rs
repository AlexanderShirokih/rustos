use alloc::{sync::Arc, vec::Vec};
use core::num::NonZeroUsize;

use drivers_common::{BootServices, RuntimeDriverRegistry, services::mmio::MmioService};
use memory::{
    MemoryRegion,
    frame_allocator::FrameAllocator,
    memory_mapper::{AddressSpaceFactory, MemoryMapper},
    physical_address::PageAlignedAddress,
    virtual_address::PageAlignedVirtualAddress,
};
use spin::Mutex;

use crate::{services::mmio::MmioServiceImpl, syscall_bridge};

/// Образ userland из initrd.
#[derive(Clone, Copy)]
pub struct UserlandImage {
    pub bytes: &'static [u8],
    pub phys_base: PageAlignedAddress,
}

/// DTB от загрузчика: физдиапазон блоба (зарезервирован раскладкой на весь
/// срок ядра).
#[derive(Clone, Copy)]
pub struct BootDtb {
    pub phys_base: PageAlignedAddress,
    pub size_bytes: NonZeroUsize,
}

impl BootDtb {
    pub fn new(phys_base: PageAlignedAddress, size_bytes: NonZeroUsize) -> Self {
        Self {
            phys_base,
            size_bytes,
        }
    }
}

/// Виртуальная арена под kernel-MMIO маппинги: база и размер.
#[derive(Clone, Copy)]
pub struct KmmioArena {
    pub base: PageAlignedVirtualAddress,
    pub size: NonZeroUsize,
}

pub struct KernelContext {
    services: BootServices,
    driver_registry: Mutex<RuntimeDriverRegistry>,
    address_space_factory: &'static (dyn AddressSpaceFactory + Send + Sync),
    userland_image: Option<UserlandImage>,
    device_regions: Vec<Arc<MemoryRegion>>,
    dtb: BootDtb,
}

impl KernelContext {
    pub fn new(
        memory_mapper: &'static (dyn MemoryMapper + Send + Sync),
        address_space_factory: &'static (dyn AddressSpaceFactory + Send + Sync),
        frame_allocator: &'static (dyn FrameAllocator + Send + Sync),
        mmio_arena: KmmioArena,
        userland_image: Option<UserlandImage>,
        device_regions: Vec<Arc<MemoryRegion>>,
        dtb: BootDtb,
    ) -> KernelContext {
        // Публикуем глобальные слоты для модулей без KernelContext.
        syscall_bridge::install_address_space_factory(address_space_factory);
        syscall_bridge::install_frame_allocator(frame_allocator);

        let mmio_service: Arc<dyn MmioService> = Arc::new(MmioServiceImpl::new(
            memory_mapper,
            mmio_arena.base,
            mmio_arena.size,
        ));

        let mut services = BootServices::new();
        services
            .set_mmio(mmio_service)
            .expect("Failed to register MmioService service");

        Self {
            services,
            driver_registry: Mutex::new(RuntimeDriverRegistry::new()),
            address_space_factory,
            userland_image,
            device_regions,
            dtb,
        }
    }

    /// Фабрика user-AS, переданная в `new`.
    pub fn address_space_factory(&self) -> &'static (dyn AddressSpaceFactory + Send + Sync) {
        self.address_space_factory
    }

    /// Образ userland из initrd, если загрузчик его передал.
    pub fn userland_image(&self) -> Option<UserlandImage> {
        self.userland_image
    }

    /// Device-MMIO регионы из FDT (kernel-owned исключены), вендимые
    /// userspace-драйверам по индексу через bootstrap-протокол.
    pub fn device_regions(&self) -> Vec<Arc<MemoryRegion>> {
        self.device_regions.clone()
    }

    /// DTB от загрузчика: физдиапазон блоба.
    pub fn dtb(&self) -> BootDtb {
        self.dtb
    }

    pub fn with_runtime_state<R>(
        &mut self,
        map: impl FnOnce(&mut BootServices, &mut RuntimeDriverRegistry) -> R,
    ) -> R {
        map(&mut self.services, self.driver_registry.get_mut())
    }
}
