mod binding;
mod executable;
mod limits;
mod names;
mod source;

#[cfg(test)]
mod tests;

use std::iter;

use anyhow::{Context, Result, anyhow};
use userland_image::{ComponentSpec, component_name, encode_plan};

use self::{
    binding::{Consumer, Providers, compile_requirements},
    executable::{boot_program, resolve_executables},
    limits::validate_composition,
    names::resolve_names,
    source::{ComponentEntry, parse_system_manifest},
};
use crate::{manifest::ProgramManifest, units::parse_memory_limit};

#[derive(Clone, Copy, Debug)]
pub(crate) struct PlanProgram<'a> {
    pub package: &'a str,
    pub bin: &'a str,
    pub path: &'a str,
    /// Загрузочная программа образа. План ставит её записью 0.
    pub boot: bool,
    pub manifest: &'a ProgramManifest,
}

pub(crate) fn compile_plan(source: &str, programs: &[PlanProgram<'_>]) -> Result<Vec<u8>> {
    let manifest = parse_system_manifest(source)?;
    let entries: Vec<&ComponentEntry> = manifest.components.iter().collect();

    let boot = boot_program(programs)?;
    let executables = resolve_executables(&entries, programs)?;
    // Запись 0 плана - загрузочная программа, записи композиции идут с номера 1. Имя и
    // манифест записи 0 участвуют в проверке имён и в поиске провайдеров наравне с остальными.
    // Ядро называет загрузочный процесс по каталогу его программы, и запись 0 называется так же.
    let boot_name = component_name(boot.path);
    let names = resolve_names(boot_name, &entries)?;
    let declarations: Vec<&ProgramManifest> = iter::once(boot.manifest)
        .chain(executables.iter().map(|program| program.manifest))
        .collect();
    validate_composition(&names, &declarations)?;

    let providers = Providers {
        names: &names,
        declarations: &declarations,
    };
    let requirements = entries
        .iter()
        .zip(declarations.iter().skip(1))
        .zip(names.iter().skip(1))
        .enumerate()
        .map(|(index, ((entry, declared), name))| {
            compile_requirements(
                &Consumer {
                    index: index + 1,
                    name,
                    entry,
                    declared,
                },
                &providers,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let exports: Vec<Vec<&str>> = declarations
        .iter()
        .map(|declared| declared.exports.iter().map(String::as_str).collect())
        .collect();
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
        requirements: &[],
        exports: exports.first().map_or(&[], Vec::as_slice),
        memory_limit: 0,
        restarts: 0,
    };
    let components: Vec<ComponentSpec<'_>> = iter::once(record_zero)
        .chain(
            entries
                .iter()
                .zip(&executables)
                .zip(names.iter().skip(1))
                .zip(&requirements)
                .zip(exports.iter().skip(1))
                .zip(&memory_limits)
                .map(
                    |(((((entry, program), name), requirements), exports), &memory_limit)| {
                        ComponentSpec {
                            executable: program.path,
                            name,
                            requirements,
                            exports,
                            memory_limit,
                            restarts: entry.restarts,
                        }
                    },
                ),
        )
        .collect();
    encode_plan(&components).map_err(|err| anyhow!("failed to serialize boot plan: {err:?}"))
}
