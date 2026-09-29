mod executable;
mod limits;
mod names;
mod source;

#[cfg(test)]
mod tests;

use std::iter;

use anyhow::{Context, Result, anyhow, ensure};
use component_abi::{ComponentManifest, decode_manifest};
use ipc::Protocol as _;
use userland::{ProgramView, decode_program};
use userland_image::{ComponentSpec, component_name, encode_plan};

use self::{
    executable::{boot_program, resolve_executables},
    limits::{validate_composition, validate_rootkeeper_limits},
    names::resolve_names,
    source::{ComponentEntry, parse_system_manifest},
};
use crate::units::parse_memory_limit;

#[derive(Clone, Copy, Debug)]
pub(crate) struct PlanProgram<'a> {
    pub package: &'a str,
    pub bin: &'a str,
    pub path: &'a str,
    /// Загрузочная программа образа. План ставит её записью 0.
    pub boot: bool,
    pub data: &'a [u8],
}

impl<'a> PlanProgram<'a> {
    fn manifest(&self) -> Result<ComponentManifest<'a>> {
        let program = decode_program(self.data)
            .map_err(|error| anyhow!("program '{}' is invalid: {error:?}", self.bin))?;
        decode_manifest(program.manifest()).map_err(|error| {
            anyhow!(
                "program '{}' carries an invalid manifest: {error:?}",
                self.bin
            )
        })
    }
}

pub(crate) fn compile_plan(source: &str, programs: &[PlanProgram<'_>]) -> Result<Vec<u8>> {
    let manifest = parse_system_manifest(source)?;
    let entries: Vec<&ComponentEntry> = manifest.components.iter().collect();

    let boot = boot_program(programs)?;
    let executables = resolve_executables(&entries, programs)?;
    // Запись 0 плана - загрузочная программа, записи композиции идут с номера 1. Имя и
    // манифест записи 0 участвуют в проверке имён и экспортов наравне с остальными.
    // Ядро называет загрузочный процесс по каталогу его программы, и запись 0 называется так же.
    let boot_name = component_name(boot.path);
    let names = resolve_names(boot_name, &entries)?;
    let declarations: Vec<_> = iter::once(boot)
        .chain(executables.iter().copied())
        .map(|program| program.manifest())
        .collect::<Result<_>>()?;
    validate_composition(&names, &declarations)?;
    for (entry, declared) in entries.iter().zip(declarations.iter().skip(1)) {
        if let Some(path) = &entry.mountpoint {
            ensure!(
                filesystem_abi::Path::new(path).is_ok(),
                "mountpoint must be an absolute path within PATH_MAX"
            );
            ensure!(
                declared
                    .exports()
                    .any(|protocol| protocol == filesystem_abi::file_system::Protocol::ID),
                "component with mountpoint must export FileSystem"
            );
        }
    }

    let memory_limits = entries
        .iter()
        .zip(names.iter().skip(1))
        .map(|(entry, name)| {
            parse_memory_limit(&entry.memory_limit)
                .with_context(|| format!("boot plan component '{name}' sets 'memory_limit'"))
        })
        .collect::<Result<Vec<_>>>()?;

    let record_zero = ComponentSpec {
        executable: boot.path,
        name: boot_name,
        mountpoint: None,
        memory_limit: 0,
        restarts: 0,
    };

    let components: Vec<ComponentSpec<'_>> = iter::once(record_zero)
        .chain(
            entries
                .iter()
                .zip(&executables)
                .zip(names.iter().skip(1))
                .zip(&memory_limits)
                .map(|(((entry, program), name), &memory_limit)| ComponentSpec {
                    executable: program.path,
                    name,
                    mountpoint: entry.mountpoint.as_deref(),
                    memory_limit,
                    restarts: entry.restarts,
                }),
        )
        .collect();

    validate_rootkeeper_limits(&declarations)?;

    encode_plan(&components).map_err(|err| anyhow!("failed to serialize boot plan: {err:?}"))
}
