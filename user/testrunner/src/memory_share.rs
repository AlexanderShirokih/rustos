//! E2E разделяемой памяти между двумя адресными пространствами из EL0:
//! регион уезжает потомку по порту отдельной capability и виден там как тот же
//! физический фрейм, но по своему VA.

use kernel_tests::kernel_test;
use runtime::{MemoryAccess, Resource, UserMemFlags, handle_close};
use syscall::{Rights, SyscallError};

use crate::{
    fixture::Fixture,
    fixture_protocol::{OP_MAP_CHECK, REPLY_OK},
};

const PAGE_SIZE: u64 = syscall::PAGE_SIZE as u64;
const PATTERN: u64 = 0x5EED_F00D_C0DE_AB1E;

#[kernel_test]
fn memory_region_shared_with_child() {
    let resource = Resource::self_resource().expect("self metering resource");
    let region = resource
        .create_virtual(PAGE_SIZE, MemoryAccess::RW)
        .expect("create shared region");
    let mapping = region
        .map(PAGE_SIZE, UserMemFlags::ReadWrite)
        .expect("map region in parent");

    let slot = usize::try_from(mapping.va()).expect("positive va fits usize") as *mut u64;
    // SAFETY: map выдал RW-окно в PAGE_SIZE байт; 8-байтные доступы по его базе
    // лежат в границах, других владельцев маппинга нет.
    unsafe {
        slot.write_volatile(PATTERN);
    }

    let fixture = Fixture::spawn();

    // Потомку уезжает производная, а не корень: корень остаётся у родителя и
    // держит регион живым, пока потомок с ним работает.
    let lease = region
        .handle()
        .duplicate(Rights::READ | Rights::WRITE | Rights::TRANSFER, None)
        .expect("duplicate region handle for child");
    let leased_raw = lease.as_raw();

    let status = fixture.request_status(OP_MAP_CHECK, PATTERN, &[leased_raw.raw()]);
    kernel_tests::kassert_eq!(status, REPLY_OK);

    // Перенос капы по порту - move: ядро забрало хэндл, наш `Drop` его закрывать
    // не должен, а сам слот больше не валиден.
    let stale = lease.into_raw();
    kernel_tests::kassert_eq!(
        handle_close(stale),
        SyscallError::BadHandle.as_return_value()
    );

    // Обратное направление: инверсию писал потомок через свой маппинг.
    // SAFETY: см. выше.
    unsafe {
        kernel_tests::kassert_eq!(slot.read_volatile(), !PATTERN);
    }
}
