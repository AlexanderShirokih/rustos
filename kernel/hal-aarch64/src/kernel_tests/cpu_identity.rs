//! Проводка идентификации CPU: разбор `/cpus` покрыт host-тестами
//! `hal-common`, здесь проверяется, что построенная таблица доехала до
//! рантайма и что идентификатор приходит из per-CPU блока.

use kernel_tests::kernel_test;
use scheduler::{ArchCpu, CpuId};

use crate::{cpu, sched::Aarch64Cpu};

#[kernel_test]
fn boot_cpu_owns_logical_id_zero() {
    let local = cpu::current().expect("per-CPU block is installed during boot");

    assert_eq!(local.id(), CpuId::ZERO, "boot CPU takes index 0");
    assert_eq!(Aarch64Cpu::current_id(), CpuId::ZERO);
}

#[kernel_test]
fn topology_resolves_current_affinity() {
    let affinity = cpu::current_affinity();

    assert_eq!(
        cpu::logical_id(affinity),
        Some(CpuId::ZERO),
        "affinity of the current CPU resolves through the topology table"
    );
}

#[kernel_test]
fn scheduler_state_is_reachable_through_cpu_local() {
    let local = cpu::current().expect("per-CPU block is installed during boot");

    assert!(
        !local.scheduler().is_null(),
        "scheduler bootstrap is complete"
    );
    assert_eq!(local.scheduler(), Aarch64Cpu::cpu_local_ptr());
}
