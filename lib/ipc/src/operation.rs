use core::{
    future::{Future, IntoFuture},
    marker::PhantomData,
};

use crate::{
    AsyncChannel, Channel, Incoming, Timeout, Undelivered,
    protocol::build_frame,
    wire::{Header, IpcError, MessageBuf},
};

/// Подготовленный вызов с ответом. Создание кодирует параметры, но не отправляет запрос.
/// `wait` использует синхронное рандеву, `.await` - асинхронный канал.
/// Разрушение до отправки закрывает capability параметров.
#[must_use = "a call must be awaited or waited"]
pub struct Call<'a, C: Channel, T, E = IpcError> {
    channel: &'a C,
    ordinal: u64,
    wait: Timeout,
    frame: Result<MessageBuf<C::Resources>, IpcError>,
    read: fn(u64, Incoming<'_, C::Resources>) -> Result<T, E>,
}

/// Подготовленная односторонняя отправка с выбором синхронного либо асинхронного ожидания.
/// До `wait` или `.await` кадр и его capability принадлежат операции.
#[must_use = "a send must be awaited or waited"]
pub struct Send<'a, C: Channel, V = (), E = IpcError> {
    channel: &'a C,
    wait: Timeout,
    frame: Result<MessageBuf<C::Resources>, IpcError>,
    read: fn(&[u8], &mut C::Resources) -> Result<V, IpcError>,
    error: PhantomData<fn() -> E>,
}

impl<'a, C: Channel, T, E: From<IpcError>> Call<'a, C, T, E> {
    #[doc(hidden)]
    pub fn __new(
        channel: &'a C,
        ordinal: u64,
        wait: Timeout,
        write: impl FnOnce(&mut MessageBuf<C::Resources>) -> Result<(), IpcError>,
        read: fn(u64, Incoming<'_, C::Resources>) -> Result<T, E>,
    ) -> Self {
        let mut call = Self {
            channel,
            ordinal,
            wait,
            frame: Ok(MessageBuf::new()),
            read,
        };
        prepare(&mut call.frame, ordinal, write);
        call
    }

    /// Блокирует поток до ответа через синхронный канал, без pending-операции и executor.
    pub fn wait(mut self) -> Result<T, E> {
        let frame = self.frame.as_mut().map_err(|error| *error)?;
        let answer = Channel::call(self.channel, frame.outgoing(), self.wait)?;
        (self.read)(self.ordinal, answer)
    }
}

impl<C: AsyncChannel, T, E: From<IpcError>> IntoFuture for Call<'_, C, T, E> {
    type Output = Result<T, E>;
    type IntoFuture = impl Future<Output = Self::Output>;

    fn into_future(mut self) -> Self::IntoFuture {
        async move {
            let frame = self.frame.as_mut().map_err(|error| *error)?;
            let answer = AsyncChannel::call(self.channel, frame.outgoing(), self.wait).await?;
            (self.read)(self.ordinal, answer)
        }
    }
}

impl<'a, C: Channel, V, E: From<Undelivered<V>>> Send<'a, C, V, E> {
    #[doc(hidden)]
    pub fn __new(
        channel: &'a C,
        ordinal: u64,
        wait: Timeout,
        write: impl FnOnce(&mut MessageBuf<C::Resources>) -> Result<(), IpcError>,
        read: fn(&[u8], &mut C::Resources) -> Result<V, IpcError>,
    ) -> Self {
        let mut send = Self {
            channel,
            wait,
            frame: Ok(MessageBuf::new()),
            read,
            error: PhantomData,
        };
        prepare(&mut send.frame, ordinal, write);
        send
    }

    /// Отправляет кадр синхронным каналом и возвращает владельцев capability при отказе.
    pub fn wait(mut self) -> Result<(), E> {
        let frame = self
            .frame
            .as_mut()
            .map_err(|error| Undelivered::unwritten(*error))?;
        Channel::send(self.channel, frame.outgoing(), self.wait)
            .map_err(|unsent| Undelivered::__new(unsent, self.read).into())
    }
}

impl<C: AsyncChannel, V, E: From<Undelivered<V>>> IntoFuture for Send<'_, C, V, E> {
    type Output = Result<(), E>;
    type IntoFuture = impl Future<Output = Self::Output>;

    fn into_future(mut self) -> Self::IntoFuture {
        async move {
            let frame = self
                .frame
                .as_mut()
                .map_err(|error| Undelivered::unwritten(*error))?;

            AsyncChannel::send(self.channel, frame.outgoing(), self.wait)
                .await
                .map_err(|unsent| Undelivered::__new(unsent, self.read).into())
        }
    }
}

fn prepare<R: crate::ReceivedResources>(
    frame: &mut Result<MessageBuf<R>, IpcError>,
    ordinal: u64,
    write: impl FnOnce(&mut MessageBuf<R>) -> Result<(), IpcError>,
) {
    if let Ok(buffer) = frame
        && let Err(error) = build_frame(buffer, Header::new(ordinal), write)
    {
        *frame = Err(error);
    }
}
