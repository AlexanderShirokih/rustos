use std::cell::Cell;

use drivers_common::{
    DeviceNode, DriverDescriptor, NodeProperty,
    probe::{ProbeContext, ProbeError, ProbeResult},
    scanner::{EmbeddedDriversScanner, MAX_DEVICE_TREE_DEPTH, ScanError},
};

thread_local! {
    static VISITED: Cell<usize> = const { Cell::new(0) };
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
