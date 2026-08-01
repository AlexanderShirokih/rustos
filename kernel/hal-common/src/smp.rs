//! Способ старта вторичного CPU, объявленный в device tree.
//!
//! Разбор биндинга; сам вызов делает платформенный слой.

use fdt::devicetree::{DeviceTree, Node};

const PSCI_CPU_ON_AARCH64: u32 = 0xC400_0003;

/// Соглашение о вызове PSCI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PsciConduit {
    /// Вызов гипервизора.
    Hvc,
    /// Вызов защищённого монитора.
    Smc,
}

/// Способ старта: общий для всех CPU либо заданный узлом конкретного CPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartMethod {
    /// PSCI `CPU_ON`.
    Psci { conduit: PsciConduit, cpu_on: u32 },
    /// Spin-table: точка входа пишется по физическому адресу.
    SpinTable { release_address: usize },
}

/// Читает способ старта из узла `/psci`. Общий для всех CPU.
pub fn psci_method(device_tree: &DeviceTree<'_>) -> Option<StartMethod> {
    let psci = device_tree.find("/psci")?;

    let conduit = match psci.prop("method").and_then(|prop| prop.as_cstr())? {
        "hvc" => PsciConduit::Hvc,
        "smc" => PsciConduit::Smc,
        _ => return None,
    };

    // Идентификатор функции объявляется узлом; в его отсутствие берётся
    // стандартный для 64-битного соглашения.
    let cpu_on = psci
        .prop("cpu_on")
        .and_then(|prop| prop.try_as_u32(0))
        .unwrap_or(PSCI_CPU_ON_AARCH64);

    Some(StartMethod::Psci { conduit, cpu_on })
}

/// Читает способ старта из узла `cpu@N`. Задаёт spin-table для этого CPU.
pub fn node_method(node: &Node<'_>) -> Option<StartMethod> {
    if node.prop("enable-method").and_then(|prop| prop.as_cstr())? != "spin-table" {
        return None;
    }

    let release_address = node
        .prop("cpu-release-addr")
        .and_then(|prop| prop.try_as_u64(0))?;

    Some(StartMethod::SpinTable {
        release_address: release_address as usize,
    })
}

#[cfg(test)]
mod tests {
    extern crate alloc;

    use alloc::vec::Vec as StdVec;

    use super::*;
    use crate::test_util::{
        StringPool, begin_node, build_fdt, end_node, end_tree, push_prop_bytes,
    };

    fn cstr(value: &str) -> StdVec<u8> {
        let mut bytes = value.as_bytes().to_vec();
        bytes.push(0);
        bytes
    }

    fn build_psci_dtb(method: Option<&str>, cpu_on: Option<u32>) -> StdVec<u8> {
        let mut pool = StringPool::new();
        let method_off = pool.intern("method");
        let cpu_on_off = pool.intern("cpu_on");

        let mut structure = StdVec::new();
        begin_node(&mut structure, "");
        begin_node(&mut structure, "psci");
        if let Some(method) = method {
            push_prop_bytes(&mut structure, method_off, &cstr(method));
        }
        if let Some(cpu_on) = cpu_on {
            push_prop_bytes(&mut structure, cpu_on_off, &cpu_on.to_be_bytes());
        }
        end_node(&mut structure);
        end_node(&mut structure);
        end_tree(&mut structure);

        build_fdt(&structure, pool.bytes())
    }

    fn build_cpu_dtb(enable_method: Option<&str>, release: Option<u64>) -> StdVec<u8> {
        let mut pool = StringPool::new();
        let enable_off = pool.intern("enable-method");
        let release_off = pool.intern("cpu-release-addr");

        let mut structure = StdVec::new();
        begin_node(&mut structure, "");
        begin_node(&mut structure, "cpus");
        begin_node(&mut structure, "cpu@1");
        if let Some(enable_method) = enable_method {
            push_prop_bytes(&mut structure, enable_off, &cstr(enable_method));
        }
        if let Some(release) = release {
            push_prop_bytes(&mut structure, release_off, &release.to_be_bytes());
        }
        end_node(&mut structure);
        end_node(&mut structure);
        end_node(&mut structure);
        end_tree(&mut structure);

        build_fdt(&structure, pool.bytes())
    }

    fn cpu_node_method(bytes: &[u8]) -> Option<StartMethod> {
        let tree = DeviceTree::from_bytes(bytes).expect("valid dtb");
        let cpus = tree.find("/cpus")?;
        cpus.children().find_map(|node| node_method(&node))
    }

    #[test]
    fn reads_hypervisor_conduit() {
        let bytes = build_psci_dtb(Some("hvc"), None);
        let tree = DeviceTree::from_bytes(&bytes).expect("valid dtb");

        assert_eq!(
            psci_method(&tree),
            Some(StartMethod::Psci {
                conduit: PsciConduit::Hvc,
                cpu_on: PSCI_CPU_ON_AARCH64,
            })
        );
    }

    #[test]
    fn reads_monitor_conduit_with_declared_function_id() {
        let bytes = build_psci_dtb(Some("smc"), Some(0xC400_00AA));
        let tree = DeviceTree::from_bytes(&bytes).expect("valid dtb");

        assert_eq!(
            psci_method(&tree),
            Some(StartMethod::Psci {
                conduit: PsciConduit::Smc,
                cpu_on: 0xC400_00AA,
            })
        );
    }

    #[test]
    fn unknown_conduit_leaves_no_method() {
        let bytes = build_psci_dtb(Some("ioctl"), None);
        let tree = DeviceTree::from_bytes(&bytes).expect("valid dtb");

        assert_eq!(psci_method(&tree), None);
    }

    #[test]
    fn absent_psci_node_leaves_no_method() {
        let bytes = build_cpu_dtb(None, None);
        let tree = DeviceTree::from_bytes(&bytes).expect("valid dtb");

        assert_eq!(psci_method(&tree), None);
    }

    #[test]
    fn reads_spin_table_release_address() {
        let bytes = build_cpu_dtb(Some("spin-table"), Some(0x4000_0000));

        assert_eq!(
            cpu_node_method(&bytes),
            Some(StartMethod::SpinTable {
                release_address: 0x4000_0000,
            })
        );
    }

    #[test]
    fn spin_table_without_release_address_is_rejected() {
        let bytes = build_cpu_dtb(Some("spin-table"), None);

        assert_eq!(cpu_node_method(&bytes), None);
    }

    #[test]
    fn foreign_enable_method_is_rejected() {
        let bytes = build_cpu_dtb(Some("psci"), Some(0x4000_0000));

        assert_eq!(cpu_node_method(&bytes), None);
    }
}
