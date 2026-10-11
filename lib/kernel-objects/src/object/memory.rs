//! Типизированные обёртки над метеринг-ресурсом, регионами памяти и их
//! маппингами.
//!
//! Страницы маппинга бывают отображены ещё и в чужом адресном пространстве, где их читают и
//! пишут параллельно. Доступ к маппингу идёт volatile-операциями. Компилятор не вправе ни
//! выбросить такую операцию, ни переупорядочить, ни склеить две соседние.
//!
//! Volatile-операция делит диапазон на три части. Голова до ближайшей границы слова и хвост
//! за последним целым словом передаются побайтно, середина - словами по `u64`. Каждый байт
//! при этом передаётся ровно один раз. Из другого адресного пространства середина наблюдается
//! пословно, голова и хвост - побайтно. Порядок трёх частей между собой ничем не закреплён.
//! Копия между маппингами передаётся словами, только если начала обоих диапазонов одинаково
//! смещены относительно границы слова. Иначе она передаётся побайтно целиком.
//!
//! Исключение - копия в маппинг профиля `WriteCombining` ([`WriteCombiningMapping::copy_from`]).
//! Она идёт обычной копией памяти. Ширину и число обращений выбирает реализация копии, а
//! компилятор вправе склеивать и переупорядочивать её операции. Байты источника, которые
//! чужое адресное пространство меняет во время такой копии, приходят в произвольной смеси
//! старых и новых значений.

use core::{
    borrow::Borrow,
    fmt, mem,
    ops::{Deref, Range},
};

const _: () = assert!(
    usize::BITS >= u64::BITS,
    "userland runtime targets 64-bit only"
);

use bootstrap_abi::DeviceWindow;
use io::mmio::Mmio;
use syscall::{
    MEMORY_REGION_CONTIGUOUS, MEMORY_REGION_WRITE_COMBINING, MemoryAccess, MemoryProfile,
    SyscallError, UserMemFlags, kind,
};

use crate::{
    error::{Error, Result, unit, value},
    handle::OwnedHandle,
    svc,
};

const fn budget_pages(budget_bytes: u64) -> u64 {
    budget_bytes.div_ceil(syscall::PAGE_SIZE as u64)
}

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

