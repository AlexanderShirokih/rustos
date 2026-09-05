//! Транспорт ipc-контрактов поверх синхронного `Port` + per-thread
//! IPC-буфер.
//!
//! `PortTransport` использует рандеву-примитив ядра (`port_send`/`recv`/`call`/`reply`):
//! сообщение всегда лежит в per-thread IPC-буфере текущего потока, а сами syscalls блокируют до
//! встречи.
//! Поскольку методы [`Transport`] принимают `&self`, а семантика
//! клиента и сервера различается, роль задаётся явно при конструировании
//! ([`PortTransport::client`] / [`PortTransport::server`]).
//!
//! Тайм-аут типизированного IPC доступен через `wait_readable(timeout)`:
//! у клиента он ограничивает `port_call`, у сервера - приём (`port_recv`)
//! очередного кадра (принятый кадр стейджится для следующего `read_message`).
//! На истечении возвращается `Timeout`. Прямой серверный `read_message` без
//! предшествующего `wait_readable` блокируется бессрочно.
//! One-way отправка отдельно настраивается через
//! [`PortTransport::with_send_timeout`].
//!
//! Ресурсы кадра транспорт отдаёт только после его доставки. [`Delivery`] возникает на
//! состоявшемся рандеву и достаётся один раз, а отказ приёма прав на прежнее содержимое
//! буфера не даёт. Владение этими ресурсами принимает `kernel_objects::ReceivedFrame` на
//! серверном приёме, [`Transport::take_resources`] на клиентском ответе и
//! `runtime::receive_startup` на стартовом кадре. Невзятую доставку транспорт сам
//! закрывает.

use core::cell::Cell;

use ipc::{
    MessageLen, Transport,
    wire::{Header, IpcError, MESSAGE_MAX_HANDLES},
};
use syscall::{
    Handle, IPC_BUFFER_DATA_MAX, IPC_BUFFER_MAX_CAPS, SYSCALL_RETURN_TIMEOUT, SyscallError,
    Timeout, decode_tag, encode_tag,
};

use crate::{
    Delivery,
    ipc_buffer::ipc_buffer_ptr,
    svc::{handle_close, port_call, port_recv, port_reply, port_send},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    ClientIdle,
    ClientCallPending,
    ClientResponseReady,
    ServerIdle,
    ServerStaged,
}

impl Phase {
    const fn is_server(self) -> bool {
        matches!(self, Self::ServerIdle | Self::ServerStaged)
    }
}

/// Порт ipc-контракта поверх синхронного port.
pub struct PortTransport {
    handle: Handle,
    phase: Cell<Phase>,
    pending_reply: Cell<Option<Handle>>,
    delivered: Cell<Option<Delivery>>,
    send_timeout: Timeout,
}

impl PortTransport {
    /// Клиентская сторона порта: отправитель `cast`/`call`.
    #[must_use]
    pub fn client(handle: impl Into<Handle>) -> Self {
        Self::with_phase(handle.into(), Phase::ClientIdle)
    }

    /// Серверная сторона порта: получатель `recv`, отвечающий `reply`.
    #[must_use]
    pub fn server(handle: impl Into<Handle>) -> Self {
        Self::with_phase(handle.into(), Phase::ServerIdle)
    }

    fn with_phase(handle: Handle, phase: Phase) -> Self {
        Self {
            handle,
            phase: Cell::new(phase),
            pending_reply: Cell::new(None),
            delivered: Cell::new(None),
            send_timeout: Timeout::INFINITE,
        }
    }

    /// Задаёт тайм-аут rendezvous для one-way `send`/`#[cast]`.
    /// Two-way `call`, server receive и reply используют свои сроки.
    #[must_use]
    pub const fn with_send_timeout(mut self, send_timeout: Timeout) -> Self {
        self.send_timeout = send_timeout;
        self
    }

    /// Текущий тайм-аут one-way отправки.
    #[must_use]
    pub const fn send_timeout(&self) -> Timeout {
        self.send_timeout
    }

    /// Значок (badge) ушедшей стороны при отказе приёма `SenderGone`. Ядро положило в буфер
    /// значок, чьи отправители исчезли, а кадр не доставило. Значок доставленного кадра
    /// отдаёт [`Delivery::badge`].
    ///
    /// Значение действительно до следующей операции над IPC-буфером текущего потока. Любой
    /// `port_send`/`port_call`/`port_recv` перезаписывает это поле.
    #[must_use]
    pub fn peer_badge(&self) -> u64 {
        // SAFETY: см. `load_buffer`. Здесь только чтение поля буфера.
        ipc_buffer_ptr().map_or(0, |ptr| unsafe { (*ptr).badge })
    }

