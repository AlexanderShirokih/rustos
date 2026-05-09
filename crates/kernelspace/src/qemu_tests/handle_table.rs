//! Сценарии вокруг `HandleTable`: insert/get/duplicate/remove + проверка
//! прав и типов.

extern crate alloc;

use test_harness_qemu::register_test;

fn handle_table_basic() {
    use kobject::{ChannelEndpoint, Event, Handle, HandleTable, IpcError, KObject, Rights};

    let mut table = HandleTable::with_capacity(4);

    // Тестируем с реальным Channel KO.
    let (ep, _) = ChannelEndpoint::create_pair(4);
    let handle = Handle::new(
        KObject::Channel(ep),
        Rights::DUPLICATE | Rights::READ | Rights::WRITE,
    );

    let id = table.insert(handle).expect("insert must succeed");
    test_harness_qemu::kassert!(table.get_channel(id, Rights::READ).is_ok());

    let dup = table
        .duplicate(id, Rights::READ)
        .expect("duplicate must succeed");
    test_harness_qemu::kassert!(matches!(
        table.get(dup, Rights::WRITE),
        Err(IpcError::AccessDenied)
    ));
    // Тип Event не совпадает с Channel.
    test_harness_qemu::kassert!(matches!(
        table.get_event(id, Rights::READ),
        Err(IpcError::WrongType)
    ));

    table.remove(id).expect("remove must succeed");
    test_harness_qemu::kassert!(matches!(table.remove(id), Err(IpcError::BadHandle)));

    // Дополнительно: Event handle работает через get_event.
    let event_handle = Handle::new(KObject::Event(Event::new()), Rights::WAIT);
    let eid = table
        .insert(event_handle)
        .expect("event insert must succeed");
    test_harness_qemu::kassert!(table.get_event(eid, Rights::WAIT).is_ok());
    test_harness_qemu::kassert!(matches!(
        table.get_channel(eid, Rights::WAIT),
        Err(IpcError::WrongType)
    ));
}

register_test!(HANDLE_TABLE_BASIC, "handle_table_basic", handle_table_basic);
