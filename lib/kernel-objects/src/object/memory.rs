//! Типизированные обёртки над метеринг-ресурсом, регионами памяти и их
//! маппингами.
//!
//! Страницы маппинга бывают отображены ещё и в чужом адресном пространстве, где их читают и
//! пишут параллельно. Доступ к маппингу идёт только volatile-операциями. Компилятор не
//! вправе ни выбросить такую операцию, ни переупорядочить, ни склеить две соседние.
//!
//! Диапазон делится на три части. Голова до ближайшей границы слова и хвост за последним
//! целым словом передаются побайтно, середина - словами по `u64`. Каждый байт при этом
//! передаётся ровно один раз. Из другого адресного пространства середина наблюдается
//! пословно, голова и хвост - побайтно. Порядок трёх частей между собой ничем не закреплён.

use core::mem;

const _: () = assert!(
    usize::BITS >= u64::BITS,
    "userland runtime targets 64-bit only"
);

use bootstrap_abi::DeviceWindow;
use io::mmio::Mmio;
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

unsafe fn word_ptr(base: *mut u8, index: usize) -> *mut u64 {
    // SAFETY: вызывающий даёт индекс в диапазоне, проверенном `range_ptr`, отстоящий от `head_len`
    // на целое число слов. Адрес выровнен на слово, потому что `head_len` отсчитан `align_offset`
    // от базы.
    unsafe { base.add(index) }.cast()
}

unsafe fn read_bytes(base: *const u8, start: usize, dst: &mut [u8]) {
    for (offset, byte) in dst.iter_mut().enumerate() {
        // SAFETY: вызывающий даёт диапазон `[start, start + dst.len())`, проверенный `range_ptr`.
        let source = unsafe { base.add(start + offset) };
        // SAFETY: адрес лежит в маппинге, установленном ядром.
        *byte = unsafe { source.read_volatile() };
    }
}

unsafe fn write_bytes(base: *mut u8, start: usize, src: &[u8]) {
    for (offset, &byte) in src.iter().enumerate() {
        // SAFETY: вызывающий даёт диапазон `[start, start + src.len())`, проверенный `range_ptr`.
        let target = unsafe { base.add(start + offset) };
        // SAFETY: адрес лежит в маппинге, установленном ядром.
        unsafe { target.write_volatile(byte) };
    }
}

