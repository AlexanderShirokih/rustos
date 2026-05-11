//! Сценарии вокруг `HandleTable`: insert/get/duplicate/remove + проверка
//! прав и типов.

extern crate alloc;

use kernel_tests::kernel_test;

#[kernel_test]
fn handle_table_basic() {
    use kobject::{Channel, Event, Handle, HandleTable, IpcError, KObject, Rights};

    let mut table = HandleTable::with_capacity(4);

    // Тестируем с реальным Channel KO.
    let (ep, _) = Channel::create_pair(4);
    let handle = Handle::new(
        KObject::Channel(ep),
        Rights::DUPLICATE | Rights::READ | Rights::WRITE,
    );

    let id = table.insert(handle).expect("insert must succeed");
    kernel_tests::kassert!(table.get_channel(id, Rights::READ).is_ok());

    let dup = table
        .duplicate(id, Rights::READ)
        .expect("duplicate must succeed");
    kernel_tests::kassert!(matches!(
        table.get(dup, Rights::WRITE),
        Err(IpcError::AccessDenied)
    ));
    // Тип Event не совпадает с Channel.
    kernel_tests::kassert!(matches!(
        table.get_event(id, Rights::READ),
        Err(IpcError::WrongType)
    ));

    table.remove(id).expect("remove must succeed");
    kernel_tests::kassert!(matches!(table.remove(id), Err(IpcError::BadHandle)));

    // Дополнительно: Event handle работает через get_event.
    let event_handle = Handle::new(KObject::Event(Event::new()), Rights::WAIT);
    let eid = table
        .insert(event_handle)
        .expect("event insert must succeed");
    kernel_tests::kassert!(table.get_event(eid, Rights::WAIT).is_ok());
    kernel_tests::kassert!(matches!(
        table.get_channel(eid, Rights::WAIT),
        Err(IpcError::WrongType)
    ));
}
