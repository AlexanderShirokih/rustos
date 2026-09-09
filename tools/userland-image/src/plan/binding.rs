use anyhow::{Context, Result, bail, ensure};
use component_abi::SlotKind;
use userland_image::{DeviceSpec, SlotSpec};

use crate::manifest::ProgramManifest;

#[derive(Clone, Copy)]
struct Provider {
    instance: usize,
    export: usize,
}

#[derive(Default)]
pub(super) struct ComponentBinding<'a> {
    pub(super) slots: Vec<SlotSpec<'a>>,
    pub(super) providers: Vec<usize>,
}

impl<'a> ComponentBinding<'a> {
    fn provided_by(&mut self, provider: usize, names: &[&'a str]) -> Result<&'a str> {
        // Импорт сервиса и каталог компонента поднимают провайдера раньше
        // потребителя, а ребро выводится из связывания.
        self.providers.push(provider);
        names
            .get(provider)
            .copied()
            .context("boot plan resolves a provider outside the composition")
    }
}

pub(super) fn compile_slots<'a>(
    name: &'a str,
    declared: &ProgramManifest,
    selectors: &'a [Vec<&'a str>],
    declarations: &[&ProgramManifest],
    names: &[&'a str],
) -> Result<ComponentBinding<'a>> {
    let mut binding = ComponentBinding::default();
    for (slot, compatible) in declared.slots.iter().zip(selectors) {
        let device = DeviceSpec {
            compatible,
            // Экземпляр среди совместимых устройств всегда нулевой: назвать
            // второй композиции нечем.
            instance: 0,
            resource: slot.resource,
        };
        let spec = match slot.kind {
            SlotKind::Userland => SlotSpec::Userland,
            SlotKind::Irq => SlotSpec::Irq(device),
            SlotKind::Mmio => SlotSpec::Mmio(device),
            SlotKind::Service => {
                let provider = find_provider(name, compatible, declarations, names)?;
                let export = u8::try_from(provider.export)
                    .context("boot plan resolves an export outside the record of its provider")?;
                SlotSpec::Service {
                    provider: binding.provided_by(provider.instance, names)?,
                    export,
                }
            }
            SlotKind::Directory => {
                let provider = find_area(name, compatible, declarations, names)?;
                SlotSpec::Directory {
                    provider: binding.provided_by(provider, names)?,
                }
            }
            SlotKind::Supervisor => SlotSpec::Supervisor,
        };
        binding.slots.push(spec);
    }
    binding.providers.sort_unstable();
    binding.providers.dedup();
    Ok(binding)
}

fn find_provider(
    name: &str,
    declared: &[&str],
    declarations: &[&ProgramManifest],
    names: &[&str],
) -> Result<Provider> {
    let protocol = declared.first().copied().with_context(|| {
        format!("boot plan component '{name}' declares a service slot without a protocol")
    })?;
    let provider = match declared.get(1).copied() {
        Some(instance) => find_instance(name, protocol, instance, declarations, names)?,
        None => find_only_provider(name, protocol, declarations, names)?,
    };
    ensure!(
        names.get(provider.instance).copied() != Some(name),
        "boot plan component '{name}' imports the protocol it exports itself"
    );
    Ok(provider)
}

fn find_instance(
    name: &str,
    protocol: &str,
    instance: &str,
    declarations: &[&ProgramManifest],
    names: &[&str],
) -> Result<Provider> {
    let index = names
        .iter()
        .position(|known| *known == instance)
        .with_context(|| {
            format!(
                "boot plan component '{name}' imports protocol '{protocol}' from instance \
                 '{instance}', which the composition does not contain"
            )
        })?;
    let declared = declarations
        .get(index)
        .context("boot plan resolves a provider outside the composition")?;
    let export = exported_by(declared, protocol).with_context(|| {
        format!(
            "boot plan component '{name}' imports protocol '{protocol}' from instance \
             '{instance}', which exports no such protocol"
        )
    })?;
    Ok(Provider {
        instance: index,
        export,
    })
}

fn find_area(
    name: &str,
    declared: &[&str],
    declarations: &[&ProgramManifest],
    names: &[&str],
) -> Result<usize> {
    let instance = declared.first().copied().with_context(|| {
        format!("boot plan component '{name}' declares a directory slot without an instance")
    })?;
    let index = names
        .iter()
        .position(|known| *known == instance)
        .with_context(|| {
            format!(
                "boot plan component '{name}' takes its directory from instance '{instance}', \
                 which the composition does not contain"
            )
        })?;
    ensure!(
        names.get(index).copied() != Some(name),
        "boot plan component '{name}' takes its directory from itself"
    );
    let provider = declarations
        .get(index)
        .context("boot plan resolves a provider outside the composition")?;
    // Каталог приходит сессией, суженной до него, и сузить её может лишь тот, кто её
    // сам создаёт.
    ensure!(
        provider.establishes_sessions,
        "boot plan component '{name}' takes its directory from instance '{instance}', \
         which does not establish its own sessions"
    );
    // Слот-каталог протокола не называет, и выбрать экспорт провайдера ему нечем.
    ensure!(
        provider.exports.len() == 1,
        "boot plan component '{name}' takes its directory from instance '{instance}', \
         which exports {} protocols; the directory slot names none",
        provider.exports.len()
    );
    Ok(index)
}

fn find_only_provider(
    name: &str,
    protocol: &str,
    declarations: &[&ProgramManifest],
    names: &[&str],
) -> Result<Provider> {
    let providers: Vec<Provider> = declarations
        .iter()
        .enumerate()
        .filter_map(|(index, declared)| {
            exported_by(declared, protocol).map(|export| Provider {
                instance: index,
                export,
            })
        })
        .collect();
    let [provider] = providers[..] else {
        if providers.is_empty() {
            bail!(
                "boot plan component '{name}' imports protocol '{protocol}', \
                 which no component of the composition exports"
            );
        }
        let exporters: Vec<&str> = providers
            .iter()
            .filter_map(|provider| names.get(provider.instance).copied())
            .collect();
        bail!(
            "boot plan component '{name}' imports protocol '{protocol}', \
             which {} components export: {}",
            exporters.len(),
            exporters.join(", ")
        );
    };
    Ok(provider)
}

fn exported_by(declared: &ProgramManifest, protocol: &str) -> Option<usize> {
    declared
        .exports
        .iter()
        .position(|export| export == protocol)
}
