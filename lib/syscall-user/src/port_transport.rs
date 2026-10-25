//! Канал ipc-контрактов поверх `Port` ядра и per-thread IPC-буфера.
//!
//! [`PortTransport`] состояния не держит. Каждая операция - один syscall рандеву (`port_send`,
//! `port_call`, `port_recv`), и кадр идёт через IPC-буфер текущего потока. Клиентскую сторону
//! задаёт [`Channel`], серверную - [`Listener`], а роль ограничивают права handle. Отправка
//! требует `Rights::WRITE`, приём - `Rights::READ`. Ответ встречному вызову уходит каналом
//! ответа из ресурсов принятого кадра.
//!
//! Канал пишет номера capability исходящего кадра в IPC-буфер. Успех отправки снимает закрытие
//! с их владельцев, отказ `send` и ответа возвращает кадр вызывающему, а отказ `call` кадр
//! разрушает. Кадр ответа `call` канал читает в буфер исходящего кадра.
//!
//! [`PortTransport::post_send`] и [`PortTransport::post_call`] ставят кадр отложенной операцией
//! порта и сразу возвращают её. Исход после `SIGNALED` операции выдают [`take_send`] и
//! [`take_call`], а сигнал ждёт вызывающий.

use core::num::NonZeroU32;

use ipc::{
    Channel, FrameBytes, Incoming, Listener, Outgoing, OutgoingParts, RecvError, ReplyChannel,
    Unsent, wire::IpcError,
};
use syscall::{
    Handle, IPC_BUFFER_DATA_MAX, IPC_BUFFER_MAX_CAPS, IpcBuffer, SyscallError, Timeout, decode_tag,
    encode_tag, kind,
};

use crate::{
    Delivery, IssuedHandle,
    ipc_buffer::ipc_buffer_ptr,
    svc::{
        pending_take, port_call, port_call_async, port_recv, port_reply, port_send, port_send_async,
    },
};

/// Канал ipc-контракта поверх port-handle.
///
/// Канал заимствует port-handle, и handle остаётся за тем, кто его открыл.
#[derive(Debug)]
pub struct PortTransport {
    handle: Handle<kind::Port>,
}

impl PortTransport {
    /// Канал поверх указанного port-handle.
    #[must_use]
    pub fn new(handle: impl Into<Handle<kind::Port>>) -> Self {
        Self {
            handle: handle.into(),
        }
    }

    /// Ставит кадр вызова и возвращает отложенную операцию. Capability кадра переходят ядру при
    /// постановке, а отказ постановки оставляет их кадру. Исход выдаёт [`take_call`].
    pub fn post_call(
        &self,
        frame: &mut Outgoing<'_, Delivery>,
    ) -> Result<IssuedHandle<kind::Pending>, IpcError> {
        self.post(frame, port_call_async)
    }

    /// Ставит односторонний кадр и возвращает отложенную операцию. До исхода capability кадра
    /// держит ядро. Отказ доставки возвращает их кадру в [`take_send`], закрытие операции до
    /// исхода закрывает их, а отказ постановки оставляет их кадру.
    pub fn post_send(
        &self,
        frame: &mut Outgoing<'_, Delivery>,
    ) -> Result<IssuedHandle<kind::Pending>, IpcError> {
        self.post(frame, port_send_async)
    }

    fn post(
        &self,
        frame: &mut Outgoing<'_, Delivery>,
        post: fn(Handle<kind::Port>) -> Result<IssuedHandle<kind::Pending>, SyscallError>,
    ) -> Result<IssuedHandle<kind::Pending>, IpcError> {
        store_buffer(frame)?;
        let pending = post(self.handle).map_err(map_error)?;
        core::mem::take(frame.resources_mut()).transferred();
        Ok(pending)
    }
}

impl AsRef<PortTransport> for PortTransport {
    fn as_ref(&self) -> &PortTransport {
        self
    }
}

