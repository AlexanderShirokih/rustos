//! Сервис консольного вывода.

use io::writer::Writer;

/// Сервис консольного вывода.
///
/// Предоставляет доступ к Writer для записи логов и отладочной информации.
pub trait ConsoleService: Writer + Send + Sync {}

impl<T: ?Sized + Writer + Send + Sync> ConsoleService for T {}
