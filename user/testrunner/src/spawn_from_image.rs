//! E2E спавна из реального userland-образа.

use bootstrap::BootstrapClient;
use kernel_tests::kernel_test;
use runtime::{MemoryRegion, OwnedHandle, PortTransport, Resource, Signal, Timeout, UserMemFlags};
use userland_image::decode;
use userland_loader::load_entry;

const CHILD_WAIT_TIMEOUT_NS: u64 = 500_000_000;
const CALL_TIMEOUT_NS: u64 = 2_000_000_000;

#[kernel_test]
fn spawn_child_from_image() {
    let client = BootstrapClient::new(PortTransport::client(crate::bootstrap_handle()));

    let cap = client
        .acquire_userland_image()
        .expect("acquire_userland_image call")
        .expect("vend ok");

    let region = MemoryRegion::from_handle(OwnedHandle::adopt(cap));
    let info = region.inspect().expect("inspect image region");
    let mapping = region
        .map(info.size_bytes, UserMemFlags::ReadOnly)
        .expect("map image read-only");

    let image = decode(mapping.as_bytes()).expect("decode acquired image");
    let child = image
        .entry("spawn-fixture")
        .expect("image carries a child entry");

    let resource = Resource::self_resource().expect("self metering resource");
    let start_handle = Signal::create().expect("signal create").into_handle();
    let process = load_entry(&child, &resource, start_handle).expect("spawn child from image");

    process
        .join(Timeout::from_ns(CHILD_WAIT_TIMEOUT_NS))
        .expect("child terminates");

    kernel_tests::kassert_eq!(process.exit_code().expect("exit code"), 0);
}

/// Сервер принимает `call` и уходит, не ответив: вызыватель обязан быть
/// разбужен отказом, а не остаться припаркованным.
#[kernel_test]
fn caller_wakes_when_server_dies_holding_reply() {
    use runtime::{Error, Port};
    use syscall::SyscallError;

    let client = BootstrapClient::new(PortTransport::client(crate::bootstrap_handle()));
    let cap = client
        .acquire_userland_image()
        .expect("acquire_userland_image call")
        .expect("vend ok");
    let region = MemoryRegion::from_handle(OwnedHandle::adopt(cap));
    let info = region.inspect().expect("inspect image region");
    let mapping = region
        .map(info.size_bytes, UserMemFlags::ReadOnly)
        .expect("map image read-only");
    let image = decode(mapping.as_bytes()).expect("decode acquired image");
    let child = image
        .entry("reply-holder")
        .expect("image carries the holder");

    let service = Port::create_service(0).expect("port create");

    let resource = Resource::self_resource().expect("self metering resource");
    let _process = load_entry(&child, &resource, service.server).expect("spawn holder from image");

    let mut out = [0u8; 8];
    // Кадр уже принят, поэтому вызыватель снят с очереди порта: его будит снос
    // reply-хэндла вместе с таблицей ушедшего сервера, а не пустое множество
    // получателей. Тайм-аут страхует от вечной парковки, исход - `PeerClosed`.
    let result = service
        .client
        .call_bytes(b"x", &mut out, Timeout::from_ns(CALL_TIMEOUT_NS));
    kernel_tests::kassert!(matches!(
        result,
        Err(Error::Syscall(SyscallError::PeerClosed))
    ));
}