#[allow(clippy::multiple_unsafe_ops_per_block)]
unsafe fn copy_bytes(source: *const u8, target: *mut u8, start: usize, len: usize) {
    for offset in start..start + len {
        // SAFETY: вызывающий даёт диапазон `[start, start + len)`, проверенный `range_ptr`
        // у обоих маппингов. Указатели остаются внутри отображений, установленных ядром;
        // исходное отображение доступно для чтения, целевое - для записи.
        unsafe {
            let from = source.add(offset);
            let to = target.add(offset);
            let byte = from.read_volatile();
            to.write_volatile(byte);
        }
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
    /// Базовый PA `Physical`-региона (для корреляции с FDT-узлом) либо адрес первой страницы
    /// непрерывного `Virtual`-региона. У прочих `Virtual`-регионов поле равно `0`.
    pub base: u64,
    /// Backing региона.
    pub kind: RegionKind,
    /// Профиль работы с памятью региона.
    pub profile: MemoryProfile,
    /// Маска доступа региона.
    pub access: MemoryAccess,
}

/// Владеет хэндлом метеринг-`Resource` и закрывает его на drop.
pub type Resource = OwnedHandle<kind::Resource>;

/// Владеет хэндлом региона памяти и закрывает его на drop.
pub type MemoryRegion = OwnedHandle<kind::MemoryRegion>;

/// Страницы, отображённые в текущее AS: базовый VA и размер. Даёт операции чтения и записи
/// байтов отображения.
#[derive(Debug)]
pub struct MappedPages {
    va: u64,
    size_bytes: u64,
}

#[derive(Debug)]
struct MappedRange(MappedPages);

/// Отображение региона в текущее AS. Отображение держит регион `R` всё время своего
/// существования. Регион бывает собственным `MemoryRegion` или заимствованным `&MemoryRegion`.
/// Операции над байтами даёт [`MappedPages`], к которому отображение разыменовывается.
///
/// Сброс сначала снимает отображение, а затем отпускает регион. Собственный регион
/// закрывается, заимствованный остаётся у владельца.
#[derive(Debug)]
pub struct Mapping<R = MemoryRegion> {
    pages: MappedRange,
    region: R,
}

/// Отказ отображения вместе с регионом или окном, которое забрал конструктор. Возвращённый
/// регион открыт.
#[derive(Debug)]
pub struct MapError<R> {
    /// Причина отказа.
    pub error: Error,
    /// Регион или окно, переданные конструктору.
    pub region: R,
}

/// Окно устройства: регион, смещение начала устройства от начала региона и размер устройства в
/// байтах.
#[derive(Debug)]
pub struct MmioWindow {
    pub region: MemoryRegion,
    pub offset: u64,
    pub size: u64,
}

/// Отображение страниц устройства. Отображение держит окно всё время своего существования.
/// Страницы отображены через срез региона окна, который держит внутреннее отображение.
///
/// Сброс снимает страницы и закрывает срез до закрытия окна.
#[derive(Debug)]
pub struct MmioMapping {
    mapping: Mapping,
    window: MmioWindow,
    start: usize,
    head: usize,
    size: usize,
}

/// Маппинг страниц устройства с профилем [`MemoryProfile::WriteCombining`]. Массовая копия в
/// устройство идёт обычной копией памяти.
#[derive(Debug)]
pub struct WriteCombiningMapping {
    device: MmioMapping,
}

/// Отображение региона, чья маска доступа не допускает запись. Страницы неизменны во всех
/// адресных пространствах. Содержимое читается безопасным срезом. Держит регион `R`, как
/// [`Mapping`].
#[derive(Debug)]
pub struct ReadOnlyMapping<R = MemoryRegion> {
    mapping: Mapping<R>,
}

/// Анонимный маппинг (allocate-fastpath). Освобождается в `Drop` через
/// `memory_free`.
#[derive(Debug)]
pub struct AnonymousMapping {
    va: u64,
    size_bytes: u64,
}

impl OwnedHandle<kind::Resource> {
    /// Свежий handle на метеринг-`Resource` текущего процесса.
    pub fn self_resource() -> Result<Self> {
        let handle = svc::process_resource_self()?;
        Ok(Self::from(handle))
    }

    /// Отрезает от бюджета ресурса не меньше `budget_bytes` байт в отдельный
    /// `Resource` с тем же потолком доступа. Размер округляется вверх до целой
    /// страницы, и исходный ресурс теряет округлённый размер до ухода производного.
    /// Для нулевого `budget_bytes` возвращает `InvalidArgument`.
    pub fn split(&self, budget_bytes: u64) -> Result<Self> {
        svc::resource_split(self.as_raw(), budget_pages(budget_bytes))
            .map(Self::from)
            .map_err(Error::Syscall)
    }

    /// Создаёт регион с Virtual backing размера `size_bytes` и маской `access`.
    pub fn create_virtual(&self, size_bytes: u64, access: MemoryAccess) -> Result<MemoryRegion> {
        svc::memory_create_virtual(self.as_raw(), size_bytes, access.raw(), 0)
            .map(MemoryRegion::from)
            .map_err(Error::Syscall)
    }

    /// Создаёт регион с Virtual backing из физически смежных страниц. [`RegionInfo::base`]
    /// региона содержит адрес первой страницы. Без свободного смежного участка нужной длины
    /// возвращает `OutOfMemory`.
    pub fn create_contiguous(&self, size_bytes: u64, access: MemoryAccess) -> Result<MemoryRegion> {
        svc::memory_create_virtual(
            self.as_raw(),
            size_bytes,
            access.raw(),
            MEMORY_REGION_CONTIGUOUS,
        )
        .map(MemoryRegion::from)
        .map_err(Error::Syscall)
    }

    /// Выделяет анонимный регион размера `size_bytes` и сразу маппит его на
    /// свободный VA с флагами `flags`.
    pub fn allocate(&self, size_bytes: u64, flags: UserMemFlags) -> Result<AnonymousMapping> {
        let va = value(svc::memory_allocate(self.as_raw(), size_bytes, flags.raw()))?;
        Ok(AnonymousMapping { va, size_bytes })
    }
}

impl OwnedHandle<kind::MemoryRegion> {
    /// Деривация под-региона `[offset, offset+size_bytes)` с маской `access`
    /// (не шире текущей). Требует `DUPLICATE` на хэндле региона.
    pub fn slice(
        &self,
        offset: u64,
        size_bytes: u64,
        access: MemoryAccess,
    ) -> Result<MemoryRegion> {
        self.slice_with_flags(offset, size_bytes, access, 0)
    }

    fn slice_with_flags(
        &self,
        offset: u64,
        size_bytes: u64,
        access: MemoryAccess,
        region_flags: u64,
    ) -> Result<MemoryRegion> {
        svc::memory_slice(
            self.as_raw(),
            offset,
            size_bytes,
            access.raw(),
            region_flags,
        )
        .map(MemoryRegion::from)
        .map_err(Error::Syscall)
    }

    /// Декодирует сырой возврат `memory_region_inspect` в [`RegionInfo`].
    pub fn inspect(&self) -> Result<RegionInfo> {
        let (size, secondary) = svc::memory_region_inspect(self.as_raw());
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
}

impl DeviceWindow<syscall_user::IssuedHandle<kind::MemoryRegion>> for MemoryRegion {
    fn physical_range(&self) -> Option<Range<u64>> {
        let info = self.inspect().ok()?;
        Some(info.base..info.base.checked_add(info.size_bytes)?)
    }
}

impl MappedPages {
    /// Базовый VA маппинга.
    pub fn va(&self) -> u64 {
        self.va
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

    /// Копирует `len` байт маппинга со смещения `offset` в `destination` со смещения
    /// `destination_offset`, минуя промежуточный буфер. Диапазон сверх размера любого из
    /// маппингов - `OutOfBounds`. Диапазоны должны быть неперекрывающимися в памяти региона,
    /// в том числе при разных отображениях одного региона.
    pub fn copy_to(
        &self,
        offset: usize,
        destination: &MappedPages,
        destination_offset: usize,
        len: usize,
    ) -> Result<()> {
        let source = self.range_ptr(offset, len)?;
        let target = destination.range_ptr(destination_offset, len)?;
        let head = if source.addr() % WORD == target.addr() % WORD {
            head_len(source, len)
        } else {
            len
        };
        let words = (len - head) / WORD;
        let tail_start = head + words * WORD;
        // SAFETY: `[0, head)` лежит в диапазонах, проверенных `range_ptr`.
        unsafe { copy_bytes(source, target, 0, head) };
        for number in 0..words {
            let index = head + number * WORD;
            // SAFETY: слово отстоит от `head` на `number` слов и лежит в диапазоне, проверенном
            // `range_ptr`.
            let from = unsafe { word_ptr(source, index) };
            // SAFETY: слово лежит в диапазоне, проверенном `range_ptr`. Начала диапазонов
            // одинаково смещены относительно границы слова, поэтому адрес выровнен на слово.
            let to = unsafe { word_ptr(target, index) };
            // SAFETY: слово лежит в маппинге, установленном ядром.
            let word = unsafe { from.read_volatile() };
            // SAFETY: слово лежит в маппинге, установленном ядром.
            unsafe { to.write_volatile(word) };
        }
        // SAFETY: хвост `[tail_start, len)` лежит в диапазонах, проверенных `range_ptr`.
        unsafe { copy_bytes(source, target, tail_start, len - tail_start) };
        Ok(())
    }

    fn copy_plain_to(
        &self,
        offset: usize,
        destination: &MappedPages,
        destination_offset: usize,
        len: usize,
    ) -> Result<()> {
        let source = self.range_ptr(offset, len)?;
        let target = destination.range_ptr(destination_offset, len)?;
        let overlaps = source.addr() < target.addr() + len && target.addr() < source.addr() + len;
        if overlaps {
            return Err(Error::Syscall(SyscallError::InvalidArgument));
        }
        // SAFETY: оба диапазона проверены `range_ptr` и лежат в маппингах, установленных ядром на
        // время жизни отображений. Приёмник отображён на запись, потому что обычный
        // маппинг пишущего и маппинг устройства `map_pages` ставят с `UserMemFlags::ReadWrite`.
        // Перекрытие по виртуальным адресам исключено проверкой выше. Параллельная запись чужого
        // адресного пространства в источник даёт в приёмнике смесь старых и новых байт. Такая
        // смесь допустима, потому что у `u8` нет недопустимых значений, а код процесса
        // скопированные байты не читает и решений по ним не принимает.
        unsafe { core::ptr::copy_nonoverlapping(source, target, len) };
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

    /// Читает два байта маппинга со смещения `offset` одним обращением. Диапазон сверх размера
    /// маппинга либо нечётный адрес - `OutOfBounds`.
    pub fn read_u16_at(&self, offset: usize) -> Result<u16> {
        let target = self.half_word_ptr(offset)?;
        // SAFETY: `half_word_ptr` проверил, что два байта лежат в маппинге, установленном ядром,
        // и адрес выровнен на `u16`.
        Ok(unsafe { target.read_volatile() })
    }

    /// Записывает `value` в два байта маппинга со смещения `offset` одним обращением. Диапазон
    /// сверх размера маппинга либо нечётный адрес - `OutOfBounds`.
    pub fn write_u16_at(&self, offset: usize, value: u16) -> Result<()> {
        let target = self.half_word_ptr(offset)?;
        // SAFETY: `half_word_ptr` проверил, что два байта лежат в маппинге, установленном ядром,
        // и адрес выровнен на `u16`.
        unsafe { target.write_volatile(value) };
        Ok(())
    }

    fn half_word_ptr(&self, offset: usize) -> Result<*mut u16> {
        let base = self.range_ptr(offset, mem::size_of::<u16>())?;
        if base.addr() % mem::align_of::<u16>() == 0 {
            Ok(base.cast())
        } else {
            Err(Error::OutOfBounds)
        }
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
}

impl Deref for MappedRange {
    type Target = MappedPages;

    fn deref(&self) -> &MappedPages {
        &self.0
    }
}

impl Drop for MappedRange {
    fn drop(&mut self) {
        let _ = svc::memory_free(self.0.va, self.0.size_bytes);
    }
}

impl<R: Borrow<MemoryRegion>> Mapping<R> {
    /// Отображает `size_bytes` байт региона `region` на свободный VA с флагами `flags` и
    /// забирает регион. С `&MemoryRegion` отображение существует не дольше заимствования. Отказ
    /// `memory_map` возвращает регион в [`MapError`].
    pub fn new(
        region: R,
        size_bytes: u64,
        flags: UserMemFlags,
    ) -> core::result::Result<Self, MapError<R>> {
        let mapped = value(svc::memory_map(
            region.borrow().as_raw(),
            size_bytes,
            flags.raw(),
        ));
        match mapped {
            Ok(va) => Ok(Self {
                pages: MappedRange(MappedPages { va, size_bytes }),
                region,
            }),
            Err(error) => Err(MapError { error, region }),
        }
    }

    /// Отображает регион `region` целиком (размер из `inspect`) с флагами `flags`, как
    /// [`Mapping::new`]. Отказ `inspect` или `memory_map` возвращает регион в [`MapError`].
    pub fn full(region: R, flags: UserMemFlags) -> core::result::Result<Self, MapError<R>> {
        match region.borrow().inspect() {
            Ok(info) => Self::new(region, info.size_bytes, flags),
            Err(error) => Err(MapError { error, region }),
        }
    }

    /// Регион, который держит отображение.
    pub fn region(&self) -> &MemoryRegion {
        self.region.borrow()
    }

    /// Снимает отображение и возвращает регион открытым.
    pub fn into_region(self) -> R {
        let Self { pages, region } = self;
        drop(pages);
        region
    }

    /// Меняет флаги маппинга на `flags`.
    pub fn remap(&self, flags: UserMemFlags) -> Result<()> {
        unit(svc::memory_remap(self.va, self.size_bytes, flags.raw()))
    }
}

impl<R> Deref for Mapping<R> {
    type Target = MappedPages;

    fn deref(&self) -> &MappedPages {
        &self.pages
    }
}

impl<R> fmt::Display for MapError<R> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "mapping refused: {}", self.error)
    }
}

impl<R: fmt::Debug> core::error::Error for MapError<R> {}

impl<R> From<MapError<R>> for Error {
    fn from(value: MapError<R>) -> Self {
        value.error
    }
}

impl MmioMapping {
    /// Отображает страницы устройства из окна `window` на чтение и запись с профилем региона и
    /// забирает окно. Требует `READ`, `WRITE` и `DUPLICATE` на хэндле региона окна. Отказы
    /// возвращают окно в [`MapError`]:
    /// - `InvalidArgument` - устройство за концом региона;
    /// - ошибка `memory_slice` или `memory_map`.
    pub fn new(window: MmioWindow) -> core::result::Result<Self, MapError<MmioWindow>> {
        Self::with_region_flags(window, 0)
    }

    fn with_region_flags(
        window: MmioWindow,
        region_flags: u64,
    ) -> core::result::Result<Self, MapError<MmioWindow>> {
        match Self::map_pages(&window, region_flags) {
            Ok((mapping, start, head, size)) => Ok(Self {
                mapping,
                window,
                start,
                head,
                size,
            }),
            Err(error) => Err(MapError {
                error,
                region: window,
            }),
        }
    }

    fn map_pages(window: &MmioWindow, region_flags: u64) -> Result<(Mapping, usize, usize, usize)> {
        let invalid = || Error::Syscall(SyscallError::InvalidArgument);
        let page = syscall::PAGE_SIZE as u64;
        let first = window.offset - window.offset % page;
        let size_bytes = window
            .offset
            .checked_add(window.size)
            .and_then(|end| end.checked_next_multiple_of(page))
            .map(|end| end - first)
            .ok_or_else(invalid)?;
        let head = usize::try_from(window.offset - first).map_err(|_| invalid())?;
        let size = usize::try_from(window.size).map_err(|_| invalid())?;

        let pages =
            window
                .region
                .slice_with_flags(first, size_bytes, MemoryAccess::RW, region_flags)?;
        let mapping = Mapping::new(pages, size_bytes, UserMemFlags::ReadWrite)?;
        let start = usize::try_from(mapping.va())
            .ok()
            .and_then(|va| va.checked_add(head))
            .ok_or_else(invalid)?;
        Ok((mapping, start, head, size))
    }

    /// Окно устройства, которое держит отображение.
    pub fn window(&self) -> &MmioWindow {
        &self.window
    }

    /// Размер устройства в байтах.
    pub fn size(&self) -> usize {
        self.size
    }

    /// MMIO-хэндлер по началу устройства.
    pub fn mmio(&self) -> Mmio {
        Mmio::new(self.start)
    }

    /// Копирует в `dst` байты устройства со смещения `offset` от его начала операциями
    /// [`MappedPages::copy_out_at`]. Если диапазон выходит за границы области, выданной устройству,
    /// возвращает ошибку `OutOfBounds`.
    pub fn copy_out_at(&self, offset: usize, dst: &mut [u8]) -> Result<()> {
        let offset = self.device_offset(offset, dst.len())?;
        self.mapping.copy_out_at(offset, dst)
    }

    /// Копирует `src` в устройство со смещения `offset` от его начала операциями
    /// [`MappedPages::copy_in_at`]. Если диапазон выходит за границы области, выданной устройству,
    /// возвращает ошибку `OutOfBounds`.
    pub fn copy_in_at(&self, offset: usize, src: &[u8]) -> Result<()> {
        let offset = self.device_offset(offset, src.len())?;
        self.mapping.copy_in_at(offset, src)
    }

    fn device_offset(&self, offset: usize, len: usize) -> Result<usize> {
        let end = offset.checked_add(len).ok_or(Error::OutOfBounds)?;
        if end > self.size {
            return Err(Error::OutOfBounds);
        }
        Ok(self.head + offset)
    }
}

impl WriteCombiningMapping {
    /// Отображает страницы устройства из окна `window` на чтение и запись с профилем
    /// [`MemoryProfile::WriteCombining`] и забирает окно. Требует `READ`, `WRITE` и `DUPLICATE` на
    /// хэндле региона окна. Отказы возвращают окно в [`MapError`]:
    /// - `InvalidArgument` - регион профиля `Cached`;
    /// - `InvalidArgument` - устройство за концом региона;
    /// - ошибка `memory_slice` или `memory_map`.
    pub fn new(window: MmioWindow) -> core::result::Result<Self, MapError<MmioWindow>> {
        MmioMapping::with_region_flags(window, MEMORY_REGION_WRITE_COMBINING)
            .map(|device| Self { device })
    }

    /// Окно устройства, которое держит отображение.
    pub fn window(&self) -> &MmioWindow {
        self.device.window()
    }

    /// MMIO-хэндлер по началу устройства.
    pub fn mmio(&self) -> Mmio {
        self.device.mmio()
    }

    /// Копирует `src` в устройство со смещения `offset` от его начала, как
    /// [`MmioMapping::copy_in_at`].
    pub fn copy_in_at(&self, offset: usize, src: &[u8]) -> Result<()> {
        self.device.copy_in_at(offset, src)
    }

    /// Копирует `len` байт маппинга `source` со смещения `source_offset` в устройство со смещения
    /// `offset` от его начала обычной копией памяти. Если диапазон выходит за границы области,
    /// выданной устройству, или за конец `source`, возвращает `OutOfBounds` без копирования.
    ///
    /// `source` - маппинг обычной памяти профиля `Cached`, физически не перекрытый с
    /// устройством. Маппинг устройства в роли источника копия читает обращениями той ширины и в
    /// том числе, которые выбирает реализация копии. Источник с теми же физическими страницами,
    /// что и устройство, даёт на устройстве произвольные байты.
    pub fn copy_from(
        &self,
        offset: usize,
        source: &MappedPages,
        source_offset: usize,
        len: usize,
    ) -> Result<()> {
        let offset = self.device.device_offset(offset, len)?;
        source.copy_plain_to(source_offset, &self.device.mapping, offset, len)
    }
}

impl<R: Borrow<MemoryRegion>> ReadOnlyMapping<R> {
    /// Отображает регион `region` целиком на чтение, удостоверяя неизменность его страниц, и
    /// забирает регион, как [`Mapping::new`]. Отказы возвращают регион в [`MapError`]:
    /// - `AccessDenied` - маска доступа региона допускает запись;
    /// - ошибка `inspect` или `memory_map`.
    pub fn new(region: R) -> core::result::Result<Self, MapError<R>> {
        let info = match region.borrow().inspect() {
            Ok(info) => info,
            Err(error) => return Err(MapError { error, region }),
        };

        if info.access.raw() & MemoryAccess::WRITE.raw() != 0 {
            return Err(MapError {
                error: Error::Syscall(SyscallError::AccessDenied),
                region,
            });
        }

        Mapping::new(region, info.size_bytes, UserMemFlags::ReadOnly)
            .map(|mapping| Self { mapping })
    }

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

    /// Регион, который держит отображение.
    pub fn region(&self) -> &MemoryRegion {
        self.mapping.region()
    }

    /// Снимает отображение и возвращает регион открытым.
    pub fn into_region(self) -> R {
        self.mapping.into_region()
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

        fn mapping(&mut self) -> MappedPages {
            let va = u64::try_from(self.base().expose_provenance()).unwrap();
            let size_bytes = u64::try_from(BUFFER_BYTES).unwrap();
            MappedPages { va, size_bytes }
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

        let heads = [2, 5, 6].map(|len| head_len(base, len));

        assert_eq!(heads, [2, 5, 5]);
    }

    #[test]
    fn copy_in_at_writes_exactly_the_range() {
        let pattern = source_bytes();

        for shift in SHIFTS {
            for len in lens() {
                let mut buffer = WordAlignedBuffer::untouched();
                let mapping = buffer.mapping();
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
    fn copy_to_moves_exactly_the_range_between_mappings() {
        let pattern = source_bytes();

        for source_shift in SHIFTS {
            for target_shift in SHIFTS {
                for len in lens() {
                    let mut source = WordAlignedBuffer::untouched();
                    let mut target = WordAlignedBuffer::untouched();
                    let source_mapping = source.mapping();
                    let target_mapping = target.mapping();
                    let bytes = &pattern[1..=len];
                    source_mapping.copy_in_at(source_shift, bytes).unwrap();

                    source_mapping
                        .copy_to(source_shift, &target_mapping, target_shift, len)
                        .unwrap();

                    assert_eq!(
                        target.0,
                        untouched_except(target_shift, bytes),
                        "source shift {source_shift}, target shift {target_shift}, len {len}"
                    );
                }
            }
        }
    }

    #[test]
    fn copy_plain_to_moves_exactly_the_range_between_mappings() {
        let pattern = source_bytes();
        let mut source = WordAlignedBuffer::untouched();
        let mut target = WordAlignedBuffer::untouched();
        let source_mapping = source.mapping();
        let target_mapping = target.mapping();
        let bytes = &pattern[1..=LEN_MAX];
        source_mapping.copy_in_at(3, bytes).unwrap();

        source_mapping
            .copy_plain_to(3, &target_mapping, 5, LEN_MAX)
            .unwrap();

        assert_eq!(target.0, untouched_except(5, bytes));
    }

    #[test]
    fn copy_plain_to_within_one_mapping_refuses_overlapping_ranges() {
        let mut buffer = WordAlignedBuffer::untouched();
        let mapping = buffer.mapping();

        let overlapping = mapping.copy_plain_to(0, &mapping, 1, 8);

        assert_eq!(
            overlapping,
            Err(Error::Syscall(SyscallError::InvalidArgument))
        );
        assert_eq!(buffer.0, [UNTOUCHED; BUFFER_BYTES]);
    }

    #[test]
    fn copy_plain_to_past_either_end_is_out_of_bounds_without_copy() {
        let mut source = WordAlignedBuffer::untouched();
        let mut target = WordAlignedBuffer::untouched();
        let source_mapping = source.mapping();
        let target_mapping = target.mapping();
        source_mapping.copy_in_at(0, &[1; BUFFER_BYTES]).unwrap();

        let past_source = source_mapping.copy_plain_to(1, &target_mapping, 0, BUFFER_BYTES);
        let past_target = source_mapping.copy_plain_to(0, &target_mapping, 1, BUFFER_BYTES);

        assert_eq!(past_source, Err(Error::OutOfBounds));
        assert_eq!(past_target, Err(Error::OutOfBounds));
        assert_eq!(target.0, [UNTOUCHED; BUFFER_BYTES]);
    }

    #[test]
    fn zero_at_clears_exactly_the_range() {
        let zeros = [0; LEN_MAX];

        for shift in SHIFTS {
            for len in lens() {
                let mut buffer = WordAlignedBuffer::untouched();
                let mapping = buffer.mapping();

                mapping.zero_at(shift, len).unwrap();

                assert_eq!(
                    buffer.0,
                    untouched_except(shift, &zeros[..len]),
                    "shift {shift}, len {len}"
                );
            }
        }
    }

    #[test]
    fn half_word_round_trips_at_an_even_offset_and_odd_one_is_refused() {
        let mut buffer = WordAlignedBuffer::untouched();
        let mapping = buffer.mapping();

        mapping.write_u16_at(2, 0x1234).unwrap();
        let read = mapping.read_u16_at(2);
        let odd = mapping.write_u16_at(3, 0);
        let past_end = mapping.read_u16_at(BUFFER_BYTES - 1);

        assert_eq!(read, Ok(0x1234));
        assert_eq!(odd, Err(Error::OutOfBounds));
        assert_eq!(past_end, Err(Error::OutOfBounds));
        assert_eq!(buffer.0, untouched_except(2, &0x1234u16.to_ne_bytes()));
    }

    #[test]
    fn range_beyond_mapping_is_out_of_bounds() {
        let mut buffer = WordAlignedBuffer::untouched();
        let mapping = buffer.mapping();
        let mut target = [UNTOUCHED; 8];

        let results = [
            mapping.copy_in_at(BUFFER_BYTES, &[1]),
            mapping.copy_in_at(BUFFER_BYTES - 4, &[1; 5]),
            mapping.copy_out_at(BUFFER_BYTES - 7, &mut target),
            mapping.copy_out_at(usize::MAX, &mut target[..1]),
            mapping.zero_at(0, BUFFER_BYTES + 1),
            mapping.zero_at(usize::MAX, 1),
            mapping.copy_to(BUFFER_BYTES - 4, &mapping, 0, 5),
            mapping.copy_to(0, &mapping, BUFFER_BYTES - 4, 5),
        ];

        assert_eq!(results, [Err(Error::OutOfBounds); 8]);
        assert_eq!(buffer.0, [UNTOUCHED; BUFFER_BYTES]);
        assert_eq!(target, [UNTOUCHED; 8]);
    }

    #[test]
    fn range_ending_at_mapping_size_is_accepted() {
        let mut buffer = WordAlignedBuffer::untouched();
        let mapping = buffer.mapping();

        let copied = mapping.copy_in_at(BUFFER_BYTES, &[]);
        let zeroed = mapping.zero_at(0, BUFFER_BYTES);

        assert_eq!(copied, Ok(()));
        assert_eq!(zeroed, Ok(()));
        assert_eq!(buffer.0, [0; BUFFER_BYTES]);
    }

    #[test]
    fn a_budget_rounds_up_to_whole_pages() {
        let page = syscall::PAGE_SIZE as u64;

        let pages = [0, 1, page, page + 1].map(budget_pages);

        assert_eq!(pages, [0, 1, 1, 2]);
    }

    #[test]
    fn the_largest_budget_converts_without_overflow() {
        let page = syscall::PAGE_SIZE as u64;

        let pages = budget_pages(u64::MAX);

        assert_eq!(pages, u64::MAX / page + 1);
    }
}
