//! Записи таблицы страниц AArch64.

use crate::level::{L0, L1, L1BlockPa, L2, L2BlockPa, L3, Level, PagePa};
use crate::mem_flags::Aarch64MemFlags;
use crate::table_flags::TableFlags;
use core::marker::PhantomData;
use memory::aligned::Address;

/// Sealed traits для ограничения типов записей на каждом уровне.
mod seal {
    /// Уровень поддерживает запись-таблицу.
    pub trait CanTable {}
    /// Уровень поддерживает блочную запись.
    pub trait CanBlock: CanTable {}
    /// Уровень поддерживает страничную запись.
    pub trait CanPage {}
}

pub trait CanTable: seal::CanTable {}
impl<L: seal::CanTable> CanTable for L {}

impl seal::CanTable for L0 {}
impl seal::CanTable for L1 {}
impl seal::CanTable for L2 {}

impl seal::CanBlock for L1 {}
impl seal::CanBlock for L2 {}
impl seal::CanPage for L3 {}

/// Маркер типа записи.
pub trait Kind {}

/// Невалидная запись (биты [1:0] = 0b00).
pub enum Invalid {}
/// Запись-указатель на следующий уровень таблицы.
pub enum Table {}
/// Блочная запись (L1: 1 ГБ, L2: 2 МБ).
pub enum Block {}
/// Страничная запись (4 КБ, только L3).
pub enum Page {}

impl Kind for Invalid {}
impl Kind for Table {}
impl Kind for Block {}
impl Kind for Page {}

/// Запись таблицы страниц уровня `L` с типом `K`.
///
/// Типизирована по уровню и виду записи для compile-time гарантий корректности.
#[repr(transparent)]
pub struct Entry<L: Level, K: Kind> {
    /// Сырое 64-битное значение дескриптора.
    raw: u64,
    _p: PhantomData<(L, K)>,
}

impl<L: Level, K: Kind> Entry<L, K> {
    pub const fn raw(&self) -> u64 {
        self.raw
    }
}

impl<L: Level> Entry<L, Invalid> {
    pub const fn invalid() -> Self {
        Self::from_raw_unchecked(0)
    }

    pub const fn from_raw_unchecked(raw: u64) -> Self {
        Self {
            raw,
            _p: PhantomData,
        }
    }
}

impl<L: Level + seal::CanTable> Entry<L, Table> {
    pub fn new(next_table: PagePa, flags: TableFlags) -> Self {
        let addr = next_table.as_u64() & 0x0000_FFFF_FFFF_F000;
        Self::from_raw_unchecked(0b11 | addr | flags.bits())
    }

    pub const fn from_raw_unchecked(raw: u64) -> Self {
        Self {
            raw,
            _p: PhantomData,
        }
    }
}

impl<L: Level + seal::CanBlock> Entry<L, Block> {
    pub const fn from_raw_unchecked(raw: u64) -> Self {
        Self {
            raw,
            _p: PhantomData,
        }
    }
}

impl Entry<L1, Block> {
    pub fn new(block_pa: L1BlockPa, flags: Aarch64MemFlags) -> Self {
        let addr = block_pa.as_u64() & 0x0000_FFFF_C000_0000;
        Self::from_raw_unchecked(0b01 | addr | flags.bits())
    }
}

impl Entry<L2, Block> {
    pub fn new(block_pa: L2BlockPa, flags: Aarch64MemFlags) -> Self {
        let addr = block_pa.as_u64() & 0x0000_FFFF_FFE0_0000;
        Self::from_raw_unchecked(0b01 | addr | flags.bits())
    }
}

impl<L: Level + seal::CanPage> Entry<L, Page> {
    pub fn new(page_pa: PagePa, flags: Aarch64MemFlags) -> Self {
        let addr = page_pa.as_u64() & 0x0000_FFFF_FFFF_F000;
        let raw = 0b11 | addr | flags.bits();

        Self::from_raw_unchecked(raw)
    }

    pub const fn from_raw_unchecked(raw: u64) -> Self {
        Self {
            raw,
            _p: PhantomData,
        }
    }
}

/// Декодирует дескриптор с битами [1:0] = 0b01.
///
/// L0 не поддерживает блоки, L1/L2 - поддерживают.
pub trait DecodeBlock: Level {
    fn decode_block(raw: u64) -> Result<AnyEntry<Self>, DecodeError>
    where
        Self: Sized;
}

impl DecodeBlock for L0 {
    fn decode_block(_: u64) -> Result<AnyEntry<Self>, DecodeError> {
        Err(DecodeError::WrongKind)
    }
}

impl DecodeBlock for L1 {
    fn decode_block(raw: u64) -> Result<AnyEntry<Self>, DecodeError> {
        Ok(AnyEntry::Block(Entry::<L1, Block>::from_raw_unchecked(raw)))
    }
}

impl DecodeBlock for L2 {
    fn decode_block(raw: u64) -> Result<AnyEntry<Self>, DecodeError> {
        Ok(AnyEntry::Block(Entry::<L2, Block>::from_raw_unchecked(raw)))
    }
}

/// Ошибка декодирования записи.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// Зарезервированное значение дескриптора.
    Reserved,
    /// Тип записи недопустим для данного уровня.
    WrongKind,
}

/// Декодированная запись произвольного типа.
pub enum AnyEntry<L: Level> {
    /// Невалидная запись.
    Invalid(Entry<L, Invalid>),
    /// Указатель на дочернюю таблицу.
    Table(Entry<L, Table>),
    /// Блочный маппинг.
    Block(Entry<L, Block>),
    /// Страничный маппинг (только L3).
    Page(Entry<L3, Page>),
}

impl<L: Level> AnyEntry<L> {
    pub fn raw(&self) -> u64 {
        match self {
            AnyEntry::Invalid(e) => e.raw(),
            AnyEntry::Table(e) => e.raw(),
            AnyEntry::Block(e) => e.raw(),
            AnyEntry::Page(e) => e.raw(),
        }
    }
}

/// Извлекает тип дескриптора (биты [1:0]).
#[inline]
fn desc_type(raw: u64) -> u64 {
    raw & 0b11
}

/// Декодирует сырое значение в типизированную запись.
pub fn decode<L>(raw: u64) -> Result<AnyEntry<L>, DecodeError>
where
    L: Level + seal::CanTable + DecodeBlock,
{
    match desc_type(raw) {
        0b00 => Ok(AnyEntry::Invalid(Entry::<L, Invalid>::from_raw_unchecked(
            raw,
        ))),

        0b11 => Ok(AnyEntry::Table(Entry::<L, Table>::from_raw_unchecked(raw))),

        0b01 => DecodeBlock::decode_block(raw),

        _ => Err(DecodeError::Reserved),
    }
}
