use anyhow::{Result, ensure};
use filesystem_abi::NAME_MAX;
use syscall::MAX_PROCESS_NAME_LEN;

use super::source::ComponentEntry;

pub(super) fn resolve_names<'a>(entries: &[&'a ComponentEntry]) -> Result<Vec<&'a str>> {
    let names: Vec<&str> = entries
        .iter()
        .map(|component| match &component.name {
            Some(name) => name.as_str(),
            // Умолчание имени экземпляра - идентичность программы.
            None => component.executable.as_str(),
        })
        .collect();
    for (index, name) in names.iter().enumerate() {
        validate_name(name)?;
        ensure!(
            !names.iter().take(index).any(|earlier| earlier == name),
            "boot plan names component '{name}' more than once"
        );
    }
    Ok(names)
}

pub(crate) fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty(),
        "boot plan component name must not be empty"
    );
    ensure!(
        name.is_ascii() && !name.contains('/'),
        "boot plan component name '{name}' must be ASCII without '/'"
    );
    // Именем экземпляра зовётся его процесс. Имя длиннее ёмкости `ProcessCreate`
    // не поднялось бы на устройстве.
    ensure!(
        name.len() <= MAX_PROCESS_NAME_LEN,
        "boot plan component name '{name}' is longer than \
         {MAX_PROCESS_NAME_LEN} bytes, which a process name holds"
    );
    Ok(())
}

pub(super) fn validate_record_name(name: &str) -> Result<()> {
    // Компонент ступени 1 назван записью образа, и её имя - один компонент пути.
    ensure!(
        name.len() <= NAME_MAX,
        "stage 1 component name '{name}' is longer than {NAME_MAX} bytes, \
         which an image record name holds"
    );
    Ok(())
}
