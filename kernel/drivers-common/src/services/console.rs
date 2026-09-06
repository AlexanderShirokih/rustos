//! Сервис консольного вывода: контракт для драйвера консоли, публикуемый в `BootServices`.

use io::writer::Writer;

/// Трейт-объект, который драйвер консоли кладёт в `BootServices` через `set_console`.
///
/// Реализуется любым типом с `Writer + Send + Sync` - явной реализации не требуется.
pub trait ConsoleService: Writer + Send + Sync {}

impl<T: ?Sized + Writer + Send + Sync> ConsoleService for T {}
