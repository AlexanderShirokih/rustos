//! Обработчики port-syscall.
//!
//! Сообщение в per-thread IPC-буфере: tag задаёт `(len, ncaps)`, тело в `data[..len]`,
//! хендлы в `caps[..ncaps]`.

use capability::{
    Capability, IpcError, Port, PortAccess, Reply, Rights, ThreadTransport, deadline_after,
    install_handle, port_call, port_recv, port_send, release_delivered, runtime,
};
use collections::LockCell;
use memory::virtual_address::VirtualAddress;
use syscall::SyscallError;

use super::{
    bridge::parse_handle_id,
    error::map_ipc_error,
    runtime::{CurrentIpcContext, runtime as syscall_runtime},
};

fn current_ipc_context() -> Result<CurrentIpcContext, SyscallError> {
    syscall_runtime()
        .current_ipc_context()
        .ok_or(SyscallError::WrongType)
}

fn transport_from(context: &CurrentIpcContext) -> Result<ThreadTransport, SyscallError> {
    let ipc_va =
        usize::try_from(context.ipc_buffer_va).map_err(|_| SyscallError::InvalidArgument)?;
    Ok(ThreadTransport::new(
        context.user_vm.mapper_arc(),
        VirtualAddress::new(ipc_va),
        context.handle_table.clone(),
    ))
}

pub(super) fn sys_port_create() -> Result<u64, SyscallError> {
    let handle = Capability::new_with_default_rights(Port::new());
    let id = install_handle(handle).map_err(map_ipc_error)?;
    Ok(u64::from(id.raw().get()))
}

fn timeout_from_abi(raw: u64) -> Option<u64> {
    if raw == syscall::PORT_TIMEOUT_INFINITE {
        None
    } else {
        Some(raw)
    }
}

pub(super) fn sys_port_send(handle: u64, timeout_ns: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let context = current_ipc_context()?;
    // Badge отправителя доставляется получателю в транспорте.
    let PortAccess { port, badge, hold } = context
        .handle_table
        .with_lock(|tbl| tbl.get_port(id, Rights::WRITE))
        .map_err(map_ipc_error)?;

    let transport = transport_from(&context)?.with_badge(badge);
    port_send(
        &port,
        &transport,
        runtime(),
        timeout_from_abi(timeout_ns),
        hold,
    )
    .map_err(map_ipc_error)?;
    Ok(0)
}

pub(super) fn sys_port_recv(handle: u64, timeout_ns: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let context = current_ipc_context()?;
    let transport = transport_from(&context)?;
    let deadline_ns = deadline_after(runtime(), timeout_from_abi(timeout_ns));
    // `port_recv` поглощает удержание роли получателя, и повтор берёт порт из таблицы заново.
    let reply = loop {
        let PortAccess { port, hold, .. } = context
            .handle_table
            .with_lock(|tbl| tbl.get_port(id, Rights::READ))
            .map_err(map_ipc_error)?;
        match port_recv(&port, &transport, runtime(), deadline_ns, hold) {
            Err(IpcError::ShouldWait) => {}
            result => break result.map_err(map_ipc_error)?,
        }
    };

    match reply {
        None => Ok(0),
        Some(reply) => {
            let h = Capability::new_with_default_rights(reply);
            let reply_id = context
                .handle_table
                .with_lock(|tbl| tbl.insert(h))
                .map_err(|rejected| {
                    // Capability рандеву уже перенесены в таблицу получателя, а приём без
                    // reply завершается отказом: без отката набор остался бы без владельца.
                    let _ = release_delivered(&transport);
                    map_ipc_error(rejected.release())
                })?;
            Ok(u64::from(reply_id.raw().get()))
        }
    }
}

pub(super) fn sys_port_call(handle: u64, timeout_ns: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let context = current_ipc_context()?;
    // Badge вызывателя идёт на сторону сервера; reply badge вызов не передаёт.
    let PortAccess { port, badge, hold } = context
        .handle_table
        .with_lock(|tbl| tbl.get_port(id, Rights::WRITE))
        .map_err(map_ipc_error)?;

    let transport = transport_from(&context)?.with_badge(badge);
    port_call(
        &port,
        &transport,
        runtime(),
        timeout_from_abi(timeout_ns),
        hold,
    )
    .map_err(map_ipc_error)?;
    Ok(0)
}

pub(super) fn sys_port_reply(handle: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let context = current_ipc_context()?;
    let reply = context
        .handle_table
        .with_lock(|tbl| tbl.get_object::<Reply>(id, Rights::WRITE))
        .map_err(map_ipc_error)?;

    let server = transport_from(&context)?;
    let res = reply.reply(&server).map_err(map_ipc_error);

    // One-shot: после reply handle исчерпан; повторный вызов даёт BadHandle.
    if let Ok(removed) = context.handle_table.with_lock(|tbl| tbl.remove(id)) {
        removed.release();
    }

    res?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_from_abi_sentinel_means_infinite() {
        assert_eq!(timeout_from_abi(syscall::PORT_TIMEOUT_INFINITE), None);
        assert_eq!(timeout_from_abi(u64::MAX), None);
    }

    #[test]
    fn timeout_from_abi_zero_is_poll() {
        // 0 - неблокирующий poll, но это всё ещё Some(0), а не None.
        assert_eq!(timeout_from_abi(syscall::PORT_TIMEOUT_POLL), Some(0));
        assert_eq!(timeout_from_abi(0), Some(0));
    }

    #[test]
    fn timeout_from_abi_finite_passes_through() {
        assert_eq!(timeout_from_abi(1), Some(1));
        assert_eq!(timeout_from_abi(1_000_000), Some(1_000_000));
        assert_eq!(timeout_from_abi(u64::MAX - 1), Some(u64::MAX - 1));
    }

    #[test]
    fn port_handlers_reject_zero_handle_before_touching_runtime() {
        // parse_handle_id отвергает 0 раньше любого обращения к runtime,
        // поэтому хендлеры безопасно тестировать без установленного runtime.
        assert_eq!(sys_port_send(0, 0), Err(SyscallError::InvalidArgument));
        assert_eq!(sys_port_recv(0, 0), Err(SyscallError::InvalidArgument));
        assert_eq!(sys_port_call(0, 0), Err(SyscallError::InvalidArgument));
        assert_eq!(sys_port_reply(0), Err(SyscallError::InvalidArgument));
    }
}
