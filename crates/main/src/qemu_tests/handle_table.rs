//! Сценарии вокруг `HandleTable`: insert/get/duplicate/remove + проверка
//! прав и типов.

extern crate alloc;

use alloc::sync::Arc;

use qemu_test_harness::register_test;

fn handle_table_basic() {
    use crate::kobject::{Handle, HandleTable, IpcError, KernelObject, Koid, ObjectType, Rights};

    struct Dummy {
        koid: Koid,
        ty: ObjectType,
    }
    impl KernelObject for Dummy {
        fn koid(&self) -> Koid {
            self.koid
        }
        fn object_type(&self) -> ObjectType {
            self.ty
        }
    }

    let mut table = HandleTable::with_capacity(4);
    let obj = Arc::new(Dummy {
        koid: Koid::allocate(),
        ty: ObjectType::Channel,
    });
    let handle = Handle::new(obj, Rights::DUPLICATE | Rights::READ | Rights::WRITE);

    let id = table.insert(handle).expect("insert must succeed");
    qemu_test_harness::kassert!(table.get(id, Rights::READ, ObjectType::Channel).is_ok());

    let dup = table
        .duplicate(id, Rights::READ)
        .expect("duplicate must succeed");
    qemu_test_harness::kassert!(matches!(
        table.get(dup, Rights::WRITE, ObjectType::Channel),
        Err(IpcError::AccessDenied)
    ));
    qemu_test_harness::kassert!(matches!(
        table.get(id, Rights::READ, ObjectType::Event),
        Err(IpcError::WrongType)
    ));

    table.remove(id).expect("remove must succeed");
    qemu_test_harness::kassert!(matches!(table.remove(id), Err(IpcError::BadHandle)));
}

register_test!(HANDLE_TABLE_BASIC, "handle_table_basic", handle_table_basic);