    /// Передаёт владение ресурсами доставленного кадра. Кадр отдаётся один раз,
    /// и приём, не дошедший до доставки, кадра не оставляет.
    ///
    /// Невзятую доставку транспорт закрывает до следующей операции либо при уничтожении.
    #[must_use]
    pub fn take_delivery(&self) -> Option<Delivery> {
        self.delivered.take()
    }

    /// Закрывает незавершённый reply названного кадра и будит вызывателя отказом вместо
    /// бессрочной парковки. No-op, когда ответ уже отправлен, встречный был `cast` либо
    /// reply принадлежит другому кадру.
    pub fn finish_reply(&self, reply: Option<Handle>) {
        let Some(reply) = reply else {
            return;
        };
        if self.pending_reply.get() != Some(reply) {
            return;
        }
        self.pending_reply.set(None);
        let _ = handle_close(reply);
    }

    fn record_delivery(&self, reply: Option<Handle>) {
        let Some(ptr) = ipc_buffer_ptr() else {
            return;
        };
        // SAFETY: см. `load_buffer`. Здесь только чтение полей буфера.
        let (badge, caps) = unsafe {
            let buf = &*ptr;
            let ncaps = decode_tag(buf.tag).ncaps.min(IPC_BUFFER_MAX_CAPS);
            let caps = core::array::from_fn(|index| {
                (index < ncaps)
                    .then(|| buf.caps.get(index).copied().and_then(Handle::new))
                    .flatten()
            });
            (buf.badge, caps)
        };
        self.delivered.set(Some(Delivery::new(badge, caps, reply)));
    }

    fn discard_pending_reply(&self) {
        if let Some(reply) = self.pending_reply.take() {
            let _ = handle_close(reply);
        }
    }

    fn recv_into_buffer(&self, timeout: Timeout) -> Result<(), IpcError> {
        drop(self.delivered.take());
        // Предыдущий запрос завершён без ответа до ожидания следующего отправителя.
        self.discard_pending_reply();
        let ret = port_recv(self.handle, timeout);
        if ret < 0 {
            return Err(map_recv_error(ret));
        }
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let reply = Handle::new(ret as u32);
        self.pending_reply.set(reply);
        self.record_delivery(reply);
        Ok(())
    }

    fn store_buffer(bytes: &[u8], handles: &[u32]) -> Result<(), IpcError> {
        if bytes.len() > IPC_BUFFER_DATA_MAX {
            return Err(IpcError::FrameOverflow);
        }
        if handles.len() > IPC_BUFFER_MAX_CAPS {
            return Err(IpcError::FrameOverflow);
        }
        let ptr = ipc_buffer_ptr().ok_or(IpcError::PeerClosed)?;
        // SAFETY: ядро замаппило per-thread IPC-буфер user-RW по одной
        // странице; `ptr` валиден и эксклюзивен для текущего потока, запись
        // в пределах полей структуры.
        unsafe {
            let buf = &mut *ptr;
            let Some(target) = buf.data.get_mut(..bytes.len()) else {
                return Err(IpcError::Truncated);
            };
            target.copy_from_slice(bytes);
            for (slot, &raw) in buf.caps.iter_mut().zip(handles) {
                *slot = raw;
            }
            buf.tag = encode_tag(bytes.len(), handles.len());
        }
        Ok(())
    }

    fn load_buffer(bytes: &mut [u8], handles: &mut [u32]) -> Result<MessageLen, IpcError> {
        let ptr = ipc_buffer_ptr().ok_or(IpcError::PeerClosed)?;
        // SAFETY: см. `store_buffer`; здесь только чтение полей буфера.
        let (len, ncaps) = unsafe {
            let buf = &*ptr;
            let tag = decode_tag(buf.tag);
            let len = tag.len.min(IPC_BUFFER_DATA_MAX);
            let ncaps = tag.ncaps.min(IPC_BUFFER_MAX_CAPS);
            if len > bytes.len() || ncaps > handles.len() {
                return Err(IpcError::Truncated);
            }
            let (Some(target), Some(source)) = (bytes.get_mut(..len), buf.data.get(..len)) else {
                return Err(IpcError::Truncated);
            };
            target.copy_from_slice(source);
            let Some(caps) = buf.caps.get(..ncaps) else {
                return Err(IpcError::Truncated);
            };
            for (dst, &raw) in handles.iter_mut().zip(caps) {
                *dst = raw;
            }
            (len, ncaps)
        };
        Ok(MessageLen::new(len, ncaps))
    }
}

