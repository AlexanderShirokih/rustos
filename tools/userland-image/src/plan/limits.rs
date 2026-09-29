use anyhow::{Result, ensure};
use component_abi::{PROTOCOL_ID_MAX, RequirementKind};
use syscall::{HANDLE_TABLE_CAPACITY, IPC_BUFFER_MAX_CAPS, MAX_SEGMENTS_PER_IMG};

use crate::manifest::ProgramManifest;

const BASE: usize = 7;
const LAUNCH: usize = MAX_SEGMENTS_PER_IMG + 3;
const ROOTKEEPER_THREADS: usize = 2;
const FRAME: usize = ROOTKEEPER_THREADS * (1 + IPC_BUFFER_MAX_CAPS);

pub(super) const ROOTKEEPER_HANDLES: usize = BASE + LAUNCH + FRAME;
pub(super) const CHILD_HANDLES: usize = 3;

pub(super) fn validate_composition(
    names: &[&str],
    declarations: &[&ProgramManifest],
) -> Result<()> {
    for (name, declared) in names.iter().zip(declarations) {
        validate_component(name, declared)?;
    }
    // Запись 0 rootkeeper не запускает, и в оценку она не входит.
    validate_rootkeeper_limits(declarations.get(1..).unwrap_or_default())
}

fn validate_component(name: &str, declared: &ProgramManifest) -> Result<()> {
    for export in &declared.exports {
        ensure!(
            export.len() <= PROTOCOL_ID_MAX,
            "boot plan component '{name}' exports protocol '{export}' of {} bytes; \
             a protocol id holds at most {PROTOCOL_ID_MAX}",
            export.len()
        );
    }
    Ok(())
}

pub(super) fn rootkeeper_handles(declarations: &[&ProgramManifest]) -> usize {
    // Оценка сверху. `BASE` покрывает порт `Bootstrap`, сессию `Hardware`, ресурс, регионы образа,
    // порты `ComponentStartup` и журнала и поток relay. `LAUNCH` покрывает
    // один запуск программы, и rootkeeper запускает программы по очереди. `FRAME` покрывает
    // принятый кадр на каждый принимающий поток. Запись компонента держит `Process`, резерв и
    // источник журнала в `CHILD_HANDLES`, а также по заявке на требование сессии, по `accept` на
    // экспорт и копии выдач incarnation. Копию сессии `Hardware` на требование к записи 0
    // покрывает слот требования сессии.
    let per_component: usize = declarations
        .iter()
        .map(|declared| {
            let session_requirements = count(declared, |kind| kind == RequirementKind::Session);
            let held = 1 + count(declared, |kind| kind != RequirementKind::Session);
            CHILD_HANDLES + session_requirements + declared.exports.len() + held
        })
        .sum();
    ROOTKEEPER_HANDLES + per_component
}

fn count(declared: &ProgramManifest, kind: impl Fn(RequirementKind) -> bool) -> usize {
    declared
        .requirements
        .iter()
        .filter(|requirement| kind(requirement.kind))
        .count()
}

fn validate_rootkeeper_limits(declarations: &[&ProgramManifest]) -> Result<()> {
    let handles = rootkeeper_handles(declarations);
    ensure!(
        handles <= HANDLE_TABLE_CAPACITY as usize,
        "boot plan composition of {} components requires up to {handles} rootkeeper handles; \
         the table holds {HANDLE_TABLE_CAPACITY}",
        declarations.len()
    );
    Ok(())
}
