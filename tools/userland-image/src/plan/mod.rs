mod binding;
mod executable;
mod limits;
mod names;
mod source;

#[cfg(test)]
mod tests;

use anyhow::{Context, Result, anyhow};
use userland_image::{ComponentSpec, encode_plan};

use self::{
    binding::{Consumer, Providers, compile_requirements},
    executable::resolve_executables,
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
    pub manifest: &'a ProgramManifest,
}

pub(crate) fn compile_plan(source: &str, programs: &[PlanProgram<'_>]) -> Result<Vec<u8>> {
    let manifest = parse_system_manifest(source)?;
    let entries: Vec<&ComponentEntry> = manifest.components.iter().collect();

    let executables = resolve_executables(&entries, programs)?;
    let names = resolve_names(&entries)?;
    let declarations: Vec<&ProgramManifest> =
        executables.iter().map(|program| program.manifest).collect();
    validate_composition(&names, &declarations)?;

    // Требования плана ссылаются на эти строки.
    let compatible: Vec<Vec<Vec<&str>>> = declarations
        .iter()
        .map(|declared| {
            declared
                .requirements
                .iter()
                .map(|requirement| requirement.names.iter().map(String::as_str).collect())
                .collect()
        })
        .collect();
    let providers = Providers {
        names: &names,
        declarations: &declarations,
    };
    let requirements = entries
        .iter()
        .zip(&declarations)
        .zip(&compatible)
        .zip(&names)
        .enumerate()
        .map(|(index, (((entry, declared), compatible), name))| {
            compile_requirements(
                &Consumer {
                    index,
                    name,
                    entry,
                    declared,
                    compatible,
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
        .zip(&names)
        .map(|(entry, name)| {
            parse_memory_limit(&entry.memory_limit)
                .with_context(|| format!("boot plan component '{name}' sets 'memory_limit'"))
        })
        .collect::<Result<Vec<_>>>()?;

    let components: Vec<ComponentSpec<'_>> = entries
        .iter()
        .zip(&executables)
        .zip(&names)
        .zip(&requirements)
        .zip(&exports)
        .zip(&memory_limits)
        .map(
            |(((((entry, program), name), requirements), exports), &memory_limit)| ComponentSpec {
                executable: program.path,
                name,
                requirements,
                exports,
                memory_limit,
                restarts: entry.restarts,
            },
        )
        .collect();
    encode_plan(&components).map_err(|err| anyhow!("failed to serialize boot plan: {err:?}"))
}
