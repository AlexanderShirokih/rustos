//! Проводка идентификации CPU: разбор `/cpus` и место boot CPU в таблице
//! покрыты host-тестами `hal-common`, здесь проверяется, что построенная
//! таблица доехала до рантайма и что идентификатор приходит из per-CPU блока.
//!
//! Поток теста мигрирует между ядрами. Проверки говорят о том CPU, на котором тест
//! исполняется в этот момент.

use scheduler::ArchCpu;

use crate::{cpu, sched::Aarch64Cpu};

#[test_harness::test]
fn per_cpu_block_and_topology_agree_on_the_current_id() {
    let local = cpu::current().expect("per-CPU block is installed during boot");

    assert_eq!(
        cpu::logical_id(cpu::current_affinity()),
        Some(local.id()),
        "affinity of the current CPU resolves to the id kept in its per-CPU block"
    );
}

#[test_harness::test]
fn scheduler_state_is_reachable_through_cpu_local() {
    let local = cpu::current().expect("per-CPU block is installed during boot");

    assert!(
        !local.scheduler().is_null(),
        "scheduler bootstrap is complete"
    );
    assert_eq!(local.scheduler(), Aarch64Cpu::cpu_local_ptr());
}
