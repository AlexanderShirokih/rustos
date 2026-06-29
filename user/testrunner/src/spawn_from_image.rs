//! E2E спавна из реального userland-образа.

use bootstrap::BootstrapClient;
use kernel_tests::kernel_test;
use runtime::{MemoryRegion, OwnedHandle, PortTransport, Resource, Signal, Timeout, UserMemFlags};
use userland_image::decode;
use userland_loader::load_entry;

const CHILD_WAIT_TIMEOUT_NS: u64 = 500_000_000;

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

    let resource = Resource::self_resource();
    let start_handle = Signal::create().expect("signal create").into_handle();
    let process = load_entry(&child, &resource, start_handle).expect("spawn child from image");

    process
        .join(Timeout::from_ns(CHILD_WAIT_TIMEOUT_NS))
        .expect("child terminates");

    kernel_tests::kassert_eq!(process.exit_code().expect("exit code"), 0);
}
