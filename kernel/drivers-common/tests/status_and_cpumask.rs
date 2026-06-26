//! Покрытие `is_node_enabled` (через scanner) для значений `status`
//! `"ok"`/`"okay"`/`"fail*"`/мусор и `CpuMask::cpu` overflow сдвига.

use std::cell::Cell;

use drivers_common::{
    DeviceNode, DriverDescriptor, NodeProperty,
    probe::{ProbeContext, ProbeError, ProbeResult},
    scanner::EmbeddedDriversScanner,
    services::interrupts::CpuMask,
};

thread_local! {
    static PROBED: Cell<u32> = const { Cell::new(0) };
}

#[derive(Clone, Copy)]
struct StatusProp {
    raw: &'static [u8],
}

impl NodeProperty for StatusProp {
    fn name(&self) -> &'static str {
        "status"
    }
    fn raw(&self) -> &[u8] {
        self.raw
    }
    fn as_cstr(&self) -> Option<&str> {
        let len = self.raw.len().saturating_sub(1);
        core::str::from_utf8(&self.raw[..len]).ok()
    }
}

#[derive(Clone, Copy)]
struct Node {
    id: u32,
    status: Option<&'static [u8]>,
    children: &'static [Node],
}

impl DeviceNode for Node {
    type Id = usize;
    type Property<'a>
        = StatusProp
    where
        Self: 'a;
    type ChildIter<'a>
        = core::iter::Copied<core::slice::Iter<'a, Self>>
    where
        Self: 'a;

    fn id(&self) -> Self::Id {
        self.id as usize
    }
    fn name(&self) -> &'static str {
        "node"
    }
    fn prop(&self, name: &str) -> Option<Self::Property<'_>> {
        if name == "status" {
            self.status.map(|raw| StatusProp { raw })
        } else {
            None
        }
    }
    fn children(&self) -> Self::ChildIter<'_> {
        self.children.iter().copied()
    }
}

fn record(ctx: &mut ProbeContext<Node>) -> ProbeResult {
    let id = ctx.node().id() as u32;
    PROBED.with(|p| p.set(p.get() | (1 << id)));
    Err(ProbeError::Unsupported("record"))
}

/// Возвращает маску id узлов, по которым прошёл probe, для дерева
/// root(id=0) с одним потомком (id=1) с заданным `status`.
fn probed_mask_for_status(status: Option<&'static [u8]>) -> u32 {
    PROBED.with(|p| p.set(0));

    let child = Node {
        id: 1,
        status,
        children: &[],
    };
    let root = Node {
        id: 0,
        status: None,
        children: Box::leak(Box::new([child])),
    };

    let drivers = [DriverDescriptor {
        name: "rec",
        probe: record as fn(&mut ProbeContext<Node>) -> ProbeResult,
    }];
    let mut scanner = EmbeddedDriversScanner::new();
    scanner.scan_and_probe(root, &drivers).unwrap();
    PROBED.with(Cell::get)
}

#[test]
fn status_okay_and_ok_are_enabled() {
    // root всегда probed (id=0); потомок (id=1) probed только если enabled.
    assert_eq!(probed_mask_for_status(Some(b"okay\0")), 0b11);
    assert_eq!(probed_mask_for_status(Some(b"ok\0")), 0b11);
    // Отсутствие status -> enabled.
    assert_eq!(probed_mask_for_status(None), 0b11);
}

#[test]
fn status_fail_and_garbage_are_disabled() {
    // "fail", "fail-sss", "disabled", мусор -> узел пропускается (только root).
    assert_eq!(probed_mask_for_status(Some(b"fail\0")), 0b01);
    assert_eq!(probed_mask_for_status(Some(b"fail-sss\0")), 0b01);
    assert_eq!(probed_mask_for_status(Some(b"disabled\0")), 0b01);
    assert_eq!(probed_mask_for_status(Some(b"garbage\0")), 0b01);
    // Не-UTF8 значение -> as_cstr == None -> не "okay"/"ok" -> disabled.
    assert_eq!(probed_mask_for_status(Some(&[0xFF, 0xFE, 0x00])), 0b01);
}

#[test]
fn cpu_mask_cpu_sets_single_bit() {
    assert_eq!(CpuMask::cpu(0), Some(CpuMask::CPU0));
    assert_eq!(CpuMask::cpu(7).map(CpuMask::raw), Some(0b1000_0000));
}

#[test]
fn cpu_mask_cpu_overflow_returns_none() {
    // Сдвиг u8 на >= 8 переполняется -> None (защита checked_shl).
    assert_eq!(CpuMask::cpu(8), None);
    assert_eq!(CpuMask::cpu(255), None);
}