impl Channel for PortTransport {
    type Resources = Delivery;

    #[inline]
    fn send<'b>(
        &self,
        frame: Outgoing<'b, Delivery>,
        wait: Timeout,
    ) -> Result<(), Unsent<'b, Delivery>> {
        let sent =
            store_buffer(&frame).and_then(|()| map_send_result(port_send(self.handle, wait)));
        settle(sent, frame)
    }

    #[inline]
    fn call<'b>(
        &self,
        frame: Outgoing<'b, Delivery>,
        wait: Timeout,
    ) -> Result<Incoming<'b, Delivery>, IpcError> {
        store_buffer(&frame).and_then(|()| map_send_result(port_call(self.handle, wait)))?;
        let OutgoingParts { buffer, resources } = frame.into_parts();
        resources.transferred();
        load_buffer(buffer, None).ok_or(IpcError::PeerClosed)
    }
}

impl Listener for PortTransport {
    type Resources = Delivery;

    fn recv<'b>(
        &self,
        into: &'b mut FrameBytes,
        wait: Timeout,
    ) -> Result<Incoming<'b, Delivery>, RecvError> {
        let reply = port_recv(self.handle, wait).map_err(map_recv_error)?;
        load_buffer(into, reply).ok_or(RecvError::Closed)
    }
}

impl ReplyChannel for IssuedHandle<kind::Reply> {
    type Resources = Delivery;

    #[inline]
    fn send(self, frame: Outgoing<'_, Delivery>) -> Result<(), Unsent<'_, Delivery>> {
        // Отказ записи кадра закрывает канал ответа, и вызыватель получает `PeerClosed`.
        let replied = store_buffer(&frame).and_then(|()| map_send_result(port_reply(self)));
        settle(replied, frame)
    }
}

/// Выдаёт исход отложенного вызова `pending` с поднятым `SIGNALED` и читает кадр ответа в
/// буфер кадра вызова `frame`.
pub fn take_call(
    pending: IssuedHandle<kind::Pending>,
    frame: Outgoing<'_, Delivery>,
) -> Result<Incoming<'_, Delivery>, IpcError> {
    take(pending)?;
    load_buffer(frame.into_parts().buffer, None).ok_or(IpcError::PeerClosed)
}

/// Выдаёт исход отложенной отправки `pending` с поднятым `SIGNALED`. Отказ возвращает кадр
/// `frame` с capability, которые вернуло ядро.
pub fn take_send(
    pending: IssuedHandle<kind::Pending>,
    mut frame: Outgoing<'_, Delivery>,
) -> Result<(), Unsent<'_, Delivery>> {
    let Err(error) = take(pending) else {
        return Ok(());
    };

    if let Some(ptr) = ipc_buffer_ptr() {
        // SAFETY: см. `load_buffer`. Здесь только чтение полей буфера.
        let buf = unsafe { &*ptr };
        // Ядро пишет возвращённые capability в порядке слотов кадра, и номер записи может
        // смениться поколением.
        *frame.resources_mut() = Delivery::new(0, buffer_caps(buf), None);
    }

    Err((error, frame))
}

/// Выдаёт конечный исход отложенной операции `pending` и расходует её. Операция без исхода
/// возвращается вызывающему.
pub fn take_outcome(
    pending: IssuedHandle<kind::Pending>,
) -> Result<i64, IssuedHandle<kind::Pending>> {
    let ret = pending_take(pending.as_raw());
    if SyscallError::from_return(ret) == Some(SyscallError::ShouldWait) {
        return Err(pending);
    }

    // Конечный исход расходует запись.
    let _ = pending.leak();
    Ok(ret)
}

fn take(pending: IssuedHandle<kind::Pending>) -> Result<(), IpcError> {
    match take_outcome(pending) {
        Ok(ret) => map_send_result(ret),
        // Операция без исхода отменяется закрытием.
        Err(unfinished) => {
            drop(unfinished);
            Err(IpcError::PeerClosed)
        }
    }
}

