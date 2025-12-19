use crate::level::{L0, L1, L1BlockPa, L2, L2BlockPa, L3, Level, PagePa};
use crate::mem_flags::MemFlags;
use crate::table_flags::TableFlags;
use core::marker::PhantomData;
use memory::aligned::Address;

mod seal {
    pub trait CanTable {}
    pub trait CanBlock: CanTable {}
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

pub trait Kind {}

pub enum Invalid {}
pub enum Table {}
pub enum Block {}
pub enum Page {}

impl Kind for Invalid {}
impl Kind for Table {}
impl Kind for Block {}
impl Kind for Page {}

#[repr(transparent)]
pub struct Entry<L: Level, K: Kind> {
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
    pub fn new(block_pa: L1BlockPa, flags: MemFlags) -> Self {
        let addr = block_pa.as_u64() & 0x0000_FFFF_C000_0000;
        Self::from_raw_unchecked(0b01 | addr | flags.bits())
    }
}

impl Entry<L2, Block> {
    pub fn new(block_pa: L2BlockPa, flags: MemFlags) -> Self {
        let addr = block_pa.as_u64() & 0x0000_FFFF_FFE0_0000;
        Self::from_raw_unchecked(0b01 | addr | flags.bits())
    }
}

impl<L: Level + seal::CanPage> Entry<L, Page> {
    pub fn new(page_pa: PagePa, flags: MemFlags) -> Self {
        // bits[1:0] = 0b11 (page) для L3
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    Reserved,
    WrongKind,
}

pub enum AnyEntry<L: Level> {
    Invalid(Entry<L, Invalid>),
    Table(Entry<L, Table>),
    Block(Entry<L, Block>),
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

#[inline]
fn desc_type(raw: u64) -> u64 {
    raw & 0b11
}

pub fn decode<L: Level + seal::CanTable>(raw: u64) -> Result<AnyEntry<L>, DecodeError> {
    match desc_type(raw) {
        0b00 => Ok(AnyEntry::Invalid(Entry::<L, Invalid>::from_raw_unchecked(
            raw,
        ))),

        0b11 => Ok(AnyEntry::Table(Entry::<L, Table>::from_raw_unchecked(raw))),

        0b10 | _ => Err(DecodeError::Reserved),
    }
}
