use std::iter;

use anyhow::{Result, ensure};
use syscall::MAX_PROCESS_NAME_LEN;

use super::source::ComponentEntry;

pub(super) fn resolve_names<'a>(
    boot: &'a str,
    entries: &[&'a ComponentEntry],
) -> Result<Vec<&'a str>> {
    let names: Vec<&str> = iter::once(boot)
        .chain(entries.iter().map(|component| match &component.name {
            Some(name) => name.as_str(),
            // Умолчание имени экземпляра - идентичность программы.
            None => component.executable.as_str(),
        }))
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

fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty(),
        "boot plan component name must not be empty"
    );
    ensure!(
        name.is_ascii() && !name.contains('/'),
        "boot plan component name '{name}' must be ASCII without '/'"
    );
    // Имя экземпляра становится именем его процесса, а `ProcessCreate` отвергает имя длиннее
    // `MAX_PROCESS_NAME_LEN`.
    ensure!(
        name.len() <= MAX_PROCESS_NAME_LEN,
        "boot plan component name '{name}' is longer than \
         {MAX_PROCESS_NAME_LEN} bytes, which a process name holds"
    );
    Ok(())
}
