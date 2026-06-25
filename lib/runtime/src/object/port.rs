//! Типизированная обёртка над rendezvous-эндпоинтом ядра и одноразовым reply.

use syscall::{
    Handle, IPC_BUFFER_DATA_MAX, IpcBuffer, SyscallError, Timeout, decode_tag, encode_tag,
};

use crate::{
    error::{Error, Result, unit, value},
    handle::{BorrowedHandle, OwnedHandle},
    ipc_buffer::ipc_buffer_ptr,
    svc,
};

/// Пишет тело без caps в IPC-буфер текущего потока: `tag = (len, 0)`,
/// `data[..len] = body`. `body` длиннее [`IPC_BUFFER_DATA_MAX`] - ошибка.
fn store_body(body: &[u8]) -> Result<()> {
    if body.len() > IPC_BUFFER_DATA_MAX {
        return Err(Error::Syscall(SyscallError::MessageTooBig));
    }
    let ptr = ipc_buffer_ptr().ok_or(Error::Syscall(SyscallError::WrongType))?;
    // SAFETY: ядро замаппило per-thread IPC-буфер user-RW по одной странице,
    // текущий поток - единственный, кто его трогает; запись в пределах полей.
    unsafe {
        let buf: &mut IpcBuffer = &mut *ptr;
        buf.data[..body.len()].copy_from_slice(body);
        buf.tag = encode_tag(body.len(), 0);
    }
    Ok(())
}

/// Читает тело из IPC-буфера: декодирует `len`, копирует `min(len, out.len())`
/// байт в `out`. Возвращает полный `len` (а не число скопированных байт).
fn load_body(out: &mut [u8]) -> Result<usize> {
    let ptr = ipc_buffer_ptr().ok_or(Error::Syscall(SyscallError::WrongType))?;
    // SAFETY: см. `store_body`; здесь только чтение полей буфера.
    unsafe {
        let buf: &IpcBuffer = &*ptr;
        let (len, _ncaps) = decode_tag(buf.tag);
        let copy = len.min(out.len()).min(IPC_BUFFER_DATA_MAX);
        out[..copy].copy_from_slice(&buf.data[..copy]);
        Ok(len)
    }
}

/// Владеет хэндлом `Port` (synchronous rendezvous-IPC) и закрывает его на drop.
#[derive(Debug)]
pub struct Port {
    handle: OwnedHandle,
}

/// Владеет одноразовым reply-хэндлом, выданным `recv` встречного `call`.
#[derive(Debug)]
pub struct Reply {
    handle: OwnedHandle,
}

impl Port {
    /// Создаёт `Port` в текущей таблице.
    pub fn create() -> Result<Self> {
        svc::port_create()
            // SAFETY: handle только что создан syscall'ом, мы единственный владелец.
            .map(|handle| Self::from_handle(unsafe { OwnedHandle::from_handle(handle) }))
            .map_err(Error::from_return)
    }

    /// Берёт во владение хэндл `Port`.
    pub fn from_handle(handle: OwnedHandle) -> Self {
        Self { handle }
    }

    /// Заимствование хэндла на время одного вызова.
    pub fn handle(&self) -> BorrowedHandle<'_> {
        self.handle.borrow()
    }

    /// Отправляет сообщение из IPC-буфера текущего потока, блокируясь до
    /// `timeout`.
    pub fn send(&self, timeout: Timeout) -> Result<()> {
        unit(svc::port_send(self.handle.as_raw(), timeout.raw()))
    }

    /// Принимает сообщение в IPC-буфер текущего потока, блокируясь до `timeout`.
    /// `Some(Reply)`, если встречный был `call`, иначе `None`.
    pub fn recv(&self, timeout: Timeout) -> Result<Option<Reply>> {
        let reply_raw = value(svc::port_recv(self.handle.as_raw(), timeout.raw()))?;
        #[allow(clippy::cast_possible_truncation)]
        let reply_id = reply_raw as u32;
        Ok(Handle::new(reply_id).map(|handle| Reply {
            // SAFETY: handle только что создан syscall'ом, мы единственный владелец.
            handle: unsafe { OwnedHandle::from_handle(handle) },
        }))
    }

    /// Запрос-ответ: сообщение из IPC-буфера текущего потока, ответ оказывается
    /// там же. `timeout` ограничивает всю операцию.
    pub fn call(&self, timeout: Timeout) -> Result<()> {
        unit(svc::port_call(self.handle.as_raw(), timeout.raw()))
    }

    /// Отдаёт владеемый хэндл.
    pub fn into_handle(self) -> OwnedHandle {
        self.handle
    }

    /// Шлёт тело без caps: пишет `body` в IPC-буфер и вызывает [`send`](Self::send).
    /// `body` длиннее [`IPC_BUFFER_DATA_MAX`] - `MessageTooBig`.
    pub fn send_bytes(&self, body: &[u8], timeout: Timeout) -> Result<()> {
        store_body(body)?;
        self.send(timeout)
    }

    /// Принимает тело без caps: [`recv`](Self::recv), затем читает тело в `out`.
    /// Возвращает полный `len` тела и `Some(Reply)`, если встречный был `call`.
    pub fn recv_bytes(&self, out: &mut [u8], timeout: Timeout) -> Result<(usize, Option<Reply>)> {
        let reply = self.recv(timeout)?;
        let len = load_body(out)?;
        Ok((len, reply))
    }

    /// Запрос-ответ телами без caps: пишет `body`, [`call`](Self::call), читает
    /// ответ в `out`. Возвращает полный `len` ответа.
    pub fn call_bytes(&self, body: &[u8], out: &mut [u8], timeout: Timeout) -> Result<usize> {
        store_body(body)?;
        self.call(timeout)?;
        load_body(out)
    }
}

impl Reply {
    /// Доставляет ответ из IPC-буфера сервера вызывателю. Ядро снимает reply-хэндл
    /// безусловно, поэтому повторный `Drop`-close подавлен.
    pub fn reply(self) -> Result<()> {
        unit(svc::port_reply(self.handle.into_raw()))
    }

    /// Отдаёт владеемый reply-хэндл.
    pub fn into_handle(self) -> OwnedHandle {
        self.handle
    }

    /// Отвечает телом без caps: пишет `body` в IPC-буфер и вызывает
    /// [`reply`](Self::reply). `body` длиннее [`IPC_BUFFER_DATA_MAX`] -
    /// `MessageTooBig`.
    pub fn reply_bytes(self, body: &[u8]) -> Result<()> {
        store_body(body)?;
        self.reply()
    }
}
