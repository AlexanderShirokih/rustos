use drivers_common::tree::{DeviceNode, DeviceTreeSource, NodeProperty};

#[derive(Clone, Copy, Debug)]
struct TestProp<'a> {
    name: &'a str,
    raw: &'a [u8],
}

impl NodeProperty for TestProp<'_> {
    fn name(&self) -> &str {
        self.name
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
struct TestNode<'a> {
    id: usize,
    name: &'a str,
    props: &'a [(&'a str, &'a [u8])],
    children: &'a [TestNode<'a>],
}

impl DeviceNode for TestNode<'_> {
    type Id = usize;
    type Property<'a>
        = TestProp<'a>
    where
        Self: 'a;
    type ChildIter<'a>
        = core::iter::Copied<core::slice::Iter<'a, Self>>
    where
        Self: 'a;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn name(&self) -> &str {
        self.name
    }

    fn prop(&self, name: &str) -> Option<Self::Property<'_>> {
        self.props
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(n, raw)| TestProp { name: n, raw })
    }

    fn children(&self) -> Self::ChildIter<'_> {
        self.children.iter().copied()
    }
}

struct TestTree<'a> {
    root: TestNode<'a>,
}

impl DeviceTreeSource for TestTree<'_> {
    type Node<'a>
        = TestNode<'a>
    where
        Self: 'a;
    type NodeIter<'a>
        = core::iter::Once<TestNode<'a>>
    where
        Self: 'a;

    fn root(&self) -> Option<Self::Node<'_>> {
        Some(self.root)
    }

    fn nodes(&self) -> Self::NodeIter<'_> {
        core::iter::once(self.root)
    }
}

#[test]
fn device_tree_source_provides_root_and_children() {
    let child = TestNode {
        id: 2,
        name: "serial@1000",
        props: &[],
        children: &[],
    };
    let root = TestNode {
        id: 1,
        name: "",
        props: &[],
        children: &[child],
    };
    let tree = TestTree { root };

    let uart = tree.root().unwrap().children().next().unwrap();
    assert_eq!(uart.name(), "serial@1000");
}
