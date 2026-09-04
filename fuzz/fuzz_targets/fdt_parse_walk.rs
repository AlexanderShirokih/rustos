#![no_main]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::missing_asserts_for_indexing,
    clippy::cast_possible_truncation,
    clippy::unwrap_in_result,
    clippy::panic,
    clippy::std_instead_of_core
)]

use fdt::devicetree::DeviceTree;
use libfuzzer_sys::fuzz_target;

const NODE_LIMIT: usize = 4096;
const PROPERTY_LIMIT: usize = 256;

fuzz_target!(|data: &[u8]| {
    let Ok(tree) = DeviceTree::from_bytes(data) else {
        return;
    };
    // Продакшн-вход - from_ptr с окном ровно в total_size; обрезаем до него,
    // иначе смещения за концом дерева читаются, а на устройстве отсекаются.
    let Some(exact) = data.get(..tree.size()) else {
        return;
    };
    let Ok(tree) = DeviceTree::from_bytes(exact) else {
        return;
    };

    let _ = tree.size();
    for reservation in tree.memory_reservations().take(NODE_LIMIT) {
        let _ = (reservation.address, reservation.size);
    }

    let Some(root) = tree.root() else {
        return;
    };
    let mut nodes = vec![root];
    let mut visited = 0;
    while let Some(node) = nodes.pop() {
        visited += 1;
        if visited > NODE_LIMIT {
            break;
        }

        let _ = (node.name(), node.key());
        for property in node.properties().take(PROPERTY_LIMIT) {
            let _ = (
                property.name(),
                property.value(),
                property.as_cstr(),
                property.try_as_u32(0),
                property.try_as_u64(0),
                property.as_usize(),
            );
        }
        let remaining = NODE_LIMIT.saturating_sub(visited + nodes.len());
        nodes.extend(node.children().take(remaining));
    }
});
