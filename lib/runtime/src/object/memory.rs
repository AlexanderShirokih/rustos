//! Типизированные обёртки над метеринг-ресурсом, регионами памяти и их
//! маппингами.

use core::mem;

use syscall::{MemoryAccess, UserMemFlags};

use crate::{
    error::{Error, Result, unit, value},
    handle::{BorrowedHandle, OwnedHandle},
    svc,
};

/// Backing региона памяти, декодированный из `kind_tag` inspect'а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionKind {
    /// Анонимные страницы (`kind_tag == 1`).
    Virtual,
    /// Физический диапазон устройства (`kind_tag == 2`).
    Physical,
}

impl RegionKind {
    /// Декодирует `kind_tag`; провенанс - ядро (валидны 1/2), поэтому неизвестный
    /// тег сводится к `Virtual`, а `debug_assert` ловит дрейф ABI в debug.
    const fn from_tag(tag: u64) -> Self {
        debug_assert!(tag == 1 || tag == 2);
        match tag {
            2 => Self::Physical,
            _ => Self::Virtual,
        }
    }
}

/// Типизированный результат `MemoryRegion::inspect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionInfo {
    /// Размер региона в байтах.
    pub size_bytes: u64,
    /// Базовый PA `Physical`-региона (для корреляции с FDT-узлом); `0` для
    /// `Virtual`.
    pub base: u64,
    /// Backing региона.
    pub kind: RegionKind,
    /// Маска доступа региона.
    pub access: MemoryAccess,
}

/// Владеет хэндлом метеринг-`Resource` и закрывает его на drop.
#[derive(Debug)]
pub struct Resource {
    handle: OwnedHandle,
}

/// Владеет хэндлом региона памяти и закрывает его на drop.
#[derive(Debug)]
pub struct MemoryRegion {
    handle: OwnedHandle,
}

/// Маппинг региона в текущий AS; освобождается в `Drop` через `memory_free`.
#[derive(Debug)]
pub struct Mapping {
    va: u64,
    size_bytes: u64,
}

/// Анонимный маппинг (allocate-fastpath); освобождается в `Drop` через
/// `memory_free`.
#[derive(Debug)]
pub struct AnonymousMapping {
    va: u64,
    size_bytes: u64,
}

impl Resource {
    /// Свежий handle на метеринг-`Resource` текущего процесса.
    pub fn self_resource() -> Self {
        let handle = svc::process_resource_self().expect("process self_resource syscall");
        Self::from_handle(OwnedHandle::from_handle(handle))
    }

    /// Берёт во владение хэндл `Resource`.
    pub fn from_handle(handle: OwnedHandle) -> Self {
        Self { handle }
    }

    /// Заимствование хэндла на время одного вызова.
    pub fn handle(&self) -> BorrowedHandle<'_> {
        self.handle.borrow()
    }

    /// Создаёт регион с Virtual backing размера `size_bytes` и маской `access`.
    pub fn create_virtual(&self, size_bytes: u64, access: MemoryAccess) -> Result<MemoryRegion> {
        svc::memory_create_virtual(self.handle.as_raw(), size_bytes, access.raw())
            .map(|handle| MemoryRegion::from_handle(OwnedHandle::from_handle(handle)))
            .map_err(Error::Syscall)
    }

    /// Выделяет анонимный регион размера `size_bytes` и сразу маппит его на
    /// свободный VA с флагами `flags`.
    pub fn allocate(&self, size_bytes: u64, flags: UserMemFlags) -> Result<AnonymousMapping> {
        let va = value(svc::memory_allocate(
            self.handle.as_raw(),
            size_bytes,
            flags.raw(),
        ))?;
        Ok(AnonymousMapping { va, size_bytes })
    }

    /// Отдаёт владеемый хэндл.
    pub fn into_handle(self) -> OwnedHandle {
        self.handle
    }
}

impl MemoryRegion {
    /// Берёт во владение хэндл `MemoryRegion`.
    pub fn from_handle(handle: OwnedHandle) -> Self {
        Self { handle }
    }

