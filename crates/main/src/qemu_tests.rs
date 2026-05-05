//! Test-mode ядра под `cfg(feature = "qemu-tests")`.
//!
//! `kmain` после bootstrap'а создаёт init-таск и под этой фичей вызывает
//! [`run`] - он подключает console writer и aarch64 backend харнесса,
//! затем зовёт `qemu_test_harness::run_all_tests()`. Тесты регистрируются
//! ниже через `register_test!` и работают в полноценном окружении ядра:
//! живой scheduler, драйверы, прерывания, аллокатор.

extern crate alloc;

use alloc::sync::Arc;

use drivers_common::{CapabilityStoreExt, services::console::ConsoleService};
use io::writer::Writer;
use qemu_test_harness::register_test;
use spin::Once;

use crate::kernel_context::KernelContext;

struct ConsoleAdapter(Arc<dyn ConsoleService>);

impl Writer for ConsoleAdapter {
    fn write_all(&self, buf: &[u8]) {
        self.0.write_all(buf);
    }
    fn flush(&self) {
        self.0.flush();
    }
}

static ADAPTER: Once<ConsoleAdapter> = Once::new();

/// Подключает harness к ядру и запускает все зарегистрированные кейсы.
/// Не возвращается: завершает QEMU через ARM semihosting.
pub fn run(kernel: &mut KernelContext) -> ! {
    kernel.with_runtime_state(|caps, _| {
        let console = caps
            .require_service::<dyn ConsoleService>()
            .expect("ConsoleService must be available for qemu-tests");
        ADAPTER.call_once(|| ConsoleAdapter(console));
    });

    let writer: &'static (dyn Writer + Send + Sync) =
        ADAPTER.get().expect("ADAPTER initialised above");
    qemu_test_harness::runner::install_writer(writer);
    qemu_test_harness::runner::install_backend(&qemu_test_harness_aarch64::BACKEND);

    qemu_test_harness::run_all_tests()
}

// === Кейсы ===

fn smoke() {
    let x = core::hint::black_box(2);
    qemu_test_harness::kassert_eq!(x + x, 4);
}

register_test!(SMOKE_TEST, "smoke", smoke);

fn allocator_basic() {
    use alloc::vec::Vec;

    let mut v: Vec<u32> = Vec::with_capacity(16);
    for i in 0..16 {
        v.push(i);
    }
    qemu_test_harness::kassert_eq!(v.len(), 16);
    qemu_test_harness::kassert_eq!(v[0], 0);
    qemu_test_harness::kassert_eq!(v[15], 15);
}

register_test!(ALLOCATOR_BASIC, "allocator_basic", allocator_basic);

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