fn map_recv_error(ret: i64) -> IpcError {
    match SyscallError::from_return(ret) {
        Some(SyscallError::Timeout) => IpcError::Timeout,
        Some(SyscallError::SenderGone) => IpcError::SenderGone,
        Some(SyscallError::OutOfHandles | SyscallError::ResourceExhausted) => {
            IpcError::ResourceExhausted
        }
        _ => IpcError::PeerClosed,
    }
}

fn map_send_result(ret: i64) -> Result<(), IpcError> {
    match SyscallError::from_return(ret) {
        None if ret == 0 => Ok(()),
        Some(SyscallError::Timeout) => Err(IpcError::Timeout),
        _ => Err(IpcError::PeerClosed),
    }
}

impl Drop for PortTransport {
    fn drop(&mut self) {
        self.discard_pending_reply();
    }
}

impl Transport for PortTransport {
    type Resources = Delivery;

    fn take_resources(&self) -> Delivery {
        self.take_delivery().unwrap_or_default()
    }

    fn write_message(&self, bytes: &[u8], handles: &[u32]) -> Result<(), IpcError> {
        if handles.len() > MESSAGE_MAX_HANDLES {
            return Err(IpcError::FrameOverflow);
        }
        let header = Header::decode(bytes)?;

        // Ответ сервера: тело уже в IPC-буфере, доставляем его вызывателю
        // через сохранённый reply-handle. Наружу серверная сторона иначе не
        // пишет, и по этому пути идёт любой её кадр: без сохранённого reply
        // адресата у него нет.
        if header.has_flag(ipc::wire::FLAG_RESPONSE) || self.phase.get().is_server() {
            Self::store_buffer(bytes, handles)?;
            let reply = self.pending_reply.take().ok_or(IpcError::PeerClosed)?;
            return map_send_result(port_reply(reply));
        }

        drop(self.delivered.take());
        Self::store_buffer(bytes, handles)?;
        if header.txid != 0 {
            // Two-way клиентский запрос: запрос уложен в IPC-буфер, но сам
            // port_call (блокирующий до reply) откладываем до wait_readable,
            // где доступен timeout клиента. Ответ окажется в том же буфере.
            self.phase.set(Phase::ClientCallPending);
            Ok(())
        } else {
            // One-way cast.
            map_send_result(port_send(self.handle, self.send_timeout))
        }
    }

    fn read_message(&self, bytes: &mut [u8], handles: &mut [u32]) -> Result<MessageLen, IpcError> {
        match self.phase.get() {
            // Ответ на call уже доставлен вызовом port_call в IPC-буфер.
            Phase::ClientResponseReady => {
                self.phase.set(Phase::ClientIdle);
                Self::load_buffer(bytes, handles)
            }
            Phase::ClientIdle | Phase::ClientCallPending => Err(IpcError::WouldBlock),
            // Кадр уже принят предшествующим wait_readable (с тайм-аутом) -
            // только декодируем.
            Phase::ServerStaged => {
                self.phase.set(Phase::ServerIdle);
                Self::load_buffer(bytes, handles)
            }
            // Блокирующий приём (бессрочно), как при прямом dispatch без
            // wait_readable.
            Phase::ServerIdle => {
                self.recv_into_buffer(Timeout::INFINITE)?;
                Self::load_buffer(bytes, handles)
            }
        }
    }

    fn wait_readable(&self, timeout: Timeout) -> Result<(), IpcError> {
        match self.phase.get() {
            // Клиент: выполняем отложенный two-way call С тайм-аутом клиента.
            // Ответ ложится в IPC-буфер; его декодирует следующий
            // read_message. `Timeout` пробрасывается вызывающему.
            Phase::ClientCallPending => {
                self.phase.set(Phase::ClientIdle);
                let ret = port_call(self.handle, timeout);
                if ret == SYSCALL_RETURN_TIMEOUT {
                    return Err(IpcError::Timeout);
                }
                map_send_result(ret)?;
                self.phase.set(Phase::ClientResponseReady);
                self.record_delivery(None);
                Ok(())
            }
            // Без отложенного call (например, после уже полученного ответа) -
            // no-op.
            Phase::ClientIdle | Phase::ClientResponseReady => Ok(()),
            // Сервер: ждём отправителя/вызывателя с тайм-аутом. Принятый кадр
            // остаётся в IPC-буфере, его декодирует следующий read_message.
            // `Timeout` пробрасывается вызывающему (например, dispatch-циклу).
            Phase::ServerIdle | Phase::ServerStaged => {
                self.recv_into_buffer(timeout)?;
                self.phase.set(Phase::ServerStaged);
                Ok(())
            }
        }
    }
}
