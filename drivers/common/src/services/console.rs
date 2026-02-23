//! Сервис консольного вывода.

use crate::services::Service;
use io::writer::Writer;

/// Сервис консольного вывода.
///
/// Предоставляет доступ к Writer для записи логов и отладочной информации.
pub trait ConsoleService: Service {
    /// Возвращает ссылку на writer для вывода.
    fn writer(&self) -> &(dyn Writer + Sync);
}