    /// Заимствование хэндла на время одного вызова.
    pub fn handle(&self) -> BorrowedHandle<'_> {
        self.handle.borrow()
    }

    /// Маппит регион на свободный VA: `size_bytes` байт с флагами `flags`.
    pub fn map(&self, size_bytes: u64, flags: UserMemFlags) -> Result<Mapping> {
        let va = value(svc::memory_map(
            self.handle.as_raw(),
            size_bytes,
            flags.raw(),
        ))?;
        Ok(Mapping { va, size_bytes })
    }

    /// Деривация под-региона `[offset, offset+size_bytes)` с маской `access`
    /// (не шире текущей). Требует `DUPLICATE` на хэндле региона.
    pub fn slice(
        &self,
        offset: u64,
        size_bytes: u64,
        access: MemoryAccess,
    ) -> Result<MemoryRegion> {
        svc::memory_slice(self.handle.as_raw(), offset, size_bytes, access.raw())
            .map(|handle| MemoryRegion::from_handle(OwnedHandle::from_handle(handle)))
            .map_err(Error::Syscall)
    }

    /// Декодирует сырой возврат inspect'а `(kind_tag << 16) | access_bits`
    /// в [`RegionInfo`].
    pub fn inspect(&self) -> Result<RegionInfo> {
        let (size, secondary) = svc::memory_region_inspect(self.handle.as_raw());
        let size_bytes = value(size)?;
        let base = secondary & !0xFFF;
        let kind = RegionKind::from_tag((secondary >> 3) & 0x3);
        let access = MemoryAccess::from_bits_truncate(secondary);
        Ok(RegionInfo {
            size_bytes,
            base,
            kind,
            access,
        })
    }

    /// Отдаёт владеемый хэндл.
    pub fn into_handle(self) -> OwnedHandle {
        self.handle
    }
}

impl Mapping {
    /// Базовый VA маппинга.
    pub fn va(&self) -> u64 {
        self.va
    }

    /// Размер маппинга в байтах.
    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    /// Содержимое маппинга как байтовый срез, заимствованный на время маппинга.
    pub fn as_bytes(&self) -> &[u8] {
        let base = self.va as *const u8;
        let len = usize::try_from(self.size_bytes).expect("mapping size fits usize");
        // SAFETY: маппинг владеет диапазоном [va, va+size_bytes) памяти, инициализированной ядром.
        // Срез заимствует его на время `&self` и не переживёт unmap/Drop.
        unsafe { core::slice::from_raw_parts(base, len) }
    }

    /// Меняет флаги маппинга на `flags`.
    pub fn remap(&self, flags: UserMemFlags) -> Result<()> {
        unit(svc::memory_remap(self.va, self.size_bytes, flags.raw()))
    }

    /// Снимает маппинг, подавляя освобождение в `Drop` (без двойного `memory_free`).
    pub fn unmap(self) -> Result<()> {
        let (va, size_bytes) = (self.va, self.size_bytes);
        mem::forget(self);
        unit(svc::memory_free(va, size_bytes))
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        let _ = svc::memory_free(self.va, self.size_bytes);
    }
}

impl AnonymousMapping {
    /// Базовый VA маппинга.
    pub fn va(&self) -> u64 {
        self.va
    }

    /// Размер маппинга в байтах.
    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    /// Меняет флаги маппинга на `flags`.
    pub fn remap(&self, flags: UserMemFlags) -> Result<()> {
        unit(svc::memory_remap(self.va, self.size_bytes, flags.raw()))
    }

    /// Освобождает регион, подавляя освобождение в `Drop` (без двойного `memory_free`).
    pub fn free(self) -> Result<()> {
        let (va, size_bytes) = (self.va, self.size_bytes);
        mem::forget(self);
        unit(svc::memory_free(va, size_bytes))
    }
}

impl Drop for AnonymousMapping {
    fn drop(&mut self) {
        let _ = svc::memory_free(self.va, self.size_bytes);
    }
}
