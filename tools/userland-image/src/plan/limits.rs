use anyhow::{Result, ensure};
use component_abi::COMPONENT_SLOT_MAX;
use syscall::{HANDLE_TABLE_CAPACITY, MAX_RESOURCE_DEPTH, WAIT_MANY_MAX_COUNT};

use crate::manifest::ProgramManifest;

const BOOT_COMPOSITION_DEPTH: u16 = 0;

pub(super) const COMPONENT_RESOURCE_LEVELS: u16 = 3;
pub(super) const SUPERVISOR_HANDLES: usize = 24;
pub(super) const SUPERVISOR_WAITS: usize = 2;
pub(super) const COMPONENT_HANDLES: usize = 13;

pub(super) fn validate_composition(
    names: &[&str],
    declarations: &[&ProgramManifest],
) -> Result<()> {
    for (name, declared) in names.iter().zip(declarations) {
        validate_component(name, declared)?;
    }
    validate_supervisor_limits(names.len(), declarations)
}

fn validate_component(name: &str, declared: &ProgramManifest) -> Result<()> {
    ensure!(
        declared.slots.len() <= COMPONENT_SLOT_MAX,
        "boot plan component '{name}' declares {} slots; \
         the startup transaction carries {COMPONENT_SLOT_MAX}",
        declared.slots.len()
    );
    ensure!(
        !declared.establishes_sessions || declared.export.is_some(),
        "boot plan component '{name}' establishes its own sessions without exporting a protocol"
    );
    // Кадр порта разбирает один dispatch, и незнакомый ordinal он отвергает. Два протокола
    // на одном порту не складываются, а установление сессий занимает control-порт.
    if let Some(control) = &declared.control {
        ensure!(
            !declared.establishes_sessions,
            "boot plan component '{name}' serves control protocol '{control}' and establishes \
             its own sessions; the control port carries one protocol"
        );
    }
    validate_resource_depth(name, COMPONENT_RESOURCE_LEVELS)
}

pub(super) fn validate_resource_depth(name: &str, levels: u16) -> Result<()> {
    ensure!(
        BOOT_COMPOSITION_DEPTH + levels <= MAX_RESOURCE_DEPTH,
        "boot plan component '{name}' needs {levels} resource levels \
         under depth {BOOT_COMPOSITION_DEPTH}; the ceiling is {MAX_RESOURCE_DEPTH}"
    );
    Ok(())
}

fn validate_supervisor_limits(count: usize, declarations: &[&ProgramManifest]) -> Result<()> {
    // Supervisor ждёт процессы компонентов вместе с потоком relay и сигналом порта
    // транзакции расширения одним массивом ожидания. Набор ступени 1 пополняет тот же
    // массив, и потолок держит обе ступени сразу.
    let waits = count + SUPERVISOR_WAITS;
    ensure!(
        waits <= WAIT_MANY_MAX_COUNT,
        "boot plan composition of {count} components needs {waits} supervisor wait objects; \
         the ceiling is {WAIT_MANY_MAX_COUNT}"
    );

    let slots: usize = declarations
        .iter()
        .map(|declared| declared.slots.len())
        .sum();
    // Оценка сверху: постоянные капы supervisor, постоянные капы каждого компонента и
    // по капе на слот. Временные капы подъёма существуют не дольше одного компонента.
    let handles = SUPERVISOR_HANDLES + count * COMPONENT_HANDLES + slots;
    ensure!(
        handles <= HANDLE_TABLE_CAPACITY as usize,
        "boot plan composition of {count} components needs up to {handles} rootkeeper handles; \
         the table holds {HANDLE_TABLE_CAPACITY}"
    );
    Ok(())
}
