//! Сценарии вокруг `HandleTable`: insert/get/duplicate/remove + проверка
//! прав и типов.

extern crate alloc;

use kernel_tests::kernel_test;

#[kernel_test]
fn handle_table_basic() {
    use capability::{Capability, CapabilityTarget, HandleTable, IpcError, Port, Rights, Signal};

    let mut table = HandleTable::with_capacity(4);

    // Тестируем с реальным Port capability target.
    let handle = Capability::new(
        CapabilityTarget::Port(Port::new()),
        Rights::DUPLICATE | Rights::READ | Rights::WRITE,
    );

    let id = table.insert(handle).expect("insert must succeed");
    kernel_tests::kassert!(table.get_port(id, Rights::READ).is_ok());

    let dup = table
        .duplicate(id, Rights::READ, 0)
        .expect("duplicate must succeed");
    kernel_tests::kassert!(matches!(
        table.get(dup, Rights::WRITE),
        Err(IpcError::AccessDenied)
    ));

    // Тип Signal не совпадает с Port.
    kernel_tests::kassert!(matches!(
        table.get_signal(id, Rights::READ),
        Err(IpcError::WrongType)
    ));

    table.remove(id).expect("remove must succeed");
    kernel_tests::kassert!(matches!(table.remove(id), Err(IpcError::BadHandle)));

    let signal_handle = Capability::new(CapabilityTarget::Signal(Signal::new()), Rights::READ);
    let nid = table
        .insert(signal_handle)
        .expect("signal insert must succeed");
    kernel_tests::kassert!(table.get_signal(nid, Rights::READ).is_ok());
    kernel_tests::kassert!(matches!(
        table.get_port(nid, Rights::READ),
        Err(IpcError::WrongType)
    ));
}
