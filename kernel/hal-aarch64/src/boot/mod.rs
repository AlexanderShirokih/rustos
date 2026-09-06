//! Путь загрузки от точки входа до `kmain`: фаза до включения MMU, фаза после него и
//! поддержанные протоколы загрузчика.

pub mod boot_early;
pub mod boot_primary;
pub mod protocol;
