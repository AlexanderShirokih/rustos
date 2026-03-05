//! Сервис консольного вывода.

use io::writer::Writer;

use crate::services::Service;

/// Сервис консольного вывода.
///
/// Предоставляет доступ к Writer для записи логов и отладочной информации.
pub trait ConsoleService: Service {
    /// Возвращает ссылку на writer для вывода.
    fn writer(&self) -> &(dyn Writer + Sync);
}
