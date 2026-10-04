//! Топология CPU: разбор узла `/cpus` и нумерация процессоров.
//!
//! Логический идентификатор - индекс в таблице, аппаратный - affinity.

use fdt::{
    devicetree::{DeviceTree, Node},
    devicetreeext::NodeExt,
};
use io::Vec;

const AFFINITY_MASK: u64 = 0x0000_00FF_00FF_FFFF;

const DEFAULT_ADDRESS_CELLS: usize = 2;

/// Аппаратный идентификатор CPU: поле affinity, которым ARM-биндинг `cpus`
/// адресует процессор.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct CpuAffinity(u64);

impl CpuAffinity {
    /// Строит affinity из значения `reg` узла `cpu@N`, отбрасывая биты вне
    /// поля affinity.
    pub const fn from_reg(reg: u64) -> Self {
        // ARM-биндинг `cpus` кладёт `Aff3` в биты [39:32], а `Aff2:Aff1:Aff0` - в [23:0].
        Self(reg & AFFINITY_MASK)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Таблица CPU: индекс - логический идентификатор, значение - affinity.
/// Ёмкость `N` задаёт владелец per-CPU таблиц.
pub struct CpuTopology<const N: usize> {
    entries: Vec<CpuAffinity, N>,
}

impl<const N: usize> CpuTopology<N> {
    /// Строит таблицу из доступных узлов `/cpus`, ставя `boot` на индекс `0`.
    /// Boot CPU попадает в таблицу и в отсутствие описывающего его узла.
    pub fn parse(device_tree: &DeviceTree<'_>, boot: CpuAffinity) -> Self {
        let mut entries: Vec<CpuAffinity, N> = Vec::new();
        let _ = entries.push(boot);

        if let Some(cpus) = device_tree.find("/cpus")
            && let Some(address_cells) = address_cells(&cpus)
        {
            for node in cpus.children() {
                let Some(affinity) = cpu_affinity(&node, address_cells) else {
                    continue;
                };
                if entries.iter().any(|known| *known == affinity) {
                    continue;
                }
                if entries.push(affinity).is_none() {
                    break;
                }
            }
        }

        Self { entries }
    }

    /// Число CPU в таблице. Не меньше единицы (boot CPU есть всегда).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Affinity CPU с логическим идентификатором `index`.
    pub fn affinity(&self, index: usize) -> Option<CpuAffinity> {
        self.entries.get(index).copied()
    }

    /// Логический идентификатор CPU по его affinity.
    pub fn index_of(&self, affinity: CpuAffinity) -> Option<usize> {
        self.entries.iter().position(|known| *known == affinity)
    }

    /// Affinity всех CPU в порядке логических идентификаторов.
    pub fn iter(&self) -> impl Iterator<Item = CpuAffinity> + '_ {
        self.entries.iter().copied()
    }

    /// Подтверждает, что `affinity` принадлежит CPU с идентификатором `claim`.
    ///
    /// Стартующий CPU не получает параметров и берёт идентификатор из общего
    /// слота. Собственное affinity - единственная достоверная информация о
    /// стартующем CPU. Взятый идентификатор сверяется с топологией.
    /// Расхождение значит, что слот уже описывает другой CPU.
    pub fn confirms(&self, affinity: CpuAffinity, claim: usize) -> bool {
        self.index_of(affinity) == Some(claim)
    }
}

/// Affinity, которым узел `cpu@N` адресует процессор. `None` - узел не
/// описывает доступный CPU либо `/cpus` объявил неверный `#address-cells`.
pub fn node_affinity(cpus: &Node<'_>, node: &Node<'_>) -> Option<CpuAffinity> {
    cpu_affinity(node, address_cells(cpus)?)
}

fn address_cells(cpus: &Node<'_>) -> Option<usize> {
    match cpus
        .prop("#address-cells")
        .and_then(|prop| prop.try_as_u32(0))
    {
        Some(1) => Some(1),
        Some(2) => Some(2),
        // Значение вне пары {1, 2}: узел описан неверно, и разбирать его `reg` нечем.
        Some(_) => None,
        // Devicetree Specification §2.3.5: отсутствие свойства значит два. Для
        // `/cpus` оно обязательно (§3.7), и путь достижим только на битом DT.
        None => Some(DEFAULT_ADDRESS_CELLS),
    }
}

fn cpu_affinity(node: &Node<'_>, address_cells: usize) -> Option<CpuAffinity> {
    if node.prop("device_type")?.as_cstr()? != "cpu" {
        return None;
    }
    if !node.is_available() {
        return None;
    }

    let reg = node.prop("reg")?;
    let raw = match address_cells {
        2 => reg.try_as_u64(0)?,
        _ => u64::from(reg.try_as_u32(0)?),
    };
    Some(CpuAffinity::from_reg(raw))
}

#[cfg(test)]
mod tests {
    extern crate alloc;

    use alloc::{vec, vec::Vec as StdVec};
    use core::slice;

    use fdt::devicetree::DeviceTree;
    use fdt_test::Token::{Bytes, Cells, End, Node, Strings};

    use super::*;

