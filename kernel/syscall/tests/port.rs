//! Интеграционные тесты port-IPC хендлеров (`syscall::port`).
//!
//! `Reply::new`/`port_call`-flow требуют двух потоков с реальными транспортами
//! (недоступно в host-тесте). Здесь проверяем только то, что достижимо через
//! `KernelRuntime`: создание Port-хендла и корректный `IpcError`-маппинг.

#![allow(clippy::unwrap_used)]
#![allow(clippy::indexing_slicing, clippy::panic, clippy::unwrap_in_result)]

mod common;

use std::sync::Arc;

use capability::{Capability, CapabilityTarget, HandleTable, Rights, Signal};
use collections::{LockCell, MutexCell};
use memory::testing::PageMapper;
use syscall_kernel::{SyscallError, SyscallOp};

use crate::common::{TableRuntime, dispatch, runtimes, test_lock};

fn runtime() -> &'static Arc<TableRuntime> {
    let (rt, stub) = runtimes();
    stub.set_mapper(Arc::new(PageMapper::new()));
    stub.set_ipc_buffer_va(0x3000_0000);
    rt
}

fn err_code(e: SyscallError) -> i64 {
    e.as_return_value()
}

#[test]
fn port_create_registers_usable_port_handle() {
    let _guard = test_lock();
    let rt = runtime();
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(4)));
    rt.set_handle_table(table.clone());

    let id = dispatch(SyscallOp::PortCreate, [0; 6]).expect("port create succeeds");
    assert!(id != 0, "fresh port handle id must be non-zero");

    let raw = u32::try_from(id).unwrap();
    let handle_id = capability::HandleId::from_raw(core::num::NonZeroU32::new(raw).unwrap());
    table.with_lock(|tbl| {
        tbl.get_port(handle_id, Rights::READ)
            .expect("port readable");
        tbl.get_port(handle_id, Rights::WRITE)
            .expect("port writable");
    });
}

#[test]
fn port_send_on_non_port_handle_is_wrong_type() {
    let _guard = test_lock();
    let rt = runtime();
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(4)));
    let sig_id = table
        .with_lock(|tbl| {
            tbl.insert(Capability::new(
                CapabilityTarget::Signal(Signal::new()),
                Rights::READ | Rights::WRITE,
            ))
        })
        .expect("insert signal");
    rt.set_handle_table(table);

    let raw = u64::from(sig_id.raw().get());
    // send/call/recv на Signal (не Port) -> WrongType.
    assert_eq!(
        dispatch(SyscallOp::PortSend, [raw, 0, 0, 0, 0, 0]),
        Err(err_code(SyscallError::WrongType))
    );
    assert_eq!(
        dispatch(SyscallOp::PortCall, [raw, 0, 0, 0, 0, 0]),
        Err(err_code(SyscallError::WrongType))
    );
    assert_eq!(
        dispatch(SyscallOp::PortRecv, [raw, 0, 0, 0, 0, 0]),
        Err(err_code(SyscallError::WrongType))
    );
}

#[test]
fn port_reply_on_non_reply_handle_is_wrong_type() {
    let _guard = test_lock();
    let rt = runtime();
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(4)));
    let sig_id = table
        .with_lock(|tbl| {
            tbl.insert(Capability::new(
                CapabilityTarget::Signal(Signal::new()),
                Rights::WRITE,
            ))
        })
        .expect("insert signal");
    rt.set_handle_table(table);

    assert_eq!(
        dispatch(
            SyscallOp::PortReply,
            [u64::from(sig_id.raw().get()), 0, 0, 0, 0, 0]
        ),
        Err(err_code(SyscallError::WrongType))
    );
}

#[test]
fn port_send_on_closed_handle_is_bad_handle() {
    let _guard = test_lock();
    let rt = runtime();
    let table = Arc::new(MutexCell::new(HandleTable::with_capacity(4)));
    rt.set_handle_table(table);

    assert_eq!(
        dispatch(SyscallOp::PortSend, [5, 0, 0, 0, 0, 0]),
        Err(err_code(SyscallError::BadHandle))
    );
}

#[test]
fn port_send_zero_handle_is_invalid_argument() {
    let _guard = test_lock();
    let rt = runtime();
    rt.set_handle_table(Arc::new(MutexCell::new(HandleTable::with_capacity(1))));
    assert_eq!(
        dispatch(SyscallOp::PortSend, [0, 0, 0, 0, 0, 0]),
        Err(err_code(SyscallError::InvalidArgument))
    );
}
