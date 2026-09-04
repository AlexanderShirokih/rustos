//! Типизированные обёртки над метеринг-ресурсом, регионами памяти и их
//! маппингами.

use core::mem;

const _: () = assert!(
    usize::BITS >= u64::BITS,
    "userland runtime targets 64-bit only"
);

use bootstrap_abi::DeviceWindow;
use io::mmio::Mmio;
use ipc::wire::Cap;
use syscall::{MemoryAccess, MemoryProfile, SIGNALED, SyscallError, Timeout, UserMemFlags};

use crate::{
    error::{Error, Result, unit, value},
    handle::{BorrowedHandle, OwnedHandle},
    object::Signal,
    svc,
};

const SPLIT_RETRY_TURN: Timeout = Timeout::from_millis(5);

const WORD: usize = mem::size_of::<u64>();

fn head_len(base: *const u8, len: usize) -> usize {
    let offset = base.align_offset(WORD);
    if offset == usize::MAX {
        len
    } else {
        offset.min(len)
    }
}

#[allow(clippy::cast_ptr_alignment)]
unsafe fn word_ptr(base: *mut u8, index: usize) -> *mut u64 {
    // SAFETY: вызывающий даёт индекс внутри маппинга, смещённый на `head_len` от базы и кратный
    // размеру слова, поэтому адрес выровнен на слово.
    unsafe { base.add(index).cast::<u64>() }
}

/// Backing региона памяти, декодированный из `kind_tag` вызова `inspect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionKind {
    /// Анонимные страницы (`kind_tag == 1`).
    Virtual,
    /// Физический диапазон устройства (`kind_tag == 2`).
    Physical,
}

impl RegionKind {
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
    /// Базовый PA `Physical`-региона (для корреляции с FDT-узлом). `0` для
    /// `Virtual`.
    pub base: u64,
    /// Backing региона.
    pub kind: RegionKind,
    /// Профиль работы с памятью региона.
    pub profile: MemoryProfile,
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

/// Маппинг региона в текущий AS. Освобождается в `Drop` через `memory_free`.
#[derive(Debug)]
pub struct Mapping {
    va: u64,
    size_bytes: u64,
}

/// Маппинг региона, чья маска доступа не допускает запись: страницы неизменны во всех адресных
/// пространствах, и содержимое читается безопасным срезом.
#[derive(Debug)]
pub struct ReadOnlyMapping {
    mapping: Mapping,
}

/// Анонимный маппинг (allocate-fastpath). Освобождается в `Drop` через
/// `memory_free`.
#[derive(Debug)]
pub struct AnonymousMapping {
    va: u64,
    size_bytes: u64,
}

impl Resource {
    /// Свежий handle на метеринг-`Resource` текущего процесса.
    pub fn self_resource() -> Result<Self> {
        let handle = svc::process_resource_self()?;
        Ok(Self::from_handle(OwnedHandle::from_handle(handle)))
    }

    /// Берёт во владение хэндл `Resource`.
    pub fn from_handle(handle: OwnedHandle) -> Self {
        Self { handle }
    }

    /// Заимствование хэндла на время одного вызова.
    pub fn handle(&self) -> BorrowedHandle<'_> {
        self.handle.borrow()
    }

    /// Выделяет `budget_pages` страниц бюджета в отдельный `Resource` с тем же
    /// физдиапазоном и потолком доступа. Страницы возвращаются исходному
    /// ресурсу, когда производный уходит.
    pub fn split(&self, budget_pages: u64) -> Result<Self> {
        svc::resource_split(self.handle.as_raw(), budget_pages)
            .map(|handle| Self::from_handle(OwnedHandle::from_handle(handle)))
            .map_err(Error::Syscall)
    }

