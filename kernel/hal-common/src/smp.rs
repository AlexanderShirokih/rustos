//! Подъём вторичных CPU: способ старта, объявленный в device tree, и учёт
//! поднятых.
//!
//! Разбор биндинга. Сам вызов делает платформенный слой.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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

/// Отказ `CPU_ON`. Успех сюда не входит. Его выражает `Ok` разбора.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PsciFailure {
    NotSupported,
    InvalidParameters,
    Denied,
    AlreadyOn,
    OnPending,
    InternalFailure,
    NotPresent,
    Disabled,
    InvalidAddress,
    /// Код вне множества, определённого спецификацией.
    Unknown(i32),
}

impl PsciFailure {
    /// Разбирает код возврата `CPU_ON`.
    pub fn from_code(code: i32) -> Result<(), Self> {
        match code {
            0 => Ok(()),
            -1 => Err(Self::NotSupported),
            -2 => Err(Self::InvalidParameters),
            -3 => Err(Self::Denied),
            -4 => Err(Self::AlreadyOn),
            -5 => Err(Self::OnPending),
            -6 => Err(Self::InternalFailure),
            -7 => Err(Self::NotPresent),
            -8 => Err(Self::Disabled),
            -9 => Err(Self::InvalidAddress),
            other => Err(Self::Unknown(other)),
        }
    }

    /// `true`, если после такого ответа целевой CPU может исполнять код.
    ///
    /// Отказ, названный спецификацией, делится надвое. Одни коды описывают
    /// вызов, отклонённый до всякого действия: CPU остался выключенным, и
    /// переданные ему параметры свободны. Другие говорят, что CPU уже
    /// поднимается или что итог неизвестен - тогда параметры остаются за ним.
    pub fn may_have_started(self) -> bool {
        match self {
            // CPU уже включён или включается - возможно, по переданной точке входа.
            Self::AlreadyOn
            | Self::OnPending
            // Отказ реализации не называет шаг, на котором он произошёл.
            | Self::InternalFailure
            // Код вне спецификации не позволяет судить об итоге.
            | Self::Unknown(_) => true,
            // Вызов отклонён до старта: CPU не тронут.
            Self::NotSupported
            | Self::InvalidParameters
            | Self::Denied
            | Self::NotPresent
            | Self::Disabled
            | Self::InvalidAddress => false,
        }
    }
}

impl core::fmt::Display for PsciFailure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotSupported => f.write_str("NOT_SUPPORTED"),
            Self::InvalidParameters => f.write_str("INVALID_PARAMETERS"),
            Self::Denied => f.write_str("DENIED"),
            Self::AlreadyOn => f.write_str("ALREADY_ON"),
            Self::OnPending => f.write_str("ON_PENDING"),
            Self::InternalFailure => f.write_str("INTERNAL_FAILURE"),
            Self::NotPresent => f.write_str("NOT_PRESENT"),
            Self::Disabled => f.write_str("DISABLED"),
            Self::InvalidAddress => f.write_str("INVALID_ADDRESS"),
            Self::Unknown(code) => write!(f, "unspecified code {code}"),
        }
    }
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

/// Учёт вторичных CPU, доложивших о готовности, и признак открытого приёма.
pub struct OnlineCpus {
    count: AtomicUsize,
    accepting: AtomicBool,
}

impl OnlineCpus {
    pub const fn new() -> Self {
        Self {
            count: AtomicUsize::new(0),
            accepting: AtomicBool::new(true),
        }
    }

    /// Отмечает CPU дошедшим до работы. `false` - приём уже закрыт, и в работу
    /// этот CPU не идёт.
    pub fn mark_online(&self) -> bool {
        if !self.accepting.load(Ordering::Acquire) {
            return false;
        }

        self.count.fetch_add(1, Ordering::Release);
        true
    }

    /// Закрывает приём. Вызывается boot CPU по завершении подъёма.
    pub fn close(&self) {
        self.accepting.store(false, Ordering::Release);
    }

    /// Число CPU, доложивших о готовности.
    pub fn count(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }
}

impl Default for OnlineCpus {
    fn default() -> Self {
        Self::new()
    }
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
        release_address: usize::try_from(release_address).ok()?,
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

    #[test]
    fn readiness_is_counted_while_the_bring_up_accepts() {
        let online = OnlineCpus::new();

        assert_eq!(online.count(), 0);
        assert!(online.mark_online());
        assert!(online.mark_online());
        assert_eq!(online.count(), 2);
    }

    #[test]
    fn readiness_after_the_bring_up_was_closed_is_refused() {
        let online = OnlineCpus::new();
        assert!(online.mark_online());

        online.close();

        assert!(!online.mark_online());
        assert_eq!(online.count(), 1);
    }

    #[test]
    fn reads_specified_status_codes() {
        assert_eq!(PsciFailure::from_code(0), Ok(()));
        assert_eq!(
            PsciFailure::from_code(-2),
            Err(PsciFailure::InvalidParameters)
        );
        assert_eq!(PsciFailure::from_code(-4), Err(PsciFailure::AlreadyOn));
        assert_eq!(PsciFailure::from_code(-9), Err(PsciFailure::InvalidAddress));
    }

    #[test]
    fn code_outside_the_specification_stays_unknown() {
        assert_eq!(PsciFailure::from_code(-42), Err(PsciFailure::Unknown(-42)));
        assert_eq!(PsciFailure::from_code(7), Err(PsciFailure::Unknown(7)));
    }

    #[test]
    fn rejection_before_start_leaves_the_cpu_untouched() {
        for status in [
            PsciFailure::NotSupported,
            PsciFailure::InvalidParameters,
            PsciFailure::Denied,
            PsciFailure::NotPresent,
            PsciFailure::Disabled,
            PsciFailure::InvalidAddress,
        ] {
            assert!(
                !status.may_have_started(),
                "{status} must not claim the CPU"
            );
        }
    }

    #[test]
    fn unfinished_or_unclear_outcome_claims_the_cpu() {
        for status in [
            PsciFailure::AlreadyOn,
            PsciFailure::OnPending,
            PsciFailure::InternalFailure,
            PsciFailure::Unknown(-42),
        ] {
            assert!(status.may_have_started(), "{status} must claim the CPU");
        }
    }
}
