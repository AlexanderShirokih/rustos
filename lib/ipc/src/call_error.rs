use crate::wire::IpcError;

/// Отказ вызова с ответом `Result<T, E>`. Его отдаёт метод `call` клиента
/// [`protocol!`](crate::protocol), если ответ операции объявлен буквально как `Result<T, E>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallError<E> {
    /// Доменная ошибка операции. Клиент `protocol!` получает её из ответа сервера.
    Remote(E),
    /// Отказ IPC: кадр вызова не записан, не доставлен либо ответ не прочитан.
    Ipc(IpcError),
}

impl<E> From<IpcError> for CallError<E> {
    fn from(error: IpcError) -> Self {
        Self::Ipc(error)
    }
}

impl<E> CallError<E> {
    /// Сводит отказ к одному значению: `ipc` получает отказ IPC, `remote` - доменную
    /// ошибку.
    pub fn fold<F>(self, ipc: impl FnOnce(IpcError) -> F, remote: impl FnOnce(E) -> F) -> F {
        match self {
            Self::Remote(error) => remote(error),
            Self::Ipc(error) => ipc(error),
        }
    }
}
