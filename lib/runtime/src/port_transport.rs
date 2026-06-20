//! Транспорт ipc-контрактов поверх синхронного `Port` + per-thread
//! IPC-буфер.
//!
//! `PortTransport` говорит на рандеву-примитивах ядра
//! (`port_send`/`recv`/`call`/`reply`): сообщение всегда лежит в
//! per-thread IPC-буфере текущего потока, а сами syscalls блокируют до
//! встречи. Поскольку методы [`Transport`] принимают `&self`, а семантика
//! клиента и сервера различается, роль задаётся явно при конструировании
//! ([`PortTransport::client`] / [`PortTransport::server`]).
//!
//! Round-trip по сгенерированному кодом паттерну:
//! - `#[cast]` клиент: `write_message` (txid==0) -> `port_send`.
//! - `#[call]` клиент: `write_message` (txid!=0, без RESPONSE) лишь
//!   укладывает запрос в IPC-буфер; затем `wait_readable(wait_ns)` ->
//!   `port_call` (блокирует до reply С тайм-аутом клиента; ответ ложится в
//!   тот же буфер), и `read_message` декодирует ответ ИЗ буфера, без recv.
//! - сервер dispatch: `read_message` -> `port_recv` (блокирует бессрочно,
//!   отдаёт reply-handle); для `#[call]` затем `write_message` (RESPONSE) ->
//!   `port_reply(reply_handle)`; для `#[cast]` reply нет.
//!
//! Тайм-аут типизированного IPC доступен через `wait_readable(timeout_ns)`:
//! у клиента он ограничивает `port_call`, у сервера - приём (`port_recv`)
//! очередного кадра (принятый кадр стейджится для следующего `read_message`).
//! На истечении возвращается `Timeout`. Прямой серверный `read_message` без
//! предшествующего `wait_readable` блокируется бессрочно.

use core::cell::Cell;

use ipc::{
    MessageLen, Transport,
    wire::{Header, IpcError, MESSAGE_MAX_HANDLES},
};
use syscall::{
    Handle, IPC_BUFFER_DATA_MAX, IPC_BUFFER_MAX_CAPS, PORT_TIMEOUT_INFINITE,
    SYSCALL_RETURN_TIMEOUT, decode_tag, encode_tag,
};

use crate::{
    ipc_buffer::ipc_buffer_ptr,
    svc::{port_call, port_recv, port_reply, port_send},
};

/// Фаза конца port-транспорта: кодирует и роль (клиент/сервер), и текущее
/// транзиентное состояние round-trip'а одним значением, делая нелегальные
/// комбинации (например, одновременно отложенный `call` и готовый ответ)
/// непредставимыми.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Клиент между сообщениями: `cast` -> send, `call` -> отложенный call.
    ClientIdle,
    /// Клиент: запрос two-way `call` уложен в IPC-буфер, блокирующий
    /// `port_call` отложен до `wait_readable` (там доступен `timeout_ns`).
    ClientCallPending,
    /// Клиент: ответ на `call` лежит в IPC-буфер, его ждёт следующий
    /// `read_message`.
    ClientResponseReady,
    /// Сервер между приёмами: `read_message` делает блокирующий `recv`.
    ServerIdle,
    /// Сервер: `wait_readable` уже принял кадр (с тайм-аутом) в IPC-буфер;
    /// следующий `read_message` лишь декодирует его, без повторного `recv`.
    ServerStaged,
}

/// Порт ipc-контракта поверх синхронного port'а.
pub struct PortTransport {
    handle: Handle,
    phase: Cell<Phase>,
    /// Серверная роль: reply-handle, сохранённый последним `recv` (если
    /// встречный был `call`); `write_message` с RESPONSE его забирает.
    pending_reply: Cell<Option<Handle>>,
}

impl PortTransport {
    /// Клиентская сторона порта: отправитель `cast`/`call`.
    #[must_use]
    pub fn client(handle: Handle) -> Self {
        Self::with_phase(handle, Phase::ClientIdle)
    }

    /// Серверная сторона порта: получатель `recv`, отвечающий `reply`.
    #[must_use]
    pub fn server(handle: Handle) -> Self {
        Self::with_phase(handle, Phase::ServerIdle)
    }

    fn with_phase(handle: Handle, phase: Phase) -> Self {
        Self {
            handle,
            phase: Cell::new(phase),
            pending_reply: Cell::new(None),
        }
    }

    /// Серверный приём кадра в IPC-буфер с тайм-аутом `timeout_ns`. Сохраняет
    /// reply-handle (если встречный был `call`) для последующего
    /// `write_message(RESPONSE)`. `Timeout` по истечении срока; `PeerClosed`
    /// при закрытом port'е.
    fn recv_into_buffer(&self, timeout_ns: u64) -> Result<(), IpcError> {
        let ret = port_recv(self.handle, timeout_ns);
        if ret == SYSCALL_RETURN_TIMEOUT {
            return Err(IpcError::Timeout);
        }
        if ret < 0 {
            return Err(IpcError::PeerClosed);
        }
        #[allow(clippy::cast_sign_loss)]
        let reply = Handle::new(ret as u32);
        self.pending_reply.set(reply);
        Ok(())
    }

