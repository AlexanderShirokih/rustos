#![allow(clippy::std_instead_of_core)]

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

    fn is_available(&self) -> bool {
        true
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
    let mut scanner = EmbeddedDriversScanner::new(None);
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
    let visited = VISITED.with(Cell::get);

    assert_eq!(result, Ok(()));
    assert_eq!(visited, MAX_DEVICE_TREE_DEPTH);
}

#[derive(Clone, Copy)]
struct AvailabilityNode {
    id: u32,
    available: bool,
    children: &'static [AvailabilityNode],
}

impl DeviceNode for AvailabilityNode {
    type Id = usize;
    type Property<'a>
        = EmptyProperty
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

    fn prop(&self, _name: &str) -> Option<Self::Property<'_>> {
        None
    }

    fn is_available(&self) -> bool {
        self.available
    }

    fn children(&self) -> Self::ChildIter<'_> {
        self.children.iter().copied()
    }
}

fn record_id_probe(context: &mut ProbeContext<AvailabilityNode>) -> ProbeResult {
    let id = context.node().id;
    PROBED_IDS.with(|set| set.set(set.get() | (1 << id)));
    Err(ProbeError::Unsupported("count only"))
}

#[test]
fn scanner_skips_unavailable_nodes_and_their_subtrees() {
    PROBED_IDS.with(|set| set.set(0));
    let available_leaf = AvailabilityNode {
        id: 4,
        available: true,
        children: &[],
    };
    let available_child_of_unavailable_bus = AvailabilityNode {
        id: 3,
        available: true,
        children: &[],
    };
    let unavailable_bus = AvailabilityNode {
        id: 2,
        available: false,
        children: core::slice::from_ref(Box::leak(Box::new(available_child_of_unavailable_bus))),
    };
    let available_node = AvailabilityNode {
        id: 1,
        available: true,
        children: &[],
    };
    let root = AvailabilityNode {
        id: 0,
        available: true,
        children: Box::leak(Box::new([available_node, unavailable_bus, available_leaf])),
    };
    let mut scanner = EmbeddedDriversScanner::new(None);
    let drivers = [DriverDescriptor {
        name: "spy",
        probe: record_id_probe as fn(&mut ProbeContext<AvailabilityNode>) -> ProbeResult,
    }];

    scanner
        .scan_and_probe(root, &drivers)
        .expect("tree well within depth limit");
    let probed_mask = PROBED_IDS.with(Cell::get);

    let expect = (1u32 << 0) | (1u32 << 1) | (1u32 << 4);
    assert_eq!(
        probed_mask, expect,
        "scanner must skip unavailable node id=2 together with its child id=3 \
         (probed_mask={probed_mask:#b}, expected={expect:#b})"
    );
}

#[test]
fn scanner_rejects_tree_deeper_than_limit() {
    VISITED.with(|visited| visited.set(0));
    let mut scanner = EmbeddedDriversScanner::new(None);
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
    let visited = VISITED.with(Cell::get);

    assert_eq!(
        result,
        Err(ScanError::TreeTooDeep {
            limit: MAX_DEVICE_TREE_DEPTH,
            attempted_depth: MAX_DEVICE_TREE_DEPTH + 1,
        })
    );
    assert_eq!(visited, MAX_DEVICE_TREE_DEPTH);
}
