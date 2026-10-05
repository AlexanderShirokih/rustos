//! Путь загрузки от точки входа до `kmain`: фаза до включения MMU, фаза после него и
//! поддержанные протоколы загрузчика.

pub(crate) mod boot_early;
pub(crate) mod boot_primary;
pub(crate) mod protocol;
