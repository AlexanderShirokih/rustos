use alloc::{sync::Arc, vec::Vec};
use core::num::NonZeroUsize;

use drivers_common::{
    BootServices, RuntimeDriverRegistry,
    services::{interrupts::IrqLineSpan, mmio::MmioService},
};
use memory::{
    MemoryRegion,
    frame_allocator::{FrameAllocator, FrameCount},
    memory_mapper::{AddressSpaceFactory, MemoryMapper},
    page_round_up,
    physical_address::{PageAlignedAddress, PhysicalAddress},
    virtual_address::PageAlignedVirtualAddress,
};
use spin::Mutex;

use crate::{services::mmio::MmioServiceImpl, syscall_bridge};

/// Образ userland из initrd. База приходит от загрузчика и на страницу не выровнена.
#[derive(Clone, Copy)]
pub struct UserlandImage {
    pub bytes: &'static [u8],
    pub phys_base: PhysicalAddress,
}

/// Страничный регион, целиком покрывающий блоб загрузчика.
#[derive(Clone, Copy)]
pub struct BlobRegion {
    /// База региона, выровненная вниз до страницы.
    pub base: PageAlignedAddress,
    /// Размер региона, округлённый вверх до страницы.
    pub size: NonZeroUsize,
    /// Смещение первого байта блоба от базы региона.
    pub offset: usize,
}

impl BlobRegion {
    /// Строит покрывающий регион по точной базе блоба и его длине.
    /// `None` - размер не округляется до страницы в пределах адресного пространства.
    pub fn covering(phys_base: PhysicalAddress, size_bytes: NonZeroUsize) -> Option<Self> {
        let base = PageAlignedAddress::aligned_down(phys_base);
        let offset = phys_base.as_usize() - base.as_usize();
        let span = NonZeroUsize::new(offset.checked_add(size_bytes.get())?)?;

        Some(Self {
            base,
            size: page_round_up(span)?,
            offset,
        })
    }
}

/// DTB от загрузчика: физдиапазон блоба (зарезервирован раскладкой на весь
/// срок ядра). База приходит от загрузчика и на страницу не выровнена.
#[derive(Clone, Copy)]
pub struct BootDtb {
    pub phys_base: PhysicalAddress,
    pub size_bytes: NonZeroUsize,
}

impl BootDtb {
    pub fn new(phys_base: PhysicalAddress, size_bytes: NonZeroUsize) -> Self {
        Self {
            phys_base,
            size_bytes,
        }
    }

    /// Страничный регион под выдачу блоба userspace.
    pub fn blob_region(&self) -> Option<BlobRegion> {
        BlobRegion::covering(self.phys_base, self.size_bytes)
    }
}

/// Виртуальная арена под kernel-MMIO маппинги: база и размер.
#[derive(Clone, Copy)]
pub struct KmmioArena {
    pub base: PageAlignedVirtualAddress,
    pub size: NonZeroUsize,
}

/// Аллокатор фреймов физической памяти в двух представлениях, выделения фреймов и их счёта.
#[derive(Clone, Copy)]
pub struct PhysicalFrames {
    pub allocator: &'static (dyn FrameAllocator + Send + Sync),
    pub count: &'static (dyn FrameCount + Send + Sync),
}

pub struct KernelContext {
    services: BootServices,
    driver_registry: Mutex<RuntimeDriverRegistry>,
    address_space_factory: &'static (dyn AddressSpaceFactory + Send + Sync),
    frame_count: &'static (dyn FrameCount + Send + Sync),
    userland_image: Option<UserlandImage>,
    device_regions: Vec<Arc<MemoryRegion>>,
    dtb: BootDtb,
}

impl KernelContext {
    pub fn new(
        memory_mapper: &'static (dyn MemoryMapper + Send + Sync),
        address_space_factory: &'static (dyn AddressSpaceFactory + Send + Sync),
        frames: PhysicalFrames,
        mmio_arena: KmmioArena,
        userland_image: Option<UserlandImage>,
        device_regions: Vec<Arc<MemoryRegion>>,
        dtb: BootDtb,
    ) -> KernelContext {
        // Публикуем глобальные слоты для модулей без KernelContext.
        syscall_bridge::install_address_space_factory(address_space_factory);
        syscall_bridge::install_frame_allocator(frames.allocator);

        let mmio_service: Arc<dyn MmioService> = Arc::new(MmioServiceImpl::new(
            memory_mapper,
            mmio_arena.base,
            mmio_arena.size,
        ));

        Self {
            services: BootServices::with_mmio(mmio_service),
            driver_registry: Mutex::new(RuntimeDriverRegistry::new()),
            address_space_factory,
            frame_count: frames.count,
            userland_image,
            device_regions,
            dtb,
        }
    }

    /// Фабрика user-AS, переданная в `new`.
    pub fn address_space_factory(&self) -> &'static (dyn AddressSpaceFactory + Send + Sync) {
        self.address_space_factory
    }

    /// Счёт фреймов аллокатора физической памяти, переданный в `new`.
    pub fn frame_count(&self) -> &'static (dyn FrameCount + Send + Sync) {
        self.frame_count
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

    /// Полоса линий прерываний, которую контроллер платформы отдаёт наружу.
    /// `None` до установки сервиса прерываний.
    pub fn assignable_irq_lines(&mut self) -> Option<IrqLineSpan> {
        self.with_runtime_state(|services, _| {
            services
                .require_interrupts()
                .ok()
                .map(|interrupts| interrupts.assignable_lines())
        })
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
