use fdt::devicetree::{DeviceTree, Node};

/// Находит узел консоли из stdout-path или первый serial.
pub fn find_console<'dt>(device_tree: &'dt DeviceTree<'dt>) -> Option<Node<'dt>> {
    stdout_node(device_tree).or_else(|| first_serial(device_tree))
}

fn stdout_node<'dt>(device_tree: &'dt DeviceTree<'dt>) -> Option<Node<'dt>> {
    let chosen = device_tree.find("/chosen")?;
    let raw = chosen.prop("stdout-path")?;
    let path = raw.as_cstr()?;
    let trimmed = path.split(':').next().unwrap_or(path);
    device_tree.find(trimmed)
}

fn first_serial<'dt>(device_tree: &'dt DeviceTree<'dt>) -> Option<Node<'dt>> {
    device_tree.nodes().find(|node| {
        let by_device_type = node
            .prop("device_type")
            .and_then(|prop| prop.as_cstr())
            .map(|value| value == "serial")
            .unwrap_or(false);

        let by_name = node.name().starts_with("serial");
        let by_compat = compatible_strings(node)
            .any(|value| value.contains("serial") || value.contains("uart"));

        by_device_type || by_name || by_compat
    })
}

/// Итерирует по null-terminated строкам из свойства `compatible`.
fn compatible_strings<'a>(node: &'a Node<'a>) -> impl Iterator<Item = &'a str> {
    let data = node.prop("compatible").map(|p| p.value()).unwrap_or(&[]);

    data.split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .filter_map(|bytes| core::str::from_utf8(bytes).ok())
}