#[inline]
fn settle(
    sent: Result<(), IpcError>,
    frame: Outgoing<'_, Delivery>,
) -> Result<(), Unsent<'_, Delivery>> {
    match sent {
        Ok(()) => {
            frame.into_resources().transferred();
            Ok(())
        }
        Err(error) => Err((error, frame)),
    }
}

fn load_buffer(
    into: &mut FrameBytes,
    reply: Option<IssuedHandle<kind::Reply>>,
) -> Option<Incoming<'_, Delivery>> {
    let ptr = ipc_buffer_ptr()?;
    // SAFETY: ядро замаппило per-thread IPC-буфер user-RW по одной странице; `ptr` валиден
    // и эксклюзивен для текущего потока. Здесь только чтение полей буфера.
    let buf = unsafe { &*ptr };
    let len = decode_tag(buf.tag).len.min(IPC_BUFFER_DATA_MAX);
    let resources = Delivery::new(buf.badge, buffer_caps(buf), reply);
    let bytes = into.get_mut(..len)?;
    bytes.copy_from_slice(buf.data.get(..len)?);
    Some(Incoming { bytes, resources })
}

fn buffer_caps(buf: &IpcBuffer) -> [Option<Handle<kind::Any>>; IPC_BUFFER_MAX_CAPS] {
    let ncaps = decode_tag(buf.tag).ncaps.min(IPC_BUFFER_MAX_CAPS);
    let raw = buf.caps.get(..ncaps).unwrap_or_default();
    core::array::from_fn(|index| {
        raw.get(index)
            .copied()
            .and_then(NonZeroU32::new)
            .map(Handle::<kind::Any>::from_abi)
    })
}

fn store_buffer(frame: &Outgoing<'_, Delivery>) -> Result<(), IpcError> {
    let bytes = frame.as_bytes();
    if bytes.len() > IPC_BUFFER_DATA_MAX {
        return Err(IpcError::FrameOverflow);
    }

    let ptr = ipc_buffer_ptr().ok_or(IpcError::PeerClosed)?;
    // SAFETY: ядро замаппило per-thread IPC-буфер user-RW по одной странице; `ptr` валиден
    // и эксклюзивен для текущего потока, запись в пределах полей структуры.
    unsafe {
        let buf = &mut *ptr;
        let Some(target) = buf.data.get_mut(..bytes.len()) else {
            return Err(IpcError::Truncated);
        };
        target.copy_from_slice(bytes);
        let mut ncaps = 0;
        for (slot, raw) in buf.caps.iter_mut().zip(frame.resources().numbers()) {
            *slot = raw;
            ncaps += 1;
        }
        buf.tag = encode_tag(bytes.len(), ncaps);
    }

    Ok(())
}

fn map_recv_error(error: SyscallError) -> RecvError {
    match error {
        SyscallError::Timeout => RecvError::Timeout,
        SyscallError::SenderGone => RecvError::SenderGone(ipc_buffer_ptr().map_or(
            0,
            // SAFETY: см. `load_buffer`. Здесь только чтение поля буфера.
            |ptr| unsafe { (*ptr).badge },
        )),
        SyscallError::OutOfHandles => RecvError::Discarded,
        _ => RecvError::Closed,
    }
}

fn map_send_result(ret: i64) -> Result<(), IpcError> {
    match SyscallError::from_return(ret) {
        None if ret == 0 => Ok(()),
        Some(error) => Err(map_error(error)),
        None => Err(IpcError::PeerClosed),
    }
}

const fn map_error(error: SyscallError) -> IpcError {
    match error {
        SyscallError::Timeout => IpcError::Timeout,
        SyscallError::OutOfHandles | SyscallError::ResourceExhausted => IpcError::ResourceExhausted,
        _ => IpcError::PeerClosed,
    }
}
