mod binding;
mod executable;
mod limits;
mod names;
mod order;
mod policy;
mod source;

#[cfg(test)]
mod tests;

use anyhow::{Result, anyhow, ensure};
use supervisor_abi::PLAN_RECORD_MAX;
use userland_image::{ComponentSpec, encode_plan};

pub(crate) use self::names::validate_name;
use self::{
    binding::compile_slots,
    executable::resolve_executables,
    limits::validate_composition,
    names::{resolve_names, validate_record_name},
    order::{resolve_after, validate_acyclic, validate_stage_order},
    policy::compile_policy,
    source::{ComponentEntry, SystemManifest, parse_system_manifest},
};
use crate::manifest::ProgramManifest;

#[derive(Clone, Copy, Debug)]
pub(crate) struct PlanProgram<'a> {
    pub package: &'a str,
    pub bin: &'a str,
    pub path: &'a str,
    pub manifest: &'a ProgramManifest,
}

#[derive(Debug)]
pub(crate) struct CompiledStages {
    pub plan: Vec<u8>,
    pub stage_one: Vec<StageOneRecord>,
    pub instances: StageInstances,
}

#[derive(Debug, Default)]
pub(crate) struct StageInstances {
    pub stage_zero: Vec<String>,
    pub stage_one: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct StageOneRecord {
    pub name: String,
    pub bytes: Vec<u8>,
}

struct Instance<'a> {
    entry: &'a ComponentEntry,
    program: PlanProgram<'a>,
    name: &'a str,
}

struct Composition<'a> {
    boundary: usize,
    instances: Vec<Instance<'a>>,
    selectors: Vec<Vec<Vec<&'a str>>>,
}

impl<'a> Composition<'a> {
    fn resolve(
        stage_zero: &'a SystemManifest,
        stage_one: Option<&'a SystemManifest>,
        programs: &[PlanProgram<'a>],
    ) -> Result<Self> {
        let boundary = stage_zero.components.len();
        // Связывание идёт над объединением ступеней: слот ступени 1 называет экспортёра
        // ступени 0 именем экземпляра, и имя занято набором целиком.
        let entries: Vec<&ComponentEntry> = stage_zero
            .components
            .iter()
            .chain(
                stage_one
                    .into_iter()
                    .flat_map(|stage| stage.components.iter()),
            )
            .collect();

        let executables = resolve_executables(&entries, programs)?;
        let names = resolve_names(&entries)?;
        let declarations: Vec<&ProgramManifest> =
            executables.iter().map(|program| program.manifest).collect();
        validate_composition(&names, &declarations)?;
        for name in &names[boundary..] {
            validate_record_name(name)?;
        }

        // Строки совместимости переживают слоты, которые на них ссылаются: слот
        // плана несёт срез объявления, а не копию.
        let selectors: Vec<Vec<Vec<&str>>> = declarations
            .iter()
            .map(|declared| {
                declared
                    .slots
                    .iter()
                    .map(|slot| slot.names.iter().map(String::as_str).collect())
                    .collect()
            })
            .collect();

        let instances = entries
            .into_iter()
            .zip(executables)
            .zip(names)
            .map(|((entry, program), name)| Instance {
                entry,
                program,
                name,
            })
            .collect();

        Ok(Self {
            boundary,
            instances,
            selectors,
        })
    }

    fn names(&self) -> Vec<&'a str> {
        self.instances
            .iter()
            .map(|instance| instance.name)
            .collect()
    }

    fn declarations(&self) -> Vec<&'a ProgramManifest> {
        self.instances
            .iter()
            .map(|instance| instance.program.manifest)
            .collect()
    }

    fn compile(&self) -> Result<CompiledStages> {
        let names = self.names();
        let declarations = self.declarations();
        let bindings = self
            .instances
            .iter()
            .zip(&self.selectors)
            .map(|(instance, selectors)| {
                compile_slots(
                    instance.name,
                    instance.program.manifest,
                    selectors,
                    &declarations,
                    &names,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let after = self
            .instances
            .iter()
            .map(|instance| resolve_after(instance.entry, instance.name, &names))
            .collect::<Result<Vec<_>>>()?;
        validate_acyclic(&bindings, &after, &names)?;
        validate_stage_order(self.boundary, &bindings, &after, &names)?;

        let after_names: Vec<Vec<&str>> = after
            .iter()
            .map(|edges| edges.iter().map(|&edge| names[edge]).collect())
            .collect();
        let policies = self
            .instances
            .iter()
            .map(|instance| compile_policy(instance.entry, instance.name))
            .collect::<Result<Vec<_>>>()?;
        let components: Vec<ComponentSpec<'_>> = (0..self.instances.len())
            .map(|index| ComponentSpec {
                executable: self.instances[index].program.path,
                name: names[index],
                slots: &bindings[index].slots,
                after: &after_names[index],
                memory_pages: policies[index].memory_pages,
                startup_timeout: policies[index].startup_timeout,
                launches: policies[index].launches,
                on_exhausted: policies[index].on_exhausted,
                establishes_sessions: declarations[index].establishes_sessions,
            })
            .collect();

        let plan = encode_plan(&components[..self.boundary])
            .map_err(|err| anyhow!("failed to serialize boot plan: {err:?}"))?;
        Ok(CompiledStages {
            plan,
            stage_one: encode_stage_one(&components[self.boundary..])?,
            instances: StageInstances {
                stage_zero: owned_names(&names[..self.boundary]),
                stage_one: owned_names(&names[self.boundary..]),
            },
        })
    }
}

pub(crate) fn compile_stages(
    plan_source: &str,
    stage_one_source: Option<&str>,
    programs: &[PlanProgram<'_>],
) -> Result<CompiledStages> {
    let stage_zero = parse_system_manifest(plan_source)?;
    let stage_one = stage_one_source.map(parse_system_manifest).transpose()?;

    Composition::resolve(&stage_zero, stage_one.as_ref(), programs)?.compile()
}

fn encode_stage_one(components: &[ComponentSpec<'_>]) -> Result<Vec<StageOneRecord>> {
    components
        .iter()
        .map(|component| {
            let name = component.name;
            let bytes = encode_plan(core::slice::from_ref(component))
                .map_err(|err| anyhow!("failed to serialize stage 1 record: {err:?}"))?;
            // Запись едет супервизору полем кадра `define`, и его ёмкость - потолок ступени 1.
            ensure!(
                bytes.len() <= PLAN_RECORD_MAX,
                "stage 1 component '{name}' encodes to {} bytes; \
                 the extension transaction carries {PLAN_RECORD_MAX}",
                bytes.len()
            );
            Ok(StageOneRecord {
                name: String::from(name),
                bytes,
            })
        })
        .collect()
}

fn owned_names(names: &[&str]) -> Vec<String> {
    names.iter().copied().map(String::from).collect()
}
