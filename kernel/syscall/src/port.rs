//! Handler-ы port-syscall'ов.
//!
//! Сообщение в per-thread IPC-буфере: tag задаёт `(len, ncaps)`, тело в `data[..len]`,
//! хендлы в `caps[..ncaps]`.

use collections::LockCell;
use kobject::{
    Handle, KObject, Port, Rights, ThreadTransport, port_call, port_recv, port_send, runtime,
};
use memory::virtual_address::VirtualAddress;

use super::{bridge::parse_handle_id, error::SyscallError, runtime::runtime as syscall_runtime};

/// Собирает [`ThreadTransport`] текущего потока. `Err`, если поток
/// не имеет user-AS или IPC-буфера (kernel-поток).
fn current_transport() -> Result<ThreadTransport, SyscallError> {
    let user_vm = syscall_runtime()
        .current_user_vm()
        .ok_or(SyscallError::WrongType)?;
    let ipc_va = syscall_runtime()
        .current_ipc_buffer_va()
        .ok_or(SyscallError::WrongType)?;
    let table = runtime()
        .current_handle_table()
        .ok_or(SyscallError::BadHandle)?;
    Ok(ThreadTransport::new(
        user_vm.mapper_arc(),
        VirtualAddress::new(usize::try_from(ipc_va).map_err(|_| SyscallError::InvalidArgument)?),
        table,
    ))
}

/// `port_create()` - создаёт `Port`. Требует handle-таблицы текущего потока.
pub(super) fn sys_port_create() -> Result<u64, SyscallError> {
    let table = runtime()
        .current_handle_table()
        .ok_or(SyscallError::BadHandle)?;
    let port = Port::new();
    let ko = KObject::Port(port);
    let handle = Handle::new(ko.clone(), Rights::defaults_for(&ko));

    let id = table
        .with_lock(|tbl| tbl.insert(handle))
        .map_err(SyscallError::from)?;
    Ok(u64::from(id.raw().get()))
}

/// ABI тайм-аут -> `Option<u64>`: `u64::MAX` ([`syscall::PORT_TIMEOUT_INFINITE`])
/// -> `None` (бессрочно), `0` ([`syscall::PORT_TIMEOUT_POLL`]) -> `Some(0)`,
/// остальное -> `Some(timeout_ns)`.
fn timeout_from_abi(raw: u64) -> Option<u64> {
    if raw == syscall::PORT_TIMEOUT_INFINITE {
        None
    } else {
        Some(raw)
    }
}

/// `port_send(handle, timeout_ns)` - блокирующая отправка из IPC-буфера
/// текущего потока. Требует `Rights::WRITE`.
pub(super) fn sys_port_send(handle: u64, timeout_ns: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let table = runtime()
        .current_handle_table()
        .ok_or(SyscallError::BadHandle)?;
    // Badge отправителя доставляется получателю в транспорте.
    let (port, badge) = table
        .with_lock(|tbl| tbl.get_port_with_badge(id, Rights::WRITE))
        .map_err(SyscallError::from)?;

    let transport = current_transport()?.with_badge(badge);
    port_send(&port, transport, runtime(), timeout_from_abi(timeout_ns))
        .map_err(SyscallError::from)?;
    Ok(0)
}

/// `port_recv(handle, timeout_ns)` - блокирующий приём в IPC-буфер текущего
/// потока. Требует `Rights::READ`. Возврат: reply handle id (если встречный
/// был `call`), иначе `0`.
pub(super) fn sys_port_recv(handle: u64, timeout_ns: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let table = runtime()
        .current_handle_table()
        .ok_or(SyscallError::BadHandle)?;
    let port = table
        .with_lock(|tbl| tbl.get_port(id, Rights::READ))
        .map_err(SyscallError::from)?;

    let transport = current_transport()?;
    let reply = port_recv(&port, transport, runtime(), timeout_from_abi(timeout_ns))
        .map_err(SyscallError::from)?;

    match reply {
        None => Ok(0),
        Some(reply) => {
            let ko = KObject::Reply(reply);
            let h = Handle::new(ko.clone(), Rights::defaults_for(&ko));
            let reply_id = table
                .with_lock(|tbl| tbl.insert(h))
                .map_err(SyscallError::from)?;
            Ok(u64::from(reply_id.raw().get()))
        }
    }
}

/// `port_call(handle, timeout_ns)` - блокирующий запрос-ответ. Сообщение из
/// IPC-буфера; ответ оказывается там же. Требует `Rights::WRITE`.
pub(super) fn sys_port_call(handle: u64, timeout_ns: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let table = runtime()
        .current_handle_table()
        .ok_or(SyscallError::BadHandle)?;
    // Badge вызывателя идёт на сторону сервера; reply badge не несёт.
    let (port, badge) = table
        .with_lock(|tbl| tbl.get_port_with_badge(id, Rights::WRITE))
        .map_err(SyscallError::from)?;

    let transport = current_transport()?.with_badge(badge);
    port_call(&port, transport, runtime(), timeout_from_abi(timeout_ns))
        .map_err(SyscallError::from)?;
    Ok(0)
}

/// `port_reply(reply_handle)` - доставляет ответ из IPC-буфера сервера
/// вызывателю. Требует `Rights::WRITE`. One-shot: реплай-handle становится
/// невалидным после успеха.
pub(super) fn sys_port_reply(handle: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let table = runtime()
        .current_handle_table()
        .ok_or(SyscallError::BadHandle)?;
    let reply = table
        .with_lock(|tbl| tbl.get_reply(id, Rights::WRITE))
        .map_err(SyscallError::from)?;

    let server = current_transport()?;
    let res = reply.reply(&server).map_err(SyscallError::from);

    // One-shot: после reply handle исчерпан; повторный вызов даёт BadHandle.
    let _ = table.with_lock(|tbl| tbl.remove(id));

    res?;
    Ok(0)
}
