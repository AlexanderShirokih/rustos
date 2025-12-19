use memory::physical_address::AlignedPhysicalAddress;

pub trait Level {
    const SHIFT: u8;
}

pub enum L0 {}
pub enum L1 {}
pub enum L2 {}
pub enum L3 {}

impl Level for L0 {
    const SHIFT: u8 = 39;
}
impl Level for L1 {
    const SHIFT: u8 = 30;
}
impl Level for L2 {
    const SHIFT: u8 = 21;
}
impl Level for L3 {
    const SHIFT: u8 = 12;
}

pub type PagePa = AlignedPhysicalAddress<{ L3::SHIFT }>;
pub type L0BlockPa = AlignedPhysicalAddress<{ L0::SHIFT }>;
pub type L1BlockPa = AlignedPhysicalAddress<{ L1::SHIFT }>;
pub type L2BlockPa = AlignedPhysicalAddress<{ L2::SHIFT }>;
