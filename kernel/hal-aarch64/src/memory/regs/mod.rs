//! Системные регистры AArch64 для управления памятью.

pub(crate) mod common;
pub(crate) mod dcache;
pub(crate) mod id_aa64mmfr0;
pub(crate) mod mair;
pub(crate) mod sctlr;
pub(crate) mod tcr;
pub(crate) mod tlb;
pub(crate) mod translate;
pub(crate) mod ttbr;