    /// Записывает кадр (`bytes`) и его `handles` в per-thread IPC-буфер
    /// текущего потока: `data[..len]`, `caps[..ncaps]`, `tag`.
    fn store_buffer(bytes: &[u8], handles: &[u32]) -> Result<(), IpcError> {
        if bytes.len() > IPC_BUFFER_DATA_MAX {
            return Err(IpcError::FrameOverflow);
        }
        if handles.len() > IPC_BUFFER_MAX_CAPS {
            return Err(IpcError::BoundExceeded);
        }
        let ptr = ipc_buffer_ptr().ok_or(IpcError::PeerClosed)?;
        // SAFETY: ядро замаппило per-thread IPC-буфер user-RW по одной
        // странице; `ptr` валиден и эксклюзивен для текущего потока, запись
        // в пределах полей структуры.
        unsafe {
            let buf = &mut *ptr;
            buf.data[..bytes.len()].copy_from_slice(bytes);
            for (slot, &raw) in buf.caps.iter_mut().zip(handles) {
                *slot = raw;
            }
            buf.tag = encode_tag(bytes.len(), handles.len());
        }
        Ok(())
    }

    /// Читает кадр из per-thread IPC-буфера в `bytes`/`handles` по `tag`.
    fn load_buffer(bytes: &mut [u8], handles: &mut [u32]) -> Result<MessageLen, IpcError> {
        let ptr = ipc_buffer_ptr().ok_or(IpcError::PeerClosed)?;
        // SAFETY: см. `store_buffer`; здесь только чтение полей буфера.
        let (len, ncaps) = unsafe {
            let buf = &*ptr;
            let (len, ncaps) = decode_tag(buf.tag);
            let len = len.min(IPC_BUFFER_DATA_MAX);
            let ncaps = ncaps.min(IPC_BUFFER_MAX_CAPS);
            if len > bytes.len() || ncaps > handles.len() {
                return Err(IpcError::Truncated);
            }
            bytes[..len].copy_from_slice(&buf.data[..len]);
            for (dst, &raw) in handles.iter_mut().zip(&buf.caps[..ncaps]) {
                *dst = raw;
            }
            (len, ncaps)
        };
        Ok(MessageLen::new(len, ncaps))
    }
}

/// Сводит код возврата port-svc к ошибке wire-транспорта.
/// `0` - успех; отрицательное - закрытый/битый port.
fn map_send_result(ret: i64) -> Result<(), IpcError> {
    if ret == 0 {
        Ok(())
    } else {
        Err(IpcError::PeerClosed)
    }
}

impl Transport for PortTransport {
    fn write_message(&self, bytes: &[u8], handles: &[u32]) -> Result<(), IpcError> {
        if handles.len() > MESSAGE_MAX_HANDLES {
            return Err(IpcError::BoundExceeded);
        }
        let header = Header::decode(bytes)?;

        // Ответ сервера: тело уже в IPC-буфере, доставляем его вызывателю
        // через сохранённый reply-handle.
        if header.has_flag(ipc::wire::FLAG_RESPONSE) {
            Self::store_buffer(bytes, handles)?;
            let reply = self.pending_reply.take().ok_or(IpcError::PeerClosed)?;
            return map_send_result(port_reply(reply));
        }

        Self::store_buffer(bytes, handles)?;
        if header.txid != 0 {
            // Two-way клиентский запрос: запрос уложен в IPC-буфер, но сам
            // port_call (блокирующий до reply) откладываем до wait_readable,
            // где доступен timeout_ns клиента. Ответ окажется в том же буфере.
            self.phase.set(Phase::ClientCallPending);
            Ok(())
        } else {
            // One-way cast.
            map_send_result(port_send(self.handle, PORT_TIMEOUT_INFINITE))
        }
    }

    fn read_message(&self, bytes: &mut [u8], handles: &mut [u32]) -> Result<MessageLen, IpcError> {
        match self.phase.get() {
            // Ответ на call уже доставлен port_call'ом в IPC-буфер.
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
                self.recv_into_buffer(PORT_TIMEOUT_INFINITE)?;
                Self::load_buffer(bytes, handles)
            }
        }
    }

    fn wait_readable(&self, timeout_ns: u64) -> Result<(), IpcError> {
        match self.phase.get() {
            // Клиент: выполняем отложенный two-way call С тайм-аутом клиента.
            // Ответ ложится в IPC-буфер; его декодирует следующий
            // read_message. `Timeout` пробрасывается вызывающему.
            Phase::ClientCallPending => {
                self.phase.set(Phase::ClientIdle);
                let ret = port_call(self.handle, timeout_ns);
                if ret == SYSCALL_RETURN_TIMEOUT {
                    return Err(IpcError::Timeout);
                }
                map_send_result(ret)?;
                self.phase.set(Phase::ClientResponseReady);
                Ok(())
            }
            // Без отложенного call (например, после уже полученного ответа) -
            // no-op.
            Phase::ClientIdle | Phase::ClientResponseReady => Ok(()),
            // Сервер: ждём отправителя/вызывателя с тайм-аутом. Принятый кадр
            // остаётся в IPC-буфере, его декодирует следующий read_message.
            // `Timeout` пробрасывается вызывающему (например, dispatch-циклу).
            Phase::ServerIdle | Phase::ServerStaged => {
                self.recv_into_buffer(timeout_ns)?;
                self.phase.set(Phase::ServerStaged);
                Ok(())
            }
        }
    }
}
