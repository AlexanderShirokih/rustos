use std::cell::Cell;

use drivers_common::{
    DeviceNode, DriverDescriptor, NodeProperty,
    probe::{ProbeContext, ProbeError, ProbeResult},
    scanner::{EmbeddedDriversScanner, MAX_DEVICE_TREE_DEPTH, ScanError},
};

thread_local! {
    static VISITED: Cell<usize> = const { Cell::new(0) };
    static PROBED_IDS: Cell<u32> = const { Cell::new(0) };
}

#[derive(Clone, Copy)]
struct EmptyProperty;

impl NodeProperty for EmptyProperty {
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
struct ChainNode {
    id: usize,
    max: usize,
}

struct ChainChildren {
    next: Option<ChainNode>,
}

impl Iterator for ChainChildren {
    type Item = ChainNode;

    fn next(&mut self) -> Option<Self::Item> {
        self.next.take()
    }
}

impl DeviceNode for ChainNode {
    type Id = usize;
    type Property<'a>
        = EmptyProperty
    where
        Self: 'a;
    type ChildIter<'a>
        = ChainChildren
    where
        Self: 'a;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn name(&self) -> &'static str {
        "chain"
    }

    fn prop(&self, _name: &str) -> Option<Self::Property<'_>> {
        None
    }

    fn children(&self) -> Self::ChildIter<'_> {
        ChainChildren {
            next: (self.id < self.max).then_some(ChainNode {
                id: self.id + 1,
                max: self.max,
            }),
        }
    }
}

fn count_probe(context: &mut ProbeContext<ChainNode>) -> ProbeResult {
    assert_eq!(context.hierarchy().len(), context.node().id() + 1);
    VISITED.with(|visited| visited.set(visited.get() + 1));
    Err(ProbeError::Unsupported("count only"))
}

#[test]
fn scanner_accepts_tree_at_depth_limit() {
    VISITED.with(|visited| visited.set(0));

    let mut scanner = EmbeddedDriversScanner::new();
    let drivers = [DriverDescriptor {
        name: "counter",
        probe: count_probe as fn(&mut ProbeContext<ChainNode>) -> ProbeResult,
    }];

    let result = scanner.scan_and_probe(
        ChainNode {
            id: 0,
            max: MAX_DEVICE_TREE_DEPTH - 1,
        },
        &drivers,
    );

    assert_eq!(result, Ok(()));
    VISITED.with(|visited| assert_eq!(visited.get(), MAX_DEVICE_TREE_DEPTH));
}

#[derive(Clone, Copy)]
struct StatusProperty {
    raw: &'static [u8],
}

impl NodeProperty for StatusProperty {
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
struct StatusNode {
    id: u32,
    status: Option<&'static [u8]>,
    children: &'static [StatusNode],
}

impl DeviceNode for StatusNode {
    type Id = usize;
    type Property<'a>
        = StatusProperty
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
            self.status.map(|raw| StatusProperty { raw })
        } else {
            None
        }
    }

    fn children(&self) -> Self::ChildIter<'_> {
        self.children.iter().copied()
    }
}

fn record_id_probe(context: &mut ProbeContext<StatusNode>) -> ProbeResult {
    let id = context.node().id;
    PROBED_IDS.with(|set| set.set(set.get() | (1 << id)));
    Err(ProbeError::Unsupported("count only"))
}

#[test]
fn scanner_skips_disabled_nodes_and_their_subtrees() {
    PROBED_IDS.with(|set| set.set(0));

    let okay_leaf = StatusNode {
        id: 4,
        status: Some(b"okay\0"),
        children: &[],
    };
    let disabled_child_of_disabled_bus = StatusNode {
        id: 3,
        status: Some(b"okay\0"),
        children: &[],
    };
    let disabled_bus = StatusNode {
        id: 2,
        status: Some(b"disabled\0"),
        children: core::slice::from_ref(Box::leak(Box::new(disabled_child_of_disabled_bus))),
    };
    let okay_implicit = StatusNode {
        id: 1,
        status: None,
        children: &[],
    };
    let root = StatusNode {
        id: 0,
        status: None,
        children: Box::leak(Box::new([okay_implicit, disabled_bus, okay_leaf])),
    };

    let mut scanner = EmbeddedDriversScanner::new();
    let drivers = [DriverDescriptor {
        name: "spy",
        probe: record_id_probe as fn(&mut ProbeContext<StatusNode>) -> ProbeResult,
    }];
    scanner
        .scan_and_probe(root, &drivers)
        .expect("tree well within depth limit");

    let probed_mask = PROBED_IDS.with(std::cell::Cell::get);
    let expect = (1u32 << 0) | (1u32 << 1) | (1u32 << 4);
    assert_eq!(
        probed_mask, expect,
        "scanner must skip disabled node id=2 together with its child id=3 \
         (probed_mask={probed_mask:#b}, expected={expect:#b})"
    );
}

#[test]
fn scanner_rejects_tree_deeper_than_limit() {
    VISITED.with(|visited| visited.set(0));

    let mut scanner = EmbeddedDriversScanner::new();
    let drivers = [DriverDescriptor {
        name: "counter",
        probe: count_probe as fn(&mut ProbeContext<ChainNode>) -> ProbeResult,
    }];

    let result = scanner.scan_and_probe(
        ChainNode {
            id: 0,
            max: MAX_DEVICE_TREE_DEPTH,
        },
        &drivers,
    );

    assert_eq!(
        result,
        Err(ScanError::TreeTooDeep {
            limit: MAX_DEVICE_TREE_DEPTH,
            attempted_depth: MAX_DEVICE_TREE_DEPTH + 1,
        })
    );
    VISITED.with(|visited| assert_eq!(visited.get(), MAX_DEVICE_TREE_DEPTH));
}