    struct CpuNode {
        name: &'static str,
        reg: StdVec<u8>,
        device_type: Option<&'static str>,
        status: Option<&'static str>,
    }

    impl CpuNode {
        fn new(name: &'static str, reg: StdVec<u8>) -> Self {
            Self {
                name,
                reg,
                device_type: Some("cpu"),
                status: None,
            }
        }

        fn with_status(mut self, status: &'static str) -> Self {
            self.status = Some(status);
            self
        }

        fn without_device_type(mut self) -> Self {
            self.device_type = None;
            self
        }
    }

    fn reg_1(value: u32) -> StdVec<u8> {
        value.to_be_bytes().to_vec()
    }

    fn reg_2(value: u64) -> StdVec<u8> {
        value.to_be_bytes().to_vec()
    }

    fn build_cpus_dtb(address_cells: Option<u32>, cpus: &[CpuNode]) -> StdVec<u8> {
        let address_cells = address_cells.map(|cells| [cells]);
        let mut tokens = vec![Node("cpus")];
        if let Some(cells) = &address_cells {
            tokens.push(Cells("#address-cells", cells));
        }

        for cpu in cpus {
            tokens.push(Node(cpu.name));
            if let Some(device_type) = &cpu.device_type {
                tokens.push(Strings("device_type", slice::from_ref(device_type)));
            }
            if let Some(status) = &cpu.status {
                tokens.push(Strings("status", slice::from_ref(status)));
            }
            tokens.push(Bytes("reg", &cpu.reg));
            tokens.push(End);
        }

        tokens.push(End);
        fdt_test::build(&tokens)
    }

    const TEST_CPUS: usize = 16;

    fn parse(bytes: &[u8], boot: u64) -> CpuTopology<TEST_CPUS> {
        let tree = DeviceTree::from_bytes(bytes).expect("valid dtb");
        CpuTopology::parse(&tree, CpuAffinity::from_reg(boot))
    }

    #[test]
    fn numbers_cpus_in_devicetree_order() {
        let bytes = build_cpus_dtb(
            Some(1),
            &[
                CpuNode::new("cpu@0", reg_1(0)),
                CpuNode::new("cpu@1", reg_1(1)),
                CpuNode::new("cpu@2", reg_1(2)),
                CpuNode::new("cpu@3", reg_1(3)),
            ],
        );

        let topology = parse(&bytes, 0);
        let affinities = [0, 1, 2, 3].map(|index| topology.affinity(index));
        let indices = [0, 1, 2, 3].map(|reg| topology.index_of(CpuAffinity::from_reg(reg)));

        assert_eq!(topology.len(), 4);
        assert_eq!(
            affinities,
            [0, 1, 2, 3].map(|reg| Some(CpuAffinity::from_reg(reg)))
        );
        assert_eq!(indices, [Some(0), Some(1), Some(2), Some(3)]);
    }

    #[test]
    fn boot_cpu_takes_index_zero_regardless_of_node_order() {
        let bytes = build_cpus_dtb(
            Some(1),
            &[
                CpuNode::new("cpu@0", reg_1(0)),
                CpuNode::new("cpu@1", reg_1(1)),
                CpuNode::new("cpu@2", reg_1(2)),
            ],
        );

        let topology = parse(&bytes, 2);

        assert_eq!(topology.affinity(0), Some(CpuAffinity::from_reg(2)));
        assert_eq!(topology.index_of(CpuAffinity::from_reg(2)), Some(0));
        assert_eq!(topology.len(), 3, "the boot CPU is not counted twice");
    }

    #[test]
    fn distinguishes_cpus_of_different_clusters() {
        let cpus: StdVec<CpuNode> = (0..2u64)
            .flat_map(|cluster| {
                (0..4u64).map(move |core| {
                    CpuNode::new(
                        "cpu",
                        reg_1(u32::try_from((cluster << 8) | core).expect("affinity fits")),
                    )
                })
            })
            .collect();
        let bytes = build_cpus_dtb(Some(1), &cpus);

        let topology = parse(&bytes, 0x0100);

        assert_eq!(topology.len(), 8);
        assert_eq!(
            topology.index_of(CpuAffinity::from_reg(0x0100)),
            Some(0),
            "the boot CPU of the second cluster gets index 0"
        );
        assert_ne!(
            topology.index_of(CpuAffinity::from_reg(0x0000)),
            topology.index_of(CpuAffinity::from_reg(0x0100)),
            "CPUs of different clusters sharing Aff0 stay distinct"
        );
    }

    #[test]
    fn reads_two_cell_reg() {
        let bytes = build_cpus_dtb(
            Some(2),
            &[
                CpuNode::new("cpu@0", reg_2(0x0000_0000)),
                CpuNode::new("cpu@100000000", reg_2(0x0000_0001_0000_0000)),
            ],
        );

        let topology = parse(&bytes, 0);

        assert_eq!(topology.len(), 2);
        assert_eq!(
            topology.affinity(1),
            Some(CpuAffinity::from_reg(0x0000_0001_0000_0000)),
            "Aff3 lives in the high cell"
        );
    }

