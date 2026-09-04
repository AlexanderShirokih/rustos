use anyhow::{Context, Result, bail, ensure};
use component_abi::SlotKind;
use userland_image::{DeviceSpec, SlotSpec};

use crate::manifest::ProgramManifest;

#[derive(Clone, Copy)]
enum Port {
    Service,
    Control,
}

#[derive(Default)]
pub(super) struct ComponentBinding<'a> {
    pub(super) slots: Vec<SlotSpec<'a>>,
    pub(super) providers: Vec<usize>,
}

impl Port {
    fn slot_kind(self) -> &'static str {
        match self {
            Self::Service => "service",
            Self::Control => "control",
        }
    }

    fn import_noun(self) -> &'static str {
        match self {
            Self::Service => "protocol",
            Self::Control => "control protocol",
        }
    }

    fn provider_verb(self) -> &'static str {
        match self {
            Self::Service => "exports",
            Self::Control => "serves",
        }
    }

    fn providers_verb(self) -> &'static str {
        match self {
            Self::Service => "export",
            Self::Control => "serve",
        }
    }

    fn served_protocol(self, declared: &ProgramManifest) -> Option<&str> {
        match self {
            Self::Service => declared.export.as_deref(),
            Self::Control => declared.control.as_deref(),
        }
    }
}

impl<'a> ComponentBinding<'a> {
    fn provided_by(&mut self, provider: usize, names: &[&'a str]) -> Result<&'a str> {
        // Импорт сервиса и каталог компонента поднимают провайдера раньше
        // потребителя: ребро выводится из связывания.
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
                let provider = find_provider(name, Port::Service, compatible, declarations, names)?;
                SlotSpec::Service {
                    provider: binding.provided_by(provider, names)?,
                }
            }
            SlotKind::Control => {
                let provider = find_provider(name, Port::Control, compatible, declarations, names)?;
                SlotSpec::Control {
                    provider: binding.provided_by(provider, names)?,
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
    port: Port,
    declared: &[&str],
    declarations: &[&ProgramManifest],
    names: &[&str],
) -> Result<usize> {
    let protocol = declared.first().copied().with_context(|| {
        let slot = port.slot_kind();
        format!("boot plan component '{name}' declares a {slot} slot without a protocol")
    })?;
    let index = match declared.get(1).copied() {
        Some(instance) => find_instance(name, port, protocol, instance, declarations, names)?,
        None => find_only_provider(name, port, protocol, declarations, names)?,
    };
    ensure!(
        names.get(index).copied() != Some(name),
        "boot plan component '{name}' imports the {} it {} itself",
        port.import_noun(),
        port.provider_verb()
    );
    Ok(index)
}

fn find_instance(
    name: &str,
    port: Port,
    protocol: &str,
    instance: &str,
    declarations: &[&ProgramManifest],
    names: &[&str],
) -> Result<usize> {
    let imported = port.import_noun();
    let index = names
        .iter()
        .position(|known| *known == instance)
        .with_context(|| {
            format!(
                "boot plan component '{name}' imports {imported} '{protocol}' from instance \
                 '{instance}', which the composition does not contain"
            )
        })?;
    let declared = declarations
        .get(index)
        .context("boot plan resolves a provider outside the composition")?;
    ensure!(
        port.served_protocol(declared) == Some(protocol),
        "boot plan component '{name}' imports {imported} '{protocol}' from instance \
         '{instance}', which {} no such protocol",
        port.provider_verb()
    );
    Ok(index)
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
    // Каталог приходит сессией, суженной до него, и сузить её умеет лишь тот, кто её
    // заводит сам.
    let provider = declarations
        .get(index)
        .context("boot plan resolves a provider outside the composition")?;
    ensure!(
        provider.establishes_sessions,
        "boot plan component '{name}' takes its directory from instance '{instance}', \
         which does not establish its own sessions"
    );
    Ok(index)
}

fn find_only_provider(
    name: &str,
    port: Port,
    protocol: &str,
    declarations: &[&ProgramManifest],
    names: &[&str],
) -> Result<usize> {
    let imported = port.import_noun();
    let providers: Vec<usize> = declarations
        .iter()
        .enumerate()
        .filter(|(_, declared)| port.served_protocol(declared) == Some(protocol))
        .map(|(index, _)| index)
        .collect();
    let [index] = providers[..] else {
        if providers.is_empty() {
            bail!(
                "boot plan component '{name}' imports {imported} '{protocol}', \
                 which no component of the composition {}",
                port.provider_verb()
            );
        }
        let providers: Vec<&str> = providers
            .iter()
            .filter_map(|index| names.get(*index).copied())
            .collect();
        bail!(
            "boot plan component '{name}' imports {imported} '{protocol}', \
             which {} components {}: {}",
            providers.len(),
            port.providers_verb(),
            providers.join(", ")
        );
    };
    Ok(index)
}
