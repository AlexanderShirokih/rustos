//! Тесты успешного пробирования: регистрация `DriverHandle` в `handles`,
//! `IntoIterator`, Occupied-ветка (один узел не регистрируется дважды),
//! приоритет первого совпавшего драйвера.

extern crate alloc;

use alloc::{boxed::Box, string::String};

use drivers_common::{
    DeviceNode, Driver, DriverDescriptor, DriverFactory, NodeProperty,
    probe::{ProbeContext, ProbeError, ProbeResult},
    scanner::EmbeddedDriversScanner,
};

#[derive(Clone, Copy)]
struct EmptyProp;

impl NodeProperty for EmptyProp {
    fn name(&self) -> &'static str {
        ""
    }
    fn raw(&self) -> &[u8] {
        &[]
    }
    fn as_cstr(&self) -> Option<&str> {
        None
    }
}

#[derive(Clone, Copy)]
struct SimpleNode {
    id: usize,
    children: &'static [SimpleNode],
}

impl DeviceNode for SimpleNode {
    type Id = usize;
    type Property<'a>
        = EmptyProp
    where
        Self: 'a;
    type ChildIter<'a>
        = core::iter::Copied<core::slice::Iter<'a, Self>>
    where
        Self: 'a;

    fn id(&self) -> Self::Id {
        self.id
    }
    fn name(&self) -> &'static str {
        "node"
    }
    fn prop(&self, _name: &str) -> Option<Self::Property<'_>> {
        None
    }
    fn children(&self) -> Self::ChildIter<'_> {
        self.children.iter().copied()
    }
}

struct NoopDriver;
impl Driver for NoopDriver {}

struct NoopFactory;
impl DriverFactory for NoopFactory {
    fn create(&self) -> Result<Box<dyn Driver>, String> {
        Ok(Box::new(NoopDriver))
    }
}

// Сигнатура обязана совпадать с типом поля `probe` (fn -> ProbeResult),
// поэтому Result обязателен, даже если функция всегда возвращает Ok.
#[allow(clippy::unnecessary_wraps)]
fn always_match(_ctx: &mut ProbeContext<SimpleNode>) -> ProbeResult {
    Ok(Box::new(NoopFactory))
}

fn never_match(_ctx: &mut ProbeContext<SimpleNode>) -> ProbeResult {
    Err(ProbeError::Unsupported("never"))
}

#[test]
fn probe_context_fold_sums_over_hierarchy() {
    // Иерархия из трёх узлов (id=10, 20, 30). fold суммирует id всех узлов,
    // у которых есть родитель (т.е. все, кроме корня): 20 + 30 = 50.
    let n10 = SimpleNode {
        id: 10,
        children: &[],
    };
    let n20 = SimpleNode {
        id: 20,
        children: &[],
    };
    let n30 = SimpleNode {
        id: 30,
        children: &[],
    };
    let ctx = ProbeContext::new(n30, alloc::vec![n10, n20, n30]);

    let sum: usize = ctx.fold(|parent, node, _ctx| parent.map(|_| node.id()));
    assert_eq!(sum, 50);

    // parent() корня (первого в иерархии) -> None.
    assert!(ctx.parent(&n10).is_none());
    // parent() узла -> предыдущий в иерархии.
    assert_eq!(ctx.parent(&n30).map(DeviceNode::id), Some(20));
}

#[test]
fn successful_probe_registers_handle() {
    let root = SimpleNode {
        id: 0,
        children: &[],
    };
    let drivers = [DriverDescriptor {
        name: "matcher",
        probe: always_match as fn(&mut ProbeContext<SimpleNode>) -> ProbeResult,
    }];

    let mut scanner = EmbeddedDriversScanner::new();
    scanner.scan_and_probe(root, &drivers).unwrap();

    // IntoIterator выдаёт зарегистрированные хэндлы.
    let handles: Vec<_> = scanner.into_iter().collect();
    assert_eq!(handles.len(), 1);
    assert_eq!(handles[0].name, "matcher");
    // Фабрика реально создаёт драйвер.
    assert!(handles[0].factory.create().is_ok());
}

#[test]
fn first_matching_driver_wins() {
    let root = SimpleNode {
        id: 0,
        children: &[],
    };
    // Первый драйвер всегда matches; второй тоже, но не должен сработать.
    let drivers = [
        DriverDescriptor {
            name: "first",
            probe: always_match as fn(&mut ProbeContext<SimpleNode>) -> ProbeResult,
        },
        DriverDescriptor {
            name: "second",
            probe: always_match as fn(&mut ProbeContext<SimpleNode>) -> ProbeResult,
        },
    ];

    let mut scanner = EmbeddedDriversScanner::new();
    scanner.scan_and_probe(root, &drivers).unwrap();

    let handles: Vec<_> = scanner.into_iter().collect();
    assert_eq!(handles.len(), 1);
    assert_eq!(handles[0].name, "first");
}

#[test]
fn skips_non_matching_driver_then_registers_matching() {
    let root = SimpleNode {
        id: 0,
        children: &[],
    };
    let drivers = [
        DriverDescriptor {
            name: "skip",
            probe: never_match as fn(&mut ProbeContext<SimpleNode>) -> ProbeResult,
        },
        DriverDescriptor {
            name: "match",
            probe: always_match as fn(&mut ProbeContext<SimpleNode>) -> ProbeResult,
        },
    ];

    let mut scanner = EmbeddedDriversScanner::new();
    scanner.scan_and_probe(root, &drivers).unwrap();

    let handles: Vec<_> = scanner.into_iter().collect();
    assert_eq!(handles.len(), 1);
    assert_eq!(handles[0].name, "match");
}

#[test]
fn occupied_entry_keeps_first_registration_per_node() {
    // Два узла с ОДИНАКОВЫМ id: после регистрации первого, у второго ключ занят
    // (Occupied-ветка) и повторная вставка не происходит.
    static DUP: [SimpleNode; 1] = [SimpleNode {
        id: 7,
        children: &[],
    }];
    static ROOT_CHILDREN: [SimpleNode; 1] = [SimpleNode {
        id: 7,
        children: &DUP,
    }];
    let root = SimpleNode {
        id: 0,
        children: &ROOT_CHILDREN,
    };

    let drivers = [DriverDescriptor {
        name: "matcher",
        probe: always_match as fn(&mut ProbeContext<SimpleNode>) -> ProbeResult,
    }];

    let mut scanner = EmbeddedDriversScanner::new();
    scanner.scan_and_probe(root, &drivers).unwrap();

    // id=0 и id=7 -> ровно два уникальных ключа (повторный id=7 в Occupied-ветке
    // отбрасывается, но узлу id=7 на верхнем уровне хэндл уже выдан).
    let handles: Vec<_> = scanner.into_iter().collect();
    assert_eq!(handles.len(), 2);
}