    #[test]
    fn skips_disabled_and_foreign_nodes() {
        let bytes = build_cpus_dtb(
            Some(1),
            &[
                CpuNode::new("cpu@0", reg_1(0)),
                CpuNode::new("cpu@1", reg_1(1)).with_status("disabled"),
                CpuNode::new("cpu-map", reg_1(2)).without_device_type(),
                CpuNode::new("cpu@3", reg_1(3)).with_status("okay"),
            ],
        );

        let topology = parse(&bytes, 0);

        assert_eq!(topology.len(), 2);
        assert_eq!(topology.index_of(CpuAffinity::from_reg(1)), None);
        assert_eq!(topology.index_of(CpuAffinity::from_reg(2)), None);
        assert_eq!(topology.index_of(CpuAffinity::from_reg(3)), Some(1));
    }

    #[test]
    fn boot_cpu_is_present_without_cpus_node() {
        let bytes = fdt_test::build(&[]);

        let topology = parse(&bytes, 0x0003);

        assert_eq!(topology.len(), 1);
        assert_eq!(topology.affinity(0), Some(CpuAffinity::from_reg(3)));
    }

    #[test]
    fn caps_table_at_max_cpus() {
        let cpus: StdVec<CpuNode> = (0..u32::try_from(TEST_CPUS).expect("fits") + 4)
            .map(|core| CpuNode::new("cpu", reg_1(core)))
            .collect();
        let bytes = build_cpus_dtb(Some(1), &cpus);

        let topology = parse(&bytes, 0);

        assert_eq!(topology.len(), TEST_CPUS);
    }

    #[test]
    fn drops_bits_outside_the_affinity_field() {
        let bytes = build_cpus_dtb(Some(1), &[CpuNode::new("cpu@1", reg_1(1))]);

        let topology = parse(&bytes, 0xC000_0001);

        assert_eq!(
            topology.len(),
            1,
            "the boot CPU affinity matches the FDT node"
        );
        assert_eq!(topology.affinity(0), Some(CpuAffinity::from_reg(1)));
    }

    #[test]
    fn duplicate_reg_is_counted_once() {
        let bytes = build_cpus_dtb(
            Some(1),
            &[
                CpuNode::new("cpu@1", reg_1(1)),
                CpuNode::new("cpu@1-dup", reg_1(1)),
            ],
        );

        let topology = parse(&bytes, 0);

        assert_eq!(topology.len(), 2, "the boot CPU and one node");
        assert_eq!(topology.index_of(CpuAffinity::from_reg(1)), Some(1));
    }

    #[test]
    fn unsupported_address_cells_leaves_only_boot_cpu() {
        let bytes = build_cpus_dtb(Some(3), &[CpuNode::new("cpu@1", reg_1(1))]);

        let topology = parse(&bytes, 0);

        assert_eq!(topology.len(), 1);
        assert_eq!(topology.index_of(CpuAffinity::from_reg(1)), None);
    }

    #[test]
    fn absent_address_cells_reads_reg_as_two_cells() {
        let bytes = build_cpus_dtb(None, &[CpuNode::new("cpu@5", reg_2(5))]);

        let topology = parse(&bytes, 0);

        assert_eq!(topology.index_of(CpuAffinity::from_reg(5)), Some(1));
        assert_eq!(
            vec![0u64, 5],
            topology.iter().map(CpuAffinity::raw).collect::<StdVec<_>>()
        );
    }

    #[test]
    fn absent_address_cells_rejects_single_cell_reg() {
        let bytes = build_cpus_dtb(None, &[CpuNode::new("cpu@5", reg_1(5))]);

        let topology = parse(&bytes, 0);

        assert_eq!(topology.len(), 1, "the two-cell parse must not succeed");
        assert_eq!(topology.index_of(CpuAffinity::from_reg(5)), None);
    }

    #[test]
    fn confirms_the_identifier_taken_by_the_starting_cpu() {
        let bytes = build_cpus_dtb(
            Some(1),
            &[
                CpuNode::new("cpu@0", reg_1(0)),
                CpuNode::new("cpu@1", reg_1(1)),
                CpuNode::new("cpu@2", reg_1(2)),
            ],
        );

        let topology = parse(&bytes, 0);

        assert!(topology.confirms(CpuAffinity::from_reg(2), 2));
    }

    #[test]
    fn rejects_an_identifier_belonging_to_another_cpu() {
        let bytes = build_cpus_dtb(
            Some(1),
            &[
                CpuNode::new("cpu@0", reg_1(0)),
                CpuNode::new("cpu@1", reg_1(1)),
                CpuNode::new("cpu@2", reg_1(2)),
            ],
        );

        let topology = parse(&bytes, 0);

        assert!(
            !topology.confirms(CpuAffinity::from_reg(2), 1),
            "a foreign identifier shares the stack and the per-CPU block with another CPU"
        );
    }

    #[test]
    fn rejects_an_identifier_for_a_cpu_outside_the_topology() {
        let bytes = build_cpus_dtb(Some(1), &[CpuNode::new("cpu@0", reg_1(0))]);

        let topology = parse(&bytes, 0);

        assert!(!topology.confirms(CpuAffinity::from_reg(9), 0));
    }
}