    /// Выделяет `budget_pages` страниц, повторяя попытку до `timeout`, пока
    /// бюджета не хватает. Исчерпание срока возвращает `Timeout`.
    ///
    /// Страницы ушедшего поддерева возвращаются позже наблюдаемого завершения
    /// его процессов.
    pub fn split_when_available(&self, budget_pages: u64, timeout: Timeout) -> Result<Self> {
        match self.split(budget_pages) {
            Err(Error::Syscall(SyscallError::ResourceExhausted)) => {}
            outcome => return outcome,
        }

        let turn = Signal::create()?;
        for _ in 1..timeout.raw() / SPLIT_RETRY_TURN.raw() {
            match turn.wait(SIGNALED, SPLIT_RETRY_TURN) {
                Err(Error::Syscall(SyscallError::Timeout)) => {}
                Err(error) => return Err(error),
                Ok(_) => return Err(Error::Syscall(SyscallError::Canceled)),
            }
            match self.split(budget_pages) {
                Err(Error::Syscall(SyscallError::ResourceExhausted)) => {}
                outcome => return outcome,
            }
        }
        Err(Error::Syscall(SyscallError::Timeout))
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

    /// Усыновляет capability региона, доставленный по IPC.
    pub fn adopt(cap: Cap) -> Self {
        Self::from_handle(OwnedHandle::adopt(cap))
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

    /// Маппит регион целиком (размер из `inspect`) с флагами `flags`.
    pub fn map_full(&self, flags: UserMemFlags) -> Result<Mapping> {
        let info = self.inspect()?;
        self.map(info.size_bytes, flags)
    }

    /// Маппит регион целиком на чтение, удостоверяя неизменность его страниц. Регион, чья маска
    /// доступа допускает запись, отвергается `AccessDenied`: его страницы вправе меняться под
    /// другим отображением.
    pub fn map_full_read_only(&self) -> Result<ReadOnlyMapping> {
        let info = self.inspect()?;
        if info.access.raw() & MemoryAccess::WRITE.raw() != 0 {
            return Err(Error::Syscall(SyscallError::AccessDenied));
        }
        let mapping = self.map(info.size_bytes, UserMemFlags::ReadOnly)?;
        Ok(ReadOnlyMapping { mapping })
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

    /// Деривация региона поверх того же PA с профилем `profile`. Профиль
    /// меняется в пределах некэшируемых. Требует `DUPLICATE` на хэндле региона.
    pub fn with_profile(&self, profile: MemoryProfile) -> Result<MemoryRegion> {
        svc::memory_with_profile(self.handle.as_raw(), profile.raw())
            .map(|handle| MemoryRegion::from_handle(OwnedHandle::from_handle(handle)))
            .map_err(Error::Syscall)
    }

    /// Декодирует сырой возврат `memory_region_inspect` в [`RegionInfo`].
    pub fn inspect(&self) -> Result<RegionInfo> {
        let (size, secondary) = svc::memory_region_inspect(self.handle.as_raw());
        let size_bytes = value(size)?;
        let base = secondary & !0xFFF;
        let kind = RegionKind::from_tag((secondary >> 3) & 0x3);
        let profile = MemoryProfile::from_raw((secondary >> 5) & 0x3)
            .map_err(|_| Error::Syscall(SyscallError::InvalidArgument))?;
        let access = MemoryAccess::from_bits_truncate(secondary);
        Ok(RegionInfo {
            size_bytes,
            base,
            kind,
            profile,
            access,
        })
    }

    /// Отдаёт владеемый хэндл.
    pub fn into_handle(self) -> OwnedHandle {
        self.handle
    }
}

impl DeviceWindow for MemoryRegion {
    fn adopt(cap: Cap) -> Self {
        Self::adopt(cap)
    }

    fn base(&self) -> Option<u64> {
        self.inspect().ok().map(|info| info.base)
    }
}

impl Mapping {
    /// Базовый VA маппинга.
    pub fn va(&self) -> u64 {
        self.va
    }

    /// MMIO-хэндлер по базовому VA маппинга (device-окна).
    pub fn mmio(&self) -> Mmio {
        #[allow(clippy::cast_possible_truncation)]
        let base = self.va as usize;
        Mmio::new(base)
    }

    /// Размер маппинга в байтах.
    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    /// Содержимое маппинга как байтовый срез, заимствованный на время маппинга.
    ///
    /// # Safety
    ///
    /// Страницы маппинга остаются неизменными на всё время жизни среза: у
    /// региона нет отображений на запись ни в одном адресном пространстве.
    pub unsafe fn as_bytes(&self) -> &[u8] {
        let base = self.va as *const u8;
        #[allow(clippy::expect_used)]
        let len = usize::try_from(self.size_bytes).expect("mapping size fits usize");
        // SAFETY: ядро установило маппинг на [va, va+size_bytes) и
        // инициализировало эту память. Срез заимствует диапазон на время `&self`
        // и не переживёт unmap/Drop, а неизменность страниц на это время
        // гарантирует вызывающий.
        unsafe { core::slice::from_raw_parts(base, len) }
    }

    /// Копирует в `dst` первые `dst.len()` байт маппинга. Длина сверх размера
    /// маппинга - `OutOfBounds`.
    pub fn copy_out(&self, dst: &mut [u8]) -> Result<()> {
        self.copy_out_at(0, dst)
    }

    /// Копирует в `dst` байты маппинга со смещения `offset`. Диапазон сверх размера маппинга -
    /// `OutOfBounds`.
    pub fn copy_out_at(&self, offset: usize, dst: &mut [u8]) -> Result<()> {
        let len = dst.len();
        let base = self.range_ptr(offset, len)?;
        let head = head_len(base, len);
        let target = dst.as_mut_ptr();
        // SAFETY: [va, va+len) лежит в маппинге, установленном ядром, а `target` покрывает
        // столько же байт. Сторона маппинга читается volatile: те же страницы бывают отображены
        // в другом адресном пространстве. Середина идёт словами по выровненному адресу, голова и
        // хвост - побайтно; каждый байт читается ровно один раз, а разрыв, наблюдаемый писателем
        // из другого адресного пространства, атомарен с точностью до слова.
        #[allow(clippy::multiple_unsafe_ops_per_block)]
        unsafe {
            for index in 0..head {
                target.add(index).write(base.add(index).read_volatile());
            }
            let mut index = head;
            while index + WORD <= len {
                let word = word_ptr(base, index).read_volatile();
                target.add(index).cast::<u64>().write_unaligned(word);
                index += WORD;
            }
            while index < len {
                target.add(index).write(base.add(index).read_volatile());
                index += 1;
            }
        }
        Ok(())
    }

    /// Копирует `src` в начало маппинга. Длина сверх размера маппинга -
    /// `OutOfBounds`. Приём по `&self`: эксклюзивности над страницами маппинг
    /// не даёт.
    pub fn copy_in(&self, src: &[u8]) -> Result<()> {
        self.copy_in_at(0, src)
    }

    /// Копирует `src` в маппинг со смещения `offset`. Диапазон сверх размера маппинга -
    /// `OutOfBounds`.
    pub fn copy_in_at(&self, offset: usize, src: &[u8]) -> Result<()> {
        let len = src.len();
        let base = self.range_ptr(offset, len)?;
        let head = head_len(base, len);
        let source = src.as_ptr();
        // SAFETY: [va, va+len) лежит в маппинге, установленном ядром, а `source` покрывает
        // столько же байт. Сторона маппинга пишется volatile: те же страницы бывают отображены
        // в другом адресном пространстве. Середина идёт словами по выровненному адресу, голова и
        // хвост - побайтно; каждый байт пишется ровно один раз, а разрыв, наблюдаемый читателем
        // из другого адресного пространства, атомарен с точностью до слова.
        #[allow(clippy::multiple_unsafe_ops_per_block)]
        unsafe {
            for index in 0..head {
                base.add(index).write_volatile(source.add(index).read());
            }
            let mut index = head;
            while index + WORD <= len {
                let word = source.add(index).cast::<u64>().read_unaligned();
                word_ptr(base, index).write_volatile(word);
                index += WORD;
            }
            while index < len {
                base.add(index).write_volatile(source.add(index).read());
                index += 1;
            }
        }
        Ok(())
    }

    /// Заполняет нулями `len` байт маппинга со смещения `offset`. Диапазон сверх размера
    /// маппинга - `OutOfBounds`.
    pub fn zero_at(&self, offset: usize, len: usize) -> Result<()> {
        let base = self.range_ptr(offset, len)?;
        let head = head_len(base, len);
        // SAFETY: [va, va+len) лежит в маппинге, установленном ядром. Запись идёт volatile: те
        // же страницы бывают отображены в другом адресном пространстве. Середина идёт словами по
        // выровненному адресу, голова и хвост - побайтно; каждый байт пишется ровно один раз, а
        // разрыв, наблюдаемый читателем из другого адресного пространства, атомарен с точностью
        // до слова.
        #[allow(clippy::multiple_unsafe_ops_per_block)]
        unsafe {
            for index in 0..head {
                base.add(index).write_volatile(0);
            }
            let mut index = head;
            while index + WORD <= len {
                word_ptr(base, index).write_volatile(0);
                index += WORD;
            }
            while index < len {
                base.add(index).write_volatile(0);
                index += 1;
            }
        }
        Ok(())
    }

    fn range_ptr(&self, offset: usize, len: usize) -> Result<*mut u8> {
        let size = usize::try_from(self.size_bytes).map_err(|_| Error::OutOfBounds)?;
        let end = offset.checked_add(len).ok_or(Error::OutOfBounds)?;
        if end > size {
            return Err(Error::OutOfBounds);
        }
        let base = usize::try_from(self.va).map_err(|_| Error::OutOfBounds)?;
        Ok((base + offset) as *mut u8)
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

impl ReadOnlyMapping {
    /// Содержимое маппинга. Срез заимствован на время его жизни.
    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: маска доступа региона не допускает запись, так что отображений на запись у его
        // страниц нет ни в одном адресном пространстве, а срез не переживает маппинг.
        unsafe { self.mapping.as_bytes() }
    }

    /// Размер маппинга в байтах.
    pub fn size_bytes(&self) -> u64 {
        self.mapping.size_bytes()
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
