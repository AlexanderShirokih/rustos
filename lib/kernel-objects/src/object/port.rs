//! Типизированная обёртка над rendezvous-эндпоинтом ядра и одноразовым reply.

use core::num::NonZeroU64;

use syscall::{
    IPC_BUFFER_DATA_MAX, IpcBuffer, Rights, SyscallError, Timeout, decode_tag, encode_tag, kind,
};
use syscall_user::ipc_buffer_ptr;

use crate::{
    error::{Error, Result, unit},
    handle::OwnedHandle,
    svc,
};

fn store_body(body: &[u8]) -> Result<()> {
    if body.len() > IPC_BUFFER_DATA_MAX {
        return Err(Error::Syscall(SyscallError::MessageTooBig));
    }
    let ptr = ipc_buffer_ptr().ok_or(Error::Syscall(SyscallError::WrongType))?;
    // SAFETY: ядро замаппило per-thread IPC-буфер user-RW по одной странице,
    // текущий поток - единственный, кто его трогает; запись в пределах полей.
    unsafe {
        let buf: &mut IpcBuffer = &mut *ptr;
        let target = buf
            .data
            .get_mut(..body.len())
            .ok_or(Error::Syscall(SyscallError::InvalidArgument))?;
        target.copy_from_slice(body);
        buf.tag = encode_tag(body.len(), 0);
    }
    Ok(())
}

fn load_body(out: &mut [u8]) -> Result<usize> {
    let ptr = ipc_buffer_ptr().ok_or(Error::Syscall(SyscallError::WrongType))?;
    // SAFETY: см. `store_body`; здесь только чтение полей буфера.
    unsafe {
        let buf: &IpcBuffer = &*ptr;
        let len = decode_tag(buf.tag).len;
        let copy = len.min(out.len()).min(IPC_BUFFER_DATA_MAX);
        let (Some(target), Some(source)) = (out.get_mut(..copy), buf.data.get(..copy)) else {
            return Err(Error::Syscall(SyscallError::InvalidArgument));
        };
        target.copy_from_slice(source);
        // Наружу уходит длина кадра целиком. В `out` при этом легло не больше его вместимости.
        Ok(len)
    }
}

/// Владеет хэндлом `Port` (synchronous rendezvous-IPC) и закрывает его на drop.
pub type Port = OwnedHandle<kind::Port>;

/// Владеет одноразовым reply-хэндлом, выданным `recv` встречного `call`.
pub type Reply = OwnedHandle<kind::Reply>;

/// Порт с разделёнными сторонами: корневая capability отдаётся серверу, у
/// создателя остаётся клиентский минт. Роль сторон различают их права.
#[derive(Debug)]
pub struct ServicePort {
    /// Серверная сторона - корневая capability, переносимая серверу целиком.
    /// Её уход лишает порт получателей.
    pub server: Port,
    /// Клиентская сторона: `send`/`call` под значком сессии.
    pub client: Port,
}

impl OwnedHandle<kind::Port> {
    /// Создаёт `Port` в текущей таблице.
    pub fn create() -> Result<Self> {
        svc::port_create().map(Self::from).map_err(Error::Syscall)
    }

    /// Создаёт порт и разделяет его стороны. Клиентская capability минтится как
    /// `WRITE | TRANSFER` со значком `badge`, серверной остаётся корневая
    /// capability. `None` даёт клиента без значка.
    pub fn create_service(badge: Option<NonZeroU64>) -> Result<ServicePort> {
        let port = Self::create()?;
        let client = port
            .borrow()
            .duplicate(Rights::WRITE | Rights::TRANSFER, badge)?;
        Ok(ServicePort {
            server: port,
            client,
        })
    }

    /// Отправляет сообщение из IPC-буфера текущего потока, блокируясь до
    /// `timeout`.
    pub fn send(&self, timeout: Timeout) -> Result<()> {
        unit(svc::port_send(self.as_raw(), timeout))
    }

    /// Принимает сообщение в IPC-буфер текущего потока, блокируясь до `timeout`.
    /// `Some(Reply)`, если встречный был `call`, иначе `None`. `SenderGone`:
    /// нет кадра, значок ушедшей стороны лежит в `IpcBuffer.badge`.
    pub fn recv(&self, timeout: Timeout) -> Result<Option<Reply>> {
        svc::port_recv(self.as_raw(), timeout)
            .map(|reply| reply.map(Reply::from))
            .map_err(Error::Syscall)
    }

    /// Отправляет сообщение из IPC-буфера текущего потока и получает ответ туда же.
    /// `timeout` ограничивает всю операцию.
    pub fn call(&self, timeout: Timeout) -> Result<()> {
        unit(svc::port_call(self.as_raw(), timeout))
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

impl OwnedHandle<kind::Reply> {
    /// Доставляет ответ из IPC-буфера сервера вызывателю. Ядро изымает reply-хэндл при
    /// любом исходе.
    pub fn reply(self) -> Result<()> {
        unit(svc::port_reply(self.into_issued()))
    }

    /// Отвечает телом без caps: пишет `body` в IPC-буфер и вызывает
    /// [`reply`](Self::reply). `body` длиннее [`IPC_BUFFER_DATA_MAX`] -
    /// `MessageTooBig`.
    pub fn reply_bytes(self, body: &[u8]) -> Result<()> {
        store_body(body)?;
        self.reply()
    }
}