unsafe fn zero_bytes(base: *mut u8, start: usize, len: usize) {
    for offset in 0..len {
        // SAFETY: вызывающий даёт диапазон `[start, start + len)`, проверенный `range_ptr`.
        let target = unsafe { base.add(start + offset) };
        // SAFETY: адрес лежит в маппинге, установленном ядром.
        unsafe { target.write_volatile(0) };
    }
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

/// Маппинг региона, чья маска доступа не допускает запись. Страницы неизменны во всех адресных
/// пространствах. Содержимое читается безопасным срезом.
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
    /// доступа допускает запись, отвергается `AccessDenied`. Под другим отображением такие
    /// страницы меняются, и неизменность обещать нечем.
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

impl From<syscall_user::ReceivedHandle> for MemoryRegion {
    fn from(handle: syscall_user::ReceivedHandle) -> Self {
        Self::from_handle(handle.into())
    }
}

impl DeviceWindow<syscall_user::ReceivedHandle> for MemoryRegion {
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
    /// Страницы маппинга остаются неизменными на всё время жизни среза. У
    /// региона нет отображений на запись ни в одном адресном пространстве.
    pub unsafe fn as_bytes(&self) -> &[u8] {
        let base = self.va as *const u8;
        // Проверка в голове модуля исключает 32-битный таргет, и приведение не усекает значение.
        #[allow(clippy::cast_possible_truncation)]
        let len = self.size_bytes as usize;
        // SAFETY: ядро установило маппинг на [va, va+size_bytes) и
        // инициализировало эту память. Срез заимствует диапазон на время `&self`
        // и теряет валидность при unmap/Drop, а неизменность страниц на это время
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
        let (head_bytes, rest) = dst.split_at_mut(head);
        let (words, tail) = rest.as_chunks_mut::<WORD>();
        // SAFETY: `[0, head)` лежит в диапазоне, проверенном `range_ptr`.
        unsafe { read_bytes(base, 0, head_bytes) };
        for (number, chunk) in words.iter_mut().enumerate() {
            // SAFETY: слово отстоит от `head` на `number` слов и лежит в диапазоне, проверенном
            // `range_ptr`.
            let word = unsafe { word_ptr(base, head + number * WORD) };
            // SAFETY: слово лежит в маппинге, установленном ядром.
            *chunk = unsafe { word.read_volatile() }.to_ne_bytes();
        }
        // SAFETY: хвост `[len - tail.len(), len)` лежит в диапазоне, проверенном `range_ptr`.
        unsafe { read_bytes(base, len - tail.len(), tail) };
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
        let (head_bytes, rest) = src.split_at(head);
        let (words, tail) = rest.as_chunks::<WORD>();
        // SAFETY: `[0, head)` лежит в диапазоне, проверенном `range_ptr`.
        unsafe { write_bytes(base, 0, head_bytes) };
        for (number, chunk) in words.iter().enumerate() {
            // SAFETY: слово отстоит от `head` на `number` слов и лежит в диапазоне, проверенном
            // `range_ptr`.
            let word = unsafe { word_ptr(base, head + number * WORD) };
            // SAFETY: слово лежит в маппинге, установленном ядром.
            unsafe { word.write_volatile(u64::from_ne_bytes(*chunk)) };
        }
        // SAFETY: хвост `[len - tail.len(), len)` лежит в диапазоне, проверенном `range_ptr`.
        unsafe { write_bytes(base, len - tail.len(), tail) };
        Ok(())
    }

    /// Заполняет нулями `len` байт маппинга со смещения `offset`. Диапазон сверх размера
    /// маппинга - `OutOfBounds`.
    pub fn zero_at(&self, offset: usize, len: usize) -> Result<()> {
        let base = self.range_ptr(offset, len)?;
        let head = head_len(base, len);
        let words = (len - head) / WORD;
        let tail_start = head + words * WORD;
        // SAFETY: `[0, head)` лежит в диапазоне, проверенном `range_ptr`.
        unsafe { zero_bytes(base, 0, head) };
        for number in 0..words {
            // SAFETY: слово отстоит от `head` на `number` слов и лежит в диапазоне, проверенном
            // `range_ptr`.
            let word = unsafe { word_ptr(base, head + number * WORD) };
            // SAFETY: слово лежит в маппинге, установленном ядром.
            unsafe { word.write_volatile(0) };
        }
        // SAFETY: хвост `[tail_start, len)` лежит в диапазоне, проверенном `range_ptr`.
        unsafe { zero_bytes(base, tail_start, len - tail_start) };
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
        // страниц нет ни в одном адресном пространстве, а срез заимствован не дольше маппинга.
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

#[cfg(test)]
mod tests {
    use core::mem::ManuallyDrop;

    use super::*;

    const BUFFER_BYTES: usize = 64;
    const UNTOUCHED: u8 = 0xAA;
    const SHIFTS: core::ops::Range<usize> = 0..WORD;
    const LEN_MAX: usize = 40;

    #[repr(align(8))]
    struct WordAlignedBuffer([u8; BUFFER_BYTES]);

    impl WordAlignedBuffer {
        fn untouched() -> Self {
            Self([UNTOUCHED; BUFFER_BYTES])
        }

        fn base(&mut self) -> *mut u8 {
            self.0.as_mut_ptr()
        }

        fn mapping(&mut self) -> ManuallyDrop<Mapping> {
            let va = u64::try_from(self.base().expose_provenance()).unwrap();
            let size_bytes = u64::try_from(BUFFER_BYTES).unwrap();
            // Освобождение в `Drop` подавлено, ядро эти страницы не выдавало.
            ManuallyDrop::new(Mapping { va, size_bytes })
        }
    }

    fn lens() -> impl Iterator<Item = usize> {
        (0..=16).chain([24, 32, LEN_MAX])
    }

    fn untouched_except(offset: usize, bytes: &[u8]) -> [u8; BUFFER_BYTES] {
        let mut expected = [UNTOUCHED; BUFFER_BYTES];
        expected[offset..offset + bytes.len()].copy_from_slice(bytes);
        expected
    }

    fn source_bytes() -> [u8; LEN_MAX + 1] {
        let mut pattern = [0; LEN_MAX + 1];
        let mut byte: u8 = 1;
        for slot in &mut pattern {
            *slot = byte;
            byte = byte.wrapping_add(3);
        }
        pattern
    }
    #[test]
    fn head_len_reaches_word_boundary_within_len() {
        let mut buffer = WordAlignedBuffer::untouched();
        let aligned = buffer.base();

        for shift in SHIFTS {
            for len in lens() {
                let base = aligned.wrapping_add(shift);
                let head = head_len(base, len);

                assert!(head <= len, "shift {shift}, len {len}: head {head}");
                assert_eq!(
                    head,
                    ((WORD - shift) % WORD).min(len),
                    "shift {shift}, len {len}"
                );
                assert!(
                    head == len || base.wrapping_add(head).addr() % WORD == 0,
                    "shift {shift}, len {len}: head {head} ends off the word boundary"
                );
            }
        }
    }
    #[test]
    fn head_len_is_len_when_range_ends_before_word_boundary() {
        let mut buffer = WordAlignedBuffer::untouched();
        let base = buffer.base().wrapping_add(3);

        assert_eq!(head_len(base, 2), 2);
        assert_eq!(head_len(base, 5), 5);
        assert_eq!(head_len(base, 6), 5);
    }
    #[test]
    fn copy_in_at_writes_exactly_the_range() {
        let pattern = source_bytes();

        for shift in SHIFTS {
            for len in lens() {
                let mut buffer = WordAlignedBuffer::untouched();
                let mapping = buffer.mapping();
                // Источник берётся с невыровненного адреса, и слова из него читаются без учёта
                // выравнивания.
                let source = &pattern[1..=len];

                mapping.copy_in_at(shift, source).unwrap();

                assert_eq!(
                    buffer.0,
                    untouched_except(shift, source),
                    "shift {shift}, len {len}"
                );
            }
        }
    }
    #[test]
    fn copy_out_at_reads_back_written_range() {
        let pattern = source_bytes();

        for shift in SHIFTS {
            for len in lens() {
                let mut buffer = WordAlignedBuffer::untouched();
                let mapping = buffer.mapping();
                let source = &pattern[1..=len];
                mapping.copy_in_at(shift, source).unwrap();
                // Приёмник берётся с невыровненного адреса, и слова в него пишутся без учёта
                // выравнивания.
                let mut target = [UNTOUCHED; BUFFER_BYTES];

                mapping.copy_out_at(shift, &mut target[1..=len]).unwrap();

                assert_eq!(
                    target,
                    untouched_except(1, source),
                    "shift {shift}, len {len}"
                );
            }
        }
    }
    #[test]
    fn zero_at_clears_exactly_the_range() {
        for shift in SHIFTS {
            for len in lens() {
                let mut buffer = WordAlignedBuffer::untouched();
                let mapping = buffer.mapping();

                mapping.zero_at(shift, len).unwrap();

                let zeros = [0; LEN_MAX];
                assert_eq!(
                    buffer.0,
                    untouched_except(shift, &zeros[..len]),
                    "shift {shift}, len {len}"
                );
            }
        }
    }
    #[test]
    fn range_beyond_mapping_is_out_of_bounds() {
        let mut buffer = WordAlignedBuffer::untouched();
        let mapping = buffer.mapping();
        let mut target = [UNTOUCHED; 8];

        assert_eq!(
            mapping.copy_in_at(BUFFER_BYTES, &[1]),
            Err(Error::OutOfBounds)
        );
        assert_eq!(
            mapping.copy_in_at(BUFFER_BYTES - 4, &[1; 5]),
            Err(Error::OutOfBounds)
        );
        assert_eq!(
            mapping.copy_out_at(BUFFER_BYTES - 7, &mut target),
            Err(Error::OutOfBounds)
        );
        assert_eq!(
            mapping.copy_out_at(usize::MAX, &mut target[..1]),
            Err(Error::OutOfBounds)
        );
        assert_eq!(
            mapping.zero_at(0, BUFFER_BYTES + 1),
            Err(Error::OutOfBounds)
        );
        assert_eq!(mapping.zero_at(usize::MAX, 1), Err(Error::OutOfBounds));

        assert_eq!(buffer.0, [UNTOUCHED; BUFFER_BYTES]);
        assert_eq!(target, [UNTOUCHED; 8]);
    }
    #[test]
    fn range_ending_at_mapping_size_is_accepted() {
        let mut buffer = WordAlignedBuffer::untouched();
        let mapping = buffer.mapping();

        assert_eq!(mapping.copy_in_at(BUFFER_BYTES, &[]), Ok(()));
        assert_eq!(mapping.zero_at(0, BUFFER_BYTES), Ok(()));

        assert_eq!(buffer.0, [0; BUFFER_BYTES]);
    }
}
