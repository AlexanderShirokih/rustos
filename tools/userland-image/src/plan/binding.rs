use anyhow::{Context, Result, bail, ensure};
use component_abi::{LOG_REQUIREMENT, RequirementKind, SESSION_SCOPE_MAX};
use userland_image::{RequirementSourceSpec, RequirementSpec};

use crate::{
    manifest::{ManifestRequirement, ProgramManifest},
    plan::source::ComponentEntry,
};

const BIND_KEY: &str = "bind";
const SCOPE_KEY: &str = "scope";

const BOOT_RECORD: usize = 0;

pub(super) struct Consumer<'a> {
    pub(super) index: usize,
    pub(super) name: &'a str,
    pub(super) entry: &'a ComponentEntry,
    pub(super) declared: &'a ProgramManifest,
}

pub(super) struct Providers<'p> {
    pub(super) names: &'p [&'p str],
    pub(super) declarations: &'p [&'p ProgramManifest],
}

pub(super) fn compile_requirements<'a>(
    consumer: &Consumer<'a>,
    providers: &Providers<'_>,
) -> Result<Vec<RequirementSpec<'a>>> {
    validate_keys(consumer)?;
    consumer
        .declared
        .requirements
        .iter()
        .map(|requirement| {
            let source = match requirement.kind {
                RequirementKind::Region => RequirementSourceSpec::Region,
                RequirementKind::Session => session(consumer, requirement, providers)?,
            };
            Ok(RequirementSpec {
                name: &requirement.name,
                source,
            })
        })
        .collect()
}

fn session<'a>(
    consumer: &Consumer<'a>,
    requirement: &'a ManifestRequirement,
    providers: &Providers<'_>,
) -> Result<RequirementSourceSpec<'a>> {
    let name = consumer.name;
    let protocol = requirement.protocol.as_deref().with_context(|| {
        format!(
            "boot plan component '{name}' declares session requirement '{}' without a protocol",
            requirement.name
        )
    })?;
    let provider = match consumer.entry.bind.get(&requirement.name) {
        Some(instance) => {
            bound_provider(consumer, &requirement.name, protocol, instance, providers)?
        }
        None => only_provider(consumer, &requirement.name, protocol, providers)?,
    };
    let scope = consumer.entry.scope.get(&requirement.name);
    // Экспорт записи 0 выдаёт сам rootkeeper без заявки провайдеру, и область читать некому.
    if provider == BOOT_RECORD && scope.is_some() {
        bail!(
            "boot plan component '{name}' sets '{SCOPE_KEY}' on requirement '{}', \
             which the boot program '{}' provides",
            requirement.name,
            providers
                .names
                .get(BOOT_RECORD)
                .copied()
                .unwrap_or_default()
        );
    }
    Ok(RequirementSourceSpec::Export {
        provider: u16::try_from(provider)
            .context("boot plan resolves a provider past the count of a plan record")?,
        protocol,
        scope: scope.map_or("", String::as_str),
    })
}

fn bound_provider(
    consumer: &Consumer<'_>,
    requirement: &str,
    protocol: &str,
    instance: &str,
    providers: &Providers<'_>,
) -> Result<usize> {
    let name = consumer.name;
    let index = providers
        .names
        .iter()
        .position(|known| *known == instance)
        .with_context(|| {
            format!(
                "boot plan component '{name}' binds requirement '{requirement}' to instance '{instance}', \
                 which the composition does not contain"
            )
        })?;
    ensure!(
        index != consumer.index,
        "boot plan component '{name}' binds requirement '{requirement}' to itself"
    );
    let declared = providers
        .declarations
        .get(index)
        .context("boot plan resolves a provider outside the composition")?;
    ensure!(
        exports(declared, protocol),
        "boot plan component '{name}' binds requirement '{requirement}' to instance '{instance}', \
         which exports no protocol '{protocol}'"
    );
    Ok(index)
}

fn only_provider(
    consumer: &Consumer<'_>,
    requirement: &str,
    protocol: &str,
    providers: &Providers<'_>,
) -> Result<usize> {
    let name = consumer.name;
    let exporters: Vec<usize> = providers
        .declarations
        .iter()
        .enumerate()
        .filter(|(_, declared)| exports(declared, protocol))
        .map(|(index, _)| index)
        .collect();
    let [provider] = exporters[..] else {
        ensure!(
            !exporters.is_empty(),
            "boot plan component '{name}' imports protocol '{protocol}' for requirement '{requirement}', \
             which no component of the composition exports"
        );
        let names: Vec<&str> = exporters
            .iter()
            .filter_map(|index| providers.names.get(*index).copied())
            .collect();
        bail!(
            "boot plan component '{name}' imports protocol '{protocol}' for requirement '{requirement}', \
             which {} components export: {}; name one with '{BIND_KEY}'",
            names.len(),
            names.join(", ")
        );
    };
    ensure!(
        provider != consumer.index,
        "boot plan component '{name}' imports the protocol it exports itself"
    );
    Ok(provider)
}

fn exports(declared: &ProgramManifest, protocol: &str) -> bool {
    declared.exports.iter().any(|export| export == protocol)
}

fn validate_keys(consumer: &Consumer<'_>) -> Result<()> {
    let entry = consumer.entry;
    for requirement in entry.bind.keys() {
        validate_key(consumer, BIND_KEY, requirement)?;
    }
    for (requirement, scope) in &entry.scope {
        validate_key(consumer, SCOPE_KEY, requirement)?;
        ensure!(
            !scope.is_empty() && scope.len() <= SESSION_SCOPE_MAX,
            "boot plan component '{}' sets '{SCOPE_KEY}' on requirement '{requirement}' with {} bytes; \
             a scope is non-empty and at most {SESSION_SCOPE_MAX}",
            consumer.name,
            scope.len()
        );
    }
    Ok(())
}

fn validate_key(consumer: &Consumer<'_>, key: &str, requirement: &str) -> Result<()> {
    let name = consumer.name;
    // Родитель сам выдаёт журнал, и описание композиции его не связывает.
    ensure!(
        requirement != LOG_REQUIREMENT,
        "boot plan component '{name}' sets '{key}' on requirement '{LOG_REQUIREMENT}', \
         the name reserved for the log its parent serves"
    );
    let declaration = consumer
        .declared
        .requirements
        .iter()
        .find(|declaration| declaration.name == requirement)
        .with_context(|| {
            format!(
                "boot plan component '{name}' sets '{key}' on requirement '{requirement}', \
                 which its manifest does not declare"
            )
        })?;
    ensure!(
        declaration.kind == RequirementKind::Session,
        "boot plan component '{name}' sets '{key}' on requirement '{requirement}', which takes no session"
    );
    Ok(())
}
