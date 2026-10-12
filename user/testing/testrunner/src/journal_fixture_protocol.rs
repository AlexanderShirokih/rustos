//! Строки, общие для `journal-subscriber-fixture` и её сценариев. Это имя наблюдаемой записи,
//! строка готовности, строка конца отчёта и отметки в строке отчёта.

pub(crate) const WATCHED_SOURCE: &str = "journal-watched";

pub(crate) const READY_LINE: &str = "history read";

pub(crate) const END_SOURCE: &str = "journal-end";

pub(crate) const END_LINE: &str = "report";

pub(crate) const KERNEL_MARK: &str = "kernel: ";

pub(crate) const GAP_MARK: &str = "gap";

pub(crate) const TIME_MARK: &str = "time went back";

pub(crate) const REPORT_SEPARATOR: &str = " | ";
