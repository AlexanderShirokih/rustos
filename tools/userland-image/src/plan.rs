use anyhow::{Context, Result, anyhow, bail, ensure};
use component_abi::{COMPONENT_SLOT_MAX, SlotKind};
use filesystem_abi::NAME_MAX;
use serde::Deserialize;
use supervisor_abi::PLAN_RECORD_MAX;
use syscall::{
    HANDLE_TABLE_CAPACITY, MAX_PROCESS_NAME_LEN, MAX_RESOURCE_DEPTH, Timeout, WAIT_MANY_MAX_COUNT,
};
use userland::entry_name;
use userland_image::{ComponentSpec, DeviceSpec, OnExhausted, SlotSpec, encode_plan};

use crate::{
    host::validate_image_path,
    manifest::ProgramManifest,
    units::{parse_duration, parse_memory_pages},
};

const BOOT_PLAN_MANIFEST_VERSION: u16 = 1;

const BOOT_COMPOSITION_DEPTH: u16 = 0;

const COMPONENT_RESOURCE_LEVELS: u16 = 3;

const SUPERVISOR_HANDLES: usize = 24;

const SUPERVISOR_WAITS: usize = 2;

const COMPONENT_HANDLES: usize = 13;

const DEFAULT_STARTUP_TIMEOUT: Timeout = Timeout::from_secs(2);

const RESTART_KEYWORD: &str = "restart";
const THEN_KEYWORD: &str = "then";
const OUTCOME_HALT: &str = "halt";
const OUTCOME_DETACH: &str = "detach";

#[derive(Clone, Copy, Debug)]
pub(crate) struct PlanProgram<'a> {
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SystemManifest {
    #[serde(default = "default_plan_version")]
    version: u16,
    components: Vec<ComponentEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ComponentEntry {
    executable: String,
    #[serde(default)]
    name: Option<String>,
    memory_limit: String,
    #[serde(default)]
    startup_timeout: Option<String>,
    on_failure: String,
    #[serde(default)]
    after: Vec<String>,
}

struct FailurePolicy {
    launches: u32,
    on_exhausted: OnExhausted,
}

struct Policy {
    memory_pages: u64,
    startup_timeout: Timeout,
    launches: u32,
    on_exhausted: OnExhausted,
}

#[derive(Clone, Copy)]
struct ImportedPort {
    slot_kind: &'static str,
    import_noun: &'static str,
    provider_verb: &'static str,
    providers_verb: &'static str,
    served_protocol: fn(&ProgramManifest) -> Option<&str>,
}

const SERVICE_PORT: ImportedPort = ImportedPort {
    slot_kind: "service",
    import_noun: "protocol",
    provider_verb: "exports",
    providers_verb: "export",
    served_protocol: |declared| declared.export.as_deref(),
};

const CONTROL_PORT: ImportedPort = ImportedPort {
    slot_kind: "control",
    import_noun: "control protocol",
    provider_verb: "serves",
    providers_verb: "serve",
    served_protocol: |declared| declared.control.as_deref(),
};

#[derive(Default)]
struct ComponentBinding<'a> {
    slots: Vec<SlotSpec<'a>>,
    providers: Vec<u16>,
}

fn default_plan_version() -> u16 {
    BOOT_PLAN_MANIFEST_VERSION
}

pub(crate) fn compile_stages(
    plan_source: &str,
    stage_one_source: Option<&str>,
    programs: &[PlanProgram<'_>],
) -> Result<CompiledStages> {
    let stage_zero = parse_system_manifest(plan_source)?;
    let stage_one = stage_one_source.map(parse_system_manifest).transpose()?;
    let boundary = stage_zero.components.len();
    // Связывание идёт над объединением ступеней: слот ступени 1 называет экспортёра ступени 0
    // именем экземпляра, и имя занято набором целиком.
    let entries: Vec<&ComponentEntry> = stage_zero
        .components
        .iter()
        .chain(stage_one.iter().flat_map(|stage| stage.components.iter()))
        .collect();

    let names = resolve_names(&entries)?;
    let declarations = resolve_declarations(&entries, programs)?;
    validate_composition(&entries, &names, &declarations)?;
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
    let bindings = declarations
        .iter()
        .zip(&selectors)
        .zip(&names)
        .map(|((declared, selectors), name)| {
            compile_slots(name, declared, selectors, &declarations, &names)
        })
        .collect::<Result<Vec<_>>>()?;
    let after = entries
        .iter()
        .zip(&names)
        .map(|(component, name)| resolve_after(component, name, &names))
        .collect::<Result<Vec<_>>>()?;
    validate_acyclic(&bindings, &after, &names)?;
    validate_stage_order(boundary, &bindings, &after, &names)?;

    let after_names: Vec<Vec<&str>> = after
        .iter()
        .map(|edges| edges.iter().map(|edge| names[usize::from(*edge)]).collect())
        .collect();
    let policies = entries
        .iter()
        .zip(&names)
        .map(|(component, name)| compile_policy(component, name))
        .collect::<Result<Vec<_>>>()?;
    let components: Vec<ComponentSpec<'_>> = entries
        .iter()
        .enumerate()
        .map(|(index, component)| ComponentSpec {
            executable: &component.executable,
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
    let plan = encode_plan(&components[..boundary])
        .map_err(|err| anyhow!("failed to serialize boot plan: {err:?}"))?;
    let records = components[boundary..]
        .iter()
        .zip(&names[boundary..])
        .map(|(component, name)| {
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
                name: String::from(*name),
                bytes,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(CompiledStages {
        plan,
        stage_one: records,
        instances: StageInstances {
            stage_zero: owned_names(&names[..boundary]),
            stage_one: owned_names(&names[boundary..]),
        },
    })
}

fn owned_names(names: &[&str]) -> Vec<String> {
    names.iter().copied().map(String::from).collect()
}

fn resolve_names<'a>(entries: &[&'a ComponentEntry]) -> Result<Vec<&'a str>> {
    let names: Vec<&str> = entries
        .iter()
        .map(|component| match &component.name {
            Some(name) => name.as_str(),
            None => entry_name(&component.executable),
        })
        .collect();
    for (index, name) in names.iter().enumerate() {
        validate_name(name)?;
        ensure!(
            !names[..index].contains(name),
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
    // Именем экземпляра зовётся его процесс: имя длиннее ёмкости `ProcessCreate`
    // не поднялось бы на устройстве.
    ensure!(
        name.len() <= MAX_PROCESS_NAME_LEN,
        "boot plan component name '{name}' is longer than \
         {MAX_PROCESS_NAME_LEN} bytes, which a process name holds"
    );
    Ok(())
}

fn validate_record_name(name: &str) -> Result<()> {
    // Компонент ступени 1 назван записью образа, и её имя - один компонент пути.
    ensure!(
        name.len() <= NAME_MAX,
        "stage 1 component name '{name}' is longer than {NAME_MAX} bytes, \
         which an image record name holds"
    );
    Ok(())
}

fn parse_system_manifest(source: &str) -> Result<SystemManifest> {
    let manifest: SystemManifest =
        toml::from_str(source).context("Failed to parse boot plan TOML")?;
    ensure!(
        manifest.version == BOOT_PLAN_MANIFEST_VERSION,
        "Unsupported boot plan version {}; expected {}",
        manifest.version,
        BOOT_PLAN_MANIFEST_VERSION
    );
    ensure!(
        !manifest.components.is_empty(),
        "boot plan must contain at least one component; found none"
    );
    Ok(manifest)
}

fn resolve_declarations<'a>(
    entries: &[&ComponentEntry],
    programs: &[PlanProgram<'a>],
) -> Result<Vec<&'a ProgramManifest>> {
    entries
        .iter()
        .map(|component| {
            let executable = &component.executable;
            validate_image_path(executable)
                .context("boot plan component names an invalid executable")?;
            let program = programs
                .iter()
                .find(|program| program.path == executable)
                .with_context(|| {
                    format!(
                        "boot plan names executable '{executable}' that the image does not contain"
                    )
                })?;
            Ok(program.manifest)
        })
        .collect()
}

fn compile_policy(component: &ComponentEntry, name: &str) -> Result<Policy> {
    let memory_pages = parse_memory_pages(&component.memory_limit)
        .with_context(|| format!("boot plan component '{name}' sets 'memory_limit'"))?;
    let startup_timeout = match &component.startup_timeout {
        Some(text) => parse_duration(text)
            .with_context(|| format!("boot plan component '{name}' sets 'startup_timeout'"))?,
        None => DEFAULT_STARTUP_TIMEOUT,
    };
    let policy = parse_failure_policy(&component.on_failure)
        .with_context(|| format!("boot plan component '{name}' sets 'on_failure'"))?;
    Ok(Policy {
        memory_pages,
        startup_timeout,
        launches: policy.launches,
        on_exhausted: policy.on_exhausted,
    })
}

fn parse_failure_policy(text: &str) -> Result<FailurePolicy> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let (restarts, outcome) = match tokens.as_slice() {
        [outcome] => (0, *outcome),
        [RESTART_KEYWORD, count, THEN_KEYWORD, outcome] => (parse_restarts(count)?, *outcome),
        _ => bail!(
            "'{text}' is not '{OUTCOME_HALT}', '{OUTCOME_DETACH}' \
             or '{RESTART_KEYWORD} N {THEN_KEYWORD} {OUTCOME_HALT}|{OUTCOME_DETACH}'"
        ),
    };
    let on_exhausted = match outcome {
        OUTCOME_HALT => OnExhausted::Halt,
        OUTCOME_DETACH => OnExhausted::Detach,
        _ => bail!("'{outcome}' is not '{OUTCOME_HALT}' or '{OUTCOME_DETACH}'"),
    };
    // Первый запуск перезапуском не считается: он есть у любого компонента.
    let launches = restarts
        .checked_add(1)
        .with_context(|| format!("restart count {restarts} overflows"))?;
    Ok(FailurePolicy {
        launches,
        on_exhausted,
    })
}

fn parse_restarts(text: &str) -> Result<u32> {
    let count: u32 = text
        .parse()
        .with_context(|| format!("'{text}' is not a restart count"))?;
    ensure!(
        count != 0,
        "'{RESTART_KEYWORD} 0' carries no restarts: name the outcome alone"
    );
    Ok(count)
}

fn validate_composition(
    entries: &[&ComponentEntry],
    names: &[&str],
    declarations: &[&ProgramManifest],
) -> Result<()> {
    for (name, declared) in names.iter().zip(declarations) {
        validate_component(name, declared)?;
    }
    validate_supervisor_limits(entries.len(), declarations)
}

fn validate_component(name: &str, declared: &ProgramManifest) -> Result<()> {
    ensure!(
        declared.slots.len() <= COMPONENT_SLOT_MAX,
        "boot plan component '{name}' declares {} slots; \
         the startup transaction carries {COMPONENT_SLOT_MAX}",
        declared.slots.len()
    );
    // Установление сессии наполняет импорт экспортируемого протокола: без
    // экспорта просить сессию у компонента некому.
    ensure!(
        !declared.establishes_sessions || declared.export.is_some(),
        "boot plan component '{name}' establishes its own sessions without exporting a protocol"
    );
    // Кадр порта разбирает один dispatch, и незнакомый ordinal он отвергает: два протокола
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

fn validate_resource_depth(name: &str, levels: u16) -> Result<()> {
    // Дерево метеринга компонента укладывается в потолок глубины split: резерв
    // компонента, ресурс incarnation и ресурс поднятого ею потомка.
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
    // Оценка сверху: у supervisor bootstrap-капа, регион образа с отображением,
    // лог-порт relay и его поток, порт транзакции расширения с приёмной капой, сигналом
    // её оборота и своим потоком, собственный ресурс, а у каждого компонента - резерв
    // бюджета, ресурс incarnation, процесс, обе стороны control-порта и его минтер, обе
    // стороны service-порта и его минтер, лог-сессия. Сверх постоянных компонент держит
    // временные капы своего подъёма: обе стороны startup-порта, а провижининг каталога -
    // сессию провайдера и выданное ею окно; они уходят до следующего компонента. Каждый
    // слот держит свою капу до конца стартовой транзакции.
    let handles = SUPERVISOR_HANDLES + count * COMPONENT_HANDLES + slots;
    ensure!(
        handles <= HANDLE_TABLE_CAPACITY as usize,
        "boot plan composition of {count} components needs up to {handles} rootkeeper handles; \
         the table holds {HANDLE_TABLE_CAPACITY}"
    );
    Ok(())
}

fn compile_slots<'a>(
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
            SlotKind::Service | SlotKind::Control => {
                let port = if slot.kind == SlotKind::Service {
                    SERVICE_PORT
                } else {
                    CONTROL_PORT
                };
                let provider = find_provider(name, port, compatible, declarations, names)?;
                binding.providers.push(provider);
                let provider = names[usize::from(provider)];
                if slot.kind == SlotKind::Service {
                    SlotSpec::Service { provider }
                } else {
                    SlotSpec::Control { provider }
                }
            }
            SlotKind::Directory => {
                let provider = find_area(name, compatible, declarations, names)?;
                binding.providers.push(provider);
                SlotSpec::Directory {
                    provider: names[usize::from(provider)],
                }
            }
            SlotKind::Supervisor => SlotSpec::Supervisor,
        };
        binding.slots.push(spec);
    }
    // Импорт сервиса и каталог компонента поднимают провайдера раньше
    // потребителя: ребро выводится из связывания.
    binding.providers.sort_unstable();
    binding.providers.dedup();
    Ok(binding)
}

fn find_provider(
    name: &str,
    port: ImportedPort,
    declared: &[&str],
    declarations: &[&ProgramManifest],
    names: &[&str],
) -> Result<u16> {
    let protocol = declared.first().copied().with_context(|| {
        let slot = port.slot_kind;
        format!("boot plan component '{name}' declares a {slot} slot without a protocol")
    })?;
    let index = match declared.get(1).copied() {
        Some(instance) => find_instance(name, port, protocol, instance, declarations, names)?,
        None => find_only_provider(name, port, protocol, declarations, names)?,
    };
    ensure!(
        names[usize::from(index)] != name,
        "boot plan component '{name}' imports the {} it {} itself",
        port.import_noun,
        port.provider_verb
    );
    Ok(index)
}

fn find_instance(
    name: &str,
    port: ImportedPort,
    protocol: &str,
    instance: &str,
    declarations: &[&ProgramManifest],
    names: &[&str],
) -> Result<u16> {
    let imported = port.import_noun;
    let index = names
        .iter()
        .position(|known| *known == instance)
        .with_context(|| {
            format!(
                "boot plan component '{name}' imports {imported} '{protocol}' from instance \
                 '{instance}', which the composition does not contain"
            )
        })?;
    ensure!(
        (port.served_protocol)(declarations[index]) == Some(protocol),
        "boot plan component '{name}' imports {imported} '{protocol}' from instance \
         '{instance}', which {} no such protocol",
        port.provider_verb
    );
    u16::try_from(index).context("component index fits the plan")
}

fn find_area(
    name: &str,
    declared: &[&str],
    declarations: &[&ProgramManifest],
    names: &[&str],
) -> Result<u16> {
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
        names[index] != name,
        "boot plan component '{name}' takes its directory from itself"
    );
    // Каталог приходит сессией, суженной до него, и сузить её умеет лишь тот, кто её
    // заводит сам.
    ensure!(
        declarations[index].establishes_sessions,
        "boot plan component '{name}' takes its directory from instance '{instance}', \
         which does not establish its own sessions"
    );
    u16::try_from(index).context("component index fits the plan")
}

fn find_only_provider(
    name: &str,
    port: ImportedPort,
    protocol: &str,
    declarations: &[&ProgramManifest],
    names: &[&str],
) -> Result<u16> {
    let imported = port.import_noun;
    let providers: Vec<usize> = declarations
        .iter()
        .enumerate()
        .filter(|(_, declared)| (port.served_protocol)(declared) == Some(protocol))
        .map(|(index, _)| index)
        .collect();
    let [index] = providers[..] else {
        if providers.is_empty() {
            bail!(
                "boot plan component '{name}' imports {imported} '{protocol}', \
                 which no component of the composition {}",
                port.provider_verb
            );
        }
        let providers: Vec<&str> = providers.iter().map(|index| names[*index]).collect();
        bail!(
            "boot plan component '{name}' imports {imported} '{protocol}', \
             which {} components {}: {}",
            providers.len(),
            port.providers_verb,
            providers.join(", ")
        );
    };
    u16::try_from(index).context("component index fits the plan")
}

fn resolve_after(component: &ComponentEntry, name: &str, names: &[&str]) -> Result<Vec<u16>> {
    component
        .after
        .iter()
        .map(|edge| {
            ensure!(
                edge != name,
                "boot plan component '{name}' names itself in 'after'"
            );
            let index = names
                .iter()
                .position(|known| known == edge)
                .with_context(|| {
                    format!(
                        "boot plan component '{name}' names '{edge}' in 'after', \
                         which the composition does not contain"
                    )
                })?;
            u16::try_from(index).context("component index fits the plan")
        })
        .collect()
}

fn validate_stage_order(
    boundary: usize,
    bindings: &[ComponentBinding<'_>],
    after: &[Vec<u16>],
    names: &[&str],
) -> Result<()> {
    // Ступень 0 поднимается целиком до того, как её владелец политики загрузки подаст набор
    // ступени 1: назвать компонент ступени 1 её зависимость не может.
    for index in 0..boundary {
        for edge in bindings[index].providers.iter().chain(&after[index]) {
            let edge = usize::from(*edge);
            ensure!(
                edge < boundary,
                "boot plan component '{}' depends on '{}', which stage 1 raises",
                names[index],
                names[edge]
            );
        }
    }
    Ok(())
}

fn validate_acyclic(
    bindings: &[ComponentBinding<'_>],
    after: &[Vec<u16>],
    names: &[&str],
) -> Result<()> {
    // Порядок запуска - топологический по объединению провайдеров слотов и
    // after: цикл делает его невыразимым.
    let edges = |index: usize| bindings[index].providers.iter().chain(&after[index]);
    let mut dependents: Vec<usize> = vec![0; names.len()];
    for index in 0..names.len() {
        for edge in edges(index) {
            dependents[usize::from(*edge)] += 1;
        }
    }
    let mut ready: Vec<usize> = (0..names.len())
        .filter(|index| dependents[*index] == 0)
        .collect();
    let mut ordered = 0;
    while let Some(index) = ready.pop() {
        ordered += 1;
        for edge in edges(index) {
            let edge = usize::from(*edge);
            dependents[edge] -= 1;
            if dependents[edge] == 0 {
                ready.push(edge);
            }
        }
    }
    if ordered != names.len() {
        let looped: Vec<&str> = (0..names.len())
            .filter(|index| dependents[*index] != 0)
            .map(|index| names[index])
            .collect();
        bail!(
            "boot plan dependencies form a cycle over components: {}",
            looped.join(", ")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use userland_image::{SlotSource, decode_plan};

    use super::*;
    use crate::manifest::ManifestSlot;

    const ROUTER: &str = "/boot/namespace-router";
    const DRIVER: &str = "/boot/driver-pl031";
    const IMAGE_FS: &str = "/boot/filesystem-imagefs";
    const RAM_FS: &str = "/boot/filesystem-ramfs";
    const ROOTKEEPER: &str = "/boot/rootkeeper";
    const CONSUMER: &str = "/boot/namespace-consumer-fixture";
    const AREA: &str = "/boot/area-consumer";

    const NAMESPACE_PROTOCOL: &str = "Namespace";
    const NAMESPACE_ADMIN_PROTOCOL: &str = "NamespaceAdmin";
    const FILE_SYSTEM_PROTOCOL: &str = "FileSystem";

    const ROUTER_NAME: &str = "namespace-router";
    const IMAGE_FS_NAME: &str = "filesystem-imagefs";
    const RAM_FS_NAME: &str = "filesystem-ramfs";
    const DRIVER_NAME: &str = "driver-pl031";
    const CONSUMER_NAME: &str = "namespace-consumer-fixture";
    const AREA_NAME: &str = "area-consumer";

    const RTC_COMPATIBLE: &[&str] = &["arm,pl031", "arm,primecell"];

    const DRIVER_FAMILY: usize = WAIT_MANY_MAX_COUNT;

    fn plan(components: &[String]) -> String {
        format!("version = 1\n{}", components.concat())
    }

    fn filesystem(fields: &str) -> String {
        format!(
            "\n[[components]]\n\
             executable = \"{ROUTER}\"\n\
             memory_limit = \"8M\"\n\
             startup_timeout = \"2s\"\n\
             on_failure = \"restart 2 then halt\"\n\
             {fields}"
        )
    }

    fn image_filesystem(fields: &str) -> String {
        format!(
            "\n[[components]]\n\
             executable = \"{IMAGE_FS}\"\n\
             memory_limit = \"8M\"\n\
             startup_timeout = \"2s\"\n\
             on_failure = \"halt\"\n\
             {fields}"
        )
    }

    fn rtc(fields: &str) -> String {
        format!(
            "\n[[components]]\n\
             executable = \"{DRIVER}\"\n\
             memory_limit = \"256K\"\n\
             on_failure = \"detach\"\n\
             {fields}"
        )
    }

    fn consumer(fields: &str) -> String {
        format!(
            "\n[[components]]\n\
             executable = \"{CONSUMER}\"\n\
             memory_limit = \"256K\"\n\
             on_failure = \"detach\"\n\
             {fields}"
        )
    }

    fn ram_filesystem(fields: &str) -> String {
        format!(
            "\n[[components]]\n\
             executable = \"{RAM_FS}\"\n\
             memory_limit = \"4M\"\n\
             on_failure = \"detach\"\n\
             {fields}"
        )
    }

    fn slot(kind: SlotKind, resource: u16, names: &[&str]) -> ManifestSlot {
        ManifestSlot {
            kind,
            resource,
            names: names.iter().map(|name| String::from(*name)).collect(),
        }
    }

    fn declaration(slots: Vec<ManifestSlot>, export: Option<&str>) -> ProgramManifest {
        ProgramManifest {
            slots,
            export: export.map(String::from),
            control: None,
            establishes_sessions: false,
        }
    }

    fn router_declaration() -> ProgramManifest {
        ProgramManifest {
            control: Some(String::from(NAMESPACE_ADMIN_PROTOCOL)),
            ..declaration(
                vec![
                    slot(SlotKind::Userland, 0, &[]),
                    slot(SlotKind::Supervisor, 0, &[]),
                ],
                Some(NAMESPACE_PROTOCOL),
            )
        }
    }

    fn device_declaration() -> ProgramManifest {
        declaration(
            vec![
                slot(SlotKind::Irq, 0, RTC_COMPATIBLE),
                slot(SlotKind::Mmio, 1, RTC_COMPATIBLE),
            ],
            None,
        )
    }

    fn consumer_declaration() -> ProgramManifest {
        declaration(
            vec![slot(SlotKind::Service, 0, &[NAMESPACE_PROTOCOL])],
            None,
        )
    }

    fn image_filesystem_declaration() -> ProgramManifest {
        ProgramManifest {
            establishes_sessions: true,
            ..declaration(
                vec![slot(SlotKind::Userland, 0, &[])],
                Some(FILE_SYSTEM_PROTOCOL),
            )
        }
    }

    fn area_declaration() -> ProgramManifest {
        declaration(vec![slot(SlotKind::Directory, 0, &[IMAGE_FS_NAME])], None)
    }

    struct Image {
        entries: Vec<(String, ProgramManifest)>,
    }

    impl Image {
        fn new() -> Self {
            let mut entries = vec![
                (String::from(ROOTKEEPER), ProgramManifest::default()),
                (String::from(ROUTER), router_declaration()),
                (String::from(DRIVER), device_declaration()),
                (String::from(CONSUMER), consumer_declaration()),
                (String::from(IMAGE_FS), image_filesystem_declaration()),
                (String::from(RAM_FS), ProgramManifest::default()),
            ];
            for index in 0..DRIVER_FAMILY {
                entries.push((driver_family_member(index), device_declaration()));
            }
            Self { entries }
        }

        fn with(mut self, path: &str, manifest: ProgramManifest) -> Self {
            self.entries.push((String::from(path), manifest));
            self
        }

        fn programs(&self) -> Vec<PlanProgram<'_>> {
            self.entries
                .iter()
                .map(|(path, manifest)| PlanProgram { path, manifest })
                .collect()
        }

        fn compile(&self, source: &str) -> Result<Vec<u8>> {
            Ok(compile_stages(source, None, &self.programs())?.plan)
        }

        fn compile_stages(&self, plan: &str, stage_one: &str) -> Result<CompiledStages> {
            compile_stages(plan, Some(stage_one), &self.programs())
        }
    }

    fn driver_family_member(index: usize) -> String {
        format!("/boot/driver-{index}")
    }

    fn compiled(source: &str) -> Vec<u8> {
        Image::new().compile(source).expect("compile must succeed")
    }

    fn rejected(source: &str) -> String {
        let error = Image::new()
            .compile(source)
            .expect_err("invalid composition must fail");
        // Причина читается вместе со своей цепочкой: значение и компонент,
        // который его назвал, лежат на разных её звеньях.
        format!("{error:#}")
    }

    #[test]
    fn stage_one_record_carries_one_component() {
        let stages = Image::new()
            .compile_stages(&plan(&[image_filesystem("")]), &plan(&[ram_filesystem("")]))
            .expect("compile must succeed");

        assert_eq!(
            decode_plan(&stages.plan)
                .expect("boot plan must decode")
                .len(),
            1
        );
        // `define` принимает запись плана ровно с одним компонентом, и владелец политики
        // загрузки подаёт её байты как есть.
        assert_eq!(stages.stage_one.len(), 1);
        assert_eq!(stages.stage_one[0].name, RAM_FS_NAME);
        let record = decode_plan(&stages.stage_one[0].bytes).expect("stage 1 record must decode");
        assert_eq!(record.len(), 1);
        let component = record.component(0).expect("record carries its component");
        assert_eq!(component.executable(), RAM_FS);
        assert_eq!(component.name(), RAM_FS_NAME);
    }

    #[test]
    fn stage_one_binds_a_slot_to_an_exporter_of_stage_zero() {
        let stages = Image::new()
            .compile_stages(&plan(&[filesystem("")]), &plan(&[consumer("")]))
            .expect("compile must succeed");

        // Имена экземпляров работают сквозь ступени: провайдер слота стоит в плане.
        let record = decode_plan(&stages.stage_one[0].bytes).expect("stage 1 record must decode");
        let component = record.component(0).expect("record carries its component");
        assert_eq!(
            component.slots().collect::<Vec<_>>(),
            [SlotSource::Service {
                provider: ROUTER_NAME
            }]
        );
    }

    #[test]
    fn stage_one_rejects_a_name_taken_by_stage_zero() {
        let error = Image::new()
            .compile_stages(
                &plan(&[image_filesystem(""), ram_filesystem("")]),
                &plan(&[ram_filesystem("")]),
            )
            .expect_err("a repeated instance name must fail");

        assert_eq!(
            format!("{error:#}"),
            format!("boot plan names component '{RAM_FS_NAME}' more than once")
        );
    }

    #[test]
    fn stage_zero_rejects_a_dependency_on_stage_one() {
        let error = Image::new()
            .compile_stages(
                &plan(&[image_filesystem(&format!("after = [\"{RAM_FS_NAME}\"]\n"))]),
                &plan(&[ram_filesystem("")]),
            )
            .expect_err("an edge into stage 1 must fail");

        assert_eq!(
            format!("{error:#}"),
            format!(
                "boot plan component '{IMAGE_FS_NAME}' depends on '{RAM_FS_NAME}', \
                 which stage 1 raises"
            )
        );
    }

    #[test]
    fn stage_one_rejects_a_name_longer_than_an_image_record() {
        let name = "n".repeat(NAME_MAX + 1);
        let error = Image::new()
            .compile_stages(
                &plan(&[image_filesystem("")]),
                &plan(&[ram_filesystem(&format!("name = \"{name}\"\n"))]),
            )
            .expect_err("a name past the record bound must fail");

        assert_eq!(
            format!("{error:#}"),
            format!(
                "stage 1 component name '{name}' is longer than {NAME_MAX} bytes, \
                 which an image record name holds"
            )
        );
    }

    #[test]
    fn stage_one_rejects_a_record_past_the_transaction_bound() {
        const BULKY: &str = "/boot/bulky-driver";
        let compatible = format!("arm,{}", "d".repeat(200));
        let slots = (0..COMPONENT_SLOT_MAX)
            .map(|_| slot(SlotKind::Irq, 0, &[&compatible, &compatible]))
            .collect();
        let error = Image::new()
            .with(BULKY, declaration(slots, None))
            .compile_stages(
                &plan(&[image_filesystem("")]),
                &plan(&[rtc("").replace(DRIVER, BULKY)]),
            )
            .expect_err("a record past the transaction bound must fail");

        let error = format!("{error:#}");
        assert!(
            error.contains(&format!(
                "the extension transaction carries {PLAN_RECORD_MAX}"
            )),
            "the record bound must be the failing check: {error}"
        );
    }

    #[test]
    fn stage_one_rejects_a_cycle_of_its_own() {
        let first = ram_filesystem(&format!("after = [\"{DRIVER_NAME}\"]\n"));
        let second = rtc(&format!("after = [\"{RAM_FS_NAME}\"]\n"));
        let error = Image::new()
            .compile_stages(&plan(&[image_filesystem("")]), &plan(&[first, second]))
            .expect_err("a cycle inside stage 1 must fail");

        // Порядок запуска топологический по объединению ступеней, и цикл внутри ступени 1
        // делает его невыразимым так же, как цикл в плане.
        assert_eq!(
            format!("{error:#}"),
            format!(
                "boot plan dependencies form a cycle over components: \
                 {RAM_FS_NAME}, {DRIVER_NAME}"
            )
        );
    }

    #[test]
    fn plan_takes_every_slot_from_the_program_declaration() {
        let bytes = compiled(&plan(&[image_filesystem(""), rtc("")]));

        let plan = decode_plan(&bytes).expect("compiled plan must decode");
        assert_eq!(plan.len(), 2);

        let filesystem = plan.component(0).expect("image filesystem");
        assert_eq!(filesystem.executable(), IMAGE_FS);
        assert_eq!(
            filesystem.slots().collect::<Vec<_>>(),
            [SlotSource::Userland]
        );

        let rtc = plan.component(1).expect("rtc");
        let slots: Vec<SlotSource<'_>> = rtc.slots().collect();
        let SlotSource::Irq(line) = slots[0] else {
            panic!("first slot is an interrupt line")
        };
        assert_eq!(line.compatible().collect::<Vec<_>>(), RTC_COMPATIBLE);
        assert_eq!(line.resource, 0);
        let SlotSource::Mmio(window) = slots[1] else {
            panic!("second slot is an MMIO window")
        };
        assert_eq!(window.compatible().collect::<Vec<_>>(), RTC_COMPATIBLE);
        assert_eq!(window.resource, 1);
    }

    #[test]
    fn plan_carries_the_declaration_of_own_sessions() {
        const FILE_SYSTEM: &str = "/boot/filesystem-establishing";
        const FILE_SYSTEM_NAME: &str = "filesystem-establishing";
        const FILE_SYSTEM_PROTOCOL: &str = "FileSystem";
        const READER: &str = "/boot/file-reader";

        let provider = ProgramManifest {
            establishes_sessions: true,
            ..declaration(vec![], Some(FILE_SYSTEM_PROTOCOL))
        };
        let reader = declaration(
            vec![slot(SlotKind::Service, 0, &[FILE_SYSTEM_PROTOCOL])],
            None,
        );
        let source = plan(&[
            filesystem("").replace(ROUTER, FILE_SYSTEM),
            consumer("").replace(CONSUMER, READER),
        ]);
        let bytes = Image::new()
            .with(FILE_SYSTEM, provider)
            .with(READER, reader)
            .compile(&source)
            .expect("compile must succeed");

        let plan = decode_plan(&bytes).expect("compiled plan must decode");
        let provider = plan.component(0).expect("file system");
        assert!(provider.establishes_sessions());
        let reader = plan.component(1).expect("reader");
        assert!(!reader.establishes_sessions());
        assert_eq!(
            reader.slots().collect::<Vec<_>>(),
            [SlotSource::Service {
                provider: FILE_SYSTEM_NAME
            }]
        );
    }

    #[test]
    fn plan_rejects_own_sessions_without_an_export() {
        const MUTE: &str = "/boot/mute-provider";
        let mute = ProgramManifest {
            establishes_sessions: true,
            ..declaration(vec![], None)
        };
        let image = Image::new().with(MUTE, mute);
        let source = plan(&[consumer("").replace(CONSUMER, MUTE)]);

        let error = image
            .compile(&source)
            .expect_err("sessions of an unexported port have no importer");
        assert_eq!(
            format!("{error:#}"),
            "boot plan component 'mute-provider' establishes its own sessions \
             without exporting a protocol"
        );
    }

    #[test]
    fn plan_binds_an_imported_protocol_to_its_exporter() {
        let bytes = compiled(&plan(&[filesystem(""), consumer("")]));

        let plan = decode_plan(&bytes).expect("compiled plan must decode");
        let consumer = plan.component(1).expect("consumer");
        assert_eq!(
            consumer.slots().collect::<Vec<_>>(),
            [SlotSource::Service {
                provider: ROUTER_NAME
            }]
        );
    }

    #[test]
    fn plan_binds_a_named_import_to_the_instance_it_names() {
        const TWIN: &str = "/boot/namespace-router-twin";
        const TWIN_NAME: &str = "namespace-router-twin";
        const PICKY: &str = "/boot/picky-consumer";

        let picky = declaration(
            vec![slot(SlotKind::Service, 0, &[NAMESPACE_PROTOCOL, TWIN_NAME])],
            None,
        );
        let image = Image::new()
            .with(TWIN, router_declaration())
            .with(PICKY, picky);
        let source = plan(&[
            filesystem(""),
            filesystem("").replace(ROUTER, TWIN),
            consumer("").replace(CONSUMER, PICKY),
        ]);

        let bytes = image
            .compile(&source)
            .expect("a named import picks its exporter");
        let plan = decode_plan(&bytes).expect("compiled plan must decode");
        assert_eq!(
            plan.component(2)
                .expect("consumer")
                .slots()
                .collect::<Vec<_>>(),
            [SlotSource::Service {
                provider: TWIN_NAME
            }]
        );
    }

    #[test]
    fn plan_rejects_a_named_import_of_an_instance_outside_the_composition() {
        const PICKY: &str = "/boot/picky-consumer";
        let picky = declaration(
            vec![slot(SlotKind::Service, 0, &[NAMESPACE_PROTOCOL, "ghost"])],
            None,
        );
        let image = Image::new().with(PICKY, picky);
        let source = plan(&[filesystem(""), consumer("").replace(CONSUMER, PICKY)]);

        let error = image
            .compile(&source)
            .expect_err("the named instance is unknown");
        assert_eq!(
            format!("{error:#}"),
            format!(
                "boot plan component 'picky-consumer' imports protocol '{NAMESPACE_PROTOCOL}' \
                 from instance 'ghost', which the composition does not contain"
            )
        );
    }

    #[test]
    fn plan_rejects_a_named_import_of_an_instance_that_exports_nothing() {
        const PICKY: &str = "/boot/picky-consumer";
        let picky = declaration(
            vec![slot(
                SlotKind::Service,
                0,
                &[NAMESPACE_PROTOCOL, DRIVER_NAME],
            )],
            None,
        );
        let image = Image::new().with(PICKY, picky);
        let source = plan(&[
            filesystem(""),
            rtc(""),
            consumer("").replace(CONSUMER, PICKY),
        ]);

        let error = image
            .compile(&source)
            .expect_err("the named instance exports another protocol");
        assert_eq!(
            format!("{error:#}"),
            format!(
                "boot plan component 'picky-consumer' imports protocol '{NAMESPACE_PROTOCOL}' \
                 from instance '{DRIVER_NAME}', which exports no such protocol"
            )
        );
    }

    #[test]
    fn plan_orders_an_importer_after_its_exporter() {
        // Потребитель стоит в записи первым, а зависит от провайдера: ребро
        // ведёт вперёд по плану, и порядок остаётся выразимым.
        let bytes = compiled(&plan(&[consumer(""), filesystem("")]));

        let plan = decode_plan(&bytes).expect("compiled plan must decode");
        assert_eq!(
            plan.component(0)
                .expect("consumer")
                .slots()
                .collect::<Vec<_>>(),
            [SlotSource::Service {
                provider: ROUTER_NAME
            }]
        );
    }

    #[test]
    fn plan_keeps_an_after_edge_without_an_import() {
        let bytes = compiled(&plan(&[
            filesystem(""),
            rtc(&format!("after = [\"{ROUTER_NAME}\"]\n")),
        ]));

        let plan = decode_plan(&bytes).expect("compiled plan must decode");
        let rtc = plan.component(1).expect("rtc");
        assert_eq!(rtc.after().collect::<Vec<_>>(), [ROUTER_NAME]);
        assert_eq!(rtc.slots().count(), 2);
    }

    #[test]
    fn plan_rejects_an_import_no_component_exports() {
        let source = plan(&[rtc(""), consumer("")]);
        assert_eq!(
            rejected(&source),
            format!(
                "boot plan component '{CONSUMER_NAME}' imports protocol '{NAMESPACE_PROTOCOL}', \
                 which no component of the composition exports"
            )
        );
    }

    #[test]
    fn plan_rejects_an_import_two_components_export() {
        const TWIN: &str = "/boot/namespace-router-twin";
        let image = Image::new().with(TWIN, router_declaration());
        let twin = filesystem("").replace(ROUTER, TWIN);
        let source = plan(&[filesystem(""), twin, consumer("")]);

        let error = image.compile(&source).expect_err("the choice is ambiguous");
        assert_eq!(
            format!("{error:#}"),
            format!(
                "boot plan component '{CONSUMER_NAME}' imports protocol '{NAMESPACE_PROTOCOL}', \
                 which 2 components export: {ROUTER_NAME}, namespace-router-twin"
            )
        );
    }

    #[test]
    fn plan_rejects_a_component_importing_what_it_exports() {
        const SELF_BOUND: &str = "/boot/self-bound";
        let image = Image::new().with(
            SELF_BOUND,
            declaration(
                vec![slot(SlotKind::Service, 0, &[NAMESPACE_PROTOCOL])],
                Some(NAMESPACE_PROTOCOL),
            ),
        );
        let source = plan(&[consumer("").replace(CONSUMER, SELF_BOUND)]);

        let error = image
            .compile(&source)
            .expect_err("a component does not take its own service");
        assert_eq!(
            format!("{error:#}"),
            "boot plan component 'self-bound' imports the protocol it exports itself"
        );
    }

    #[test]
    fn plan_binds_a_control_import_to_the_component_serving_it() {
        const ADMIN: &str = "/boot/mount-admin";
        let admin = declaration(
            vec![slot(SlotKind::Control, 0, &[NAMESPACE_ADMIN_PROTOCOL])],
            None,
        );
        let image = Image::new().with(ADMIN, admin);
        let source = plan(&[filesystem(""), consumer("").replace(CONSUMER, ADMIN)]);

        let bytes = image.compile(&source).expect("compile must succeed");
        let plan = decode_plan(&bytes).expect("compiled plan must decode");
        // Ребро зависимости выведено из связывания: провайдер поднимается раньше того, кто
        // зовёт его админ-операции.
        assert_eq!(
            plan.component(1)
                .expect("admin consumer")
                .slots()
                .collect::<Vec<_>>(),
            [SlotSource::Control {
                provider: ROUTER_NAME
            }]
        );
    }

    #[test]
    fn plan_binds_a_named_control_import_to_the_instance_it_names() {
        const TWIN: &str = "/boot/namespace-router-twin";
        const TWIN_NAME: &str = "namespace-router-twin";
        const ADMIN: &str = "/boot/mount-admin";

        let admin = declaration(
            vec![slot(
                SlotKind::Control,
                0,
                &[NAMESPACE_ADMIN_PROTOCOL, TWIN_NAME],
            )],
            None,
        );
        let image = Image::new()
            .with(TWIN, router_declaration())
            .with(ADMIN, admin);
        let source = plan(&[
            filesystem(""),
            filesystem("").replace(ROUTER, TWIN),
            consumer("").replace(CONSUMER, ADMIN),
        ]);

        let bytes = image
            .compile(&source)
            .expect("a named control import picks its provider");
        let plan = decode_plan(&bytes).expect("compiled plan must decode");
        assert_eq!(
            plan.component(2)
                .expect("admin consumer")
                .slots()
                .collect::<Vec<_>>(),
            [SlotSource::Control {
                provider: TWIN_NAME
            }]
        );
    }

    #[test]
    fn plan_rejects_a_control_import_no_component_serves() {
        const ADMIN: &str = "/boot/mount-admin";
        let admin = declaration(
            vec![slot(SlotKind::Control, 0, &[NAMESPACE_ADMIN_PROTOCOL])],
            None,
        );
        let image = Image::new().with(ADMIN, admin);
        let source = plan(&[rtc(""), consumer("").replace(CONSUMER, ADMIN)]);

        let error = image
            .compile(&source)
            .expect_err("the composition serves no such control protocol");
        assert_eq!(
            format!("{error:#}"),
            format!(
                "boot plan component 'mount-admin' imports control protocol \
                 '{NAMESPACE_ADMIN_PROTOCOL}', \
                 which no component of the composition serves"
            )
        );
    }

    #[test]
    fn plan_rejects_a_named_control_import_of_an_instance_that_serves_nothing() {
        const ADMIN: &str = "/boot/mount-admin";
        let admin = declaration(
            vec![slot(
                SlotKind::Control,
                0,
                &[NAMESPACE_ADMIN_PROTOCOL, DRIVER_NAME],
            )],
            None,
        );
        let image = Image::new().with(ADMIN, admin);
        let source = plan(&[
            filesystem(""),
            rtc(""),
            consumer("").replace(CONSUMER, ADMIN),
        ]);

        let error = image
            .compile(&source)
            .expect_err("the named instance serves another control protocol");
        assert_eq!(
            format!("{error:#}"),
            format!(
                "boot plan component 'mount-admin' imports control protocol \
                 '{NAMESPACE_ADMIN_PROTOCOL}' from instance '{DRIVER_NAME}', \
                 which serves no such protocol"
            )
        );
    }

    #[test]
    fn plan_rejects_a_control_import_of_an_instance_outside_the_composition() {
        const ADMIN: &str = "/boot/mount-admin";
        let admin = declaration(
            vec![slot(
                SlotKind::Control,
                0,
                &[NAMESPACE_ADMIN_PROTOCOL, "ghost"],
            )],
            None,
        );
        let image = Image::new().with(ADMIN, admin);
        let source = plan(&[filesystem(""), consumer("").replace(CONSUMER, ADMIN)]);

        let error = image
            .compile(&source)
            .expect_err("the named instance is unknown");
        assert_eq!(
            format!("{error:#}"),
            format!(
                "boot plan component 'mount-admin' imports control protocol \
                 '{NAMESPACE_ADMIN_PROTOCOL}' from instance 'ghost', \
                 which the composition does not contain"
            )
        );
    }

    #[test]
    fn plan_rejects_a_component_importing_the_control_protocol_it_serves() {
        const SELF_BOUND: &str = "/boot/self-administered";
        let manifest = ProgramManifest {
            control: Some(String::from(NAMESPACE_ADMIN_PROTOCOL)),
            ..declaration(
                vec![slot(SlotKind::Control, 0, &[NAMESPACE_ADMIN_PROTOCOL])],
                None,
            )
        };
        let image = Image::new().with(SELF_BOUND, manifest);
        let source = plan(&[consumer("").replace(CONSUMER, SELF_BOUND)]);

        let error = image
            .compile(&source)
            .expect_err("a component does not take its own control port");
        assert_eq!(
            format!("{error:#}"),
            "boot plan component 'self-administered' imports the control protocol it serves itself"
        );
    }

    #[test]
    fn plan_rejects_a_control_protocol_beside_own_session_establishment() {
        const CROWDED: &str = "/boot/crowded-control";
        let crowded = ProgramManifest {
            control: Some(String::from(NAMESPACE_ADMIN_PROTOCOL)),
            establishes_sessions: true,
            ..declaration(vec![], Some("FileSystem"))
        };
        let image = Image::new().with(CROWDED, crowded);
        let source = plan(&[consumer("").replace(CONSUMER, CROWDED)]);

        // Кадры двух протоколов на одном порту разбирает один dispatch, и незнакомый
        // ordinal он отвергает.
        let error = image
            .compile(&source)
            .expect_err("one port carries one protocol");
        assert_eq!(
            format!("{error:#}"),
            format!(
                "boot plan component 'crowded-control' serves control protocol \
                 '{NAMESPACE_ADMIN_PROTOCOL}' and establishes its own sessions; \
                 the control port carries one protocol"
            )
        );
    }

    #[test]
    fn plan_binds_a_directory_slot_to_the_named_instance() {
        let image = Image::new().with(AREA, area_declaration());
        let source = plan(&[image_filesystem(""), rtc("").replace(DRIVER, AREA)]);

        let bytes = image.compile(&source).expect("compile must succeed");
        let plan = decode_plan(&bytes).expect("compiled plan must decode");
        let area = plan.component(1).expect("area");
        // Ребро зависимости выведено из связывания: файловая система поднимается
        // раньше того, кому выдаёт каталог.
        assert_eq!(
            area.slots().collect::<Vec<_>>(),
            [SlotSource::Directory {
                provider: IMAGE_FS_NAME
            }]
        );
    }

    #[test]
    fn plan_rejects_a_directory_slot_naming_its_own_component() {
        let image = Image::new().with(
            AREA,
            declaration(vec![slot(SlotKind::Directory, 0, &[AREA_NAME])], None),
        );
        let source = plan(&[image_filesystem(""), rtc("").replace(DRIVER, AREA)]);

        let error = image
            .compile(&source)
            .expect_err("a component takes no directory of its own");
        assert_eq!(
            format!("{error:#}"),
            format!("boot plan component '{AREA_NAME}' takes its directory from itself")
        );
    }

    #[test]
    fn plan_rejects_a_directory_slot_naming_an_instance_outside_the_composition() {
        let image = Image::new().with(AREA, area_declaration());
        let source = plan(&[rtc(""), rtc("").replace(DRIVER, AREA)]);

        let error = image
            .compile(&source)
            .expect_err("the composition carries no such instance");
        assert_eq!(
            format!("{error:#}"),
            format!(
                "boot plan component '{AREA_NAME}' takes its directory from instance \
                 '{IMAGE_FS_NAME}', which the composition does not contain"
            )
        );
    }

    #[test]
    fn plan_rejects_a_directory_slot_from_an_instance_without_own_sessions() {
        let image = Image::new().with(
            AREA,
            declaration(vec![slot(SlotKind::Directory, 0, &[ROUTER_NAME])], None),
        );
        let source = plan(&[filesystem(""), rtc("").replace(DRIVER, AREA)]);

        // Область назначает supervisor, а сужает её сам провайдер: сминченный значок
        // области не несёт.
        let error = image
            .compile(&source)
            .expect_err("a minted badge carries no scope");
        assert_eq!(
            format!("{error:#}"),
            format!(
                "boot plan component '{AREA_NAME}' takes its directory from instance \
                 '{ROUTER_NAME}', which does not establish its own sessions"
            )
        );
    }

    #[test]
    fn plan_rejects_a_directory_slot_of_a_component_the_provider_waits_for() {
        let image = Image::new().with(AREA, area_declaration());
        let source = plan(&[
            image_filesystem(&format!("after = [\"{AREA_NAME}\"]\n")),
            rtc("").replace(DRIVER, AREA),
        ]);

        // Компонент, которого файловая система ждёт сама, каталога от неё не
        // получает: порядок запуска такой композиции невыразим.
        let error = image
            .compile(&source)
            .expect_err("the directory edge closes the order");
        assert_eq!(
            format!("{error:#}"),
            format!(
                "boot plan dependencies form a cycle over components: \
                 {IMAGE_FS_NAME}, {AREA_NAME}"
            )
        );
    }

    #[test]
    fn plan_rejects_an_unsupported_version() {
        let source = plan(&[filesystem("")]).replace("version = 1", "version = 3");
        assert_eq!(
            rejected(&source),
            "Unsupported boot plan version 3; expected 1"
        );
    }

    #[test]
    fn a_component_without_a_name_is_called_by_its_directory() {
        let bytes = compiled(&plan(&[filesystem("")]));

        let plan = decode_plan(&bytes).expect("compiled plan must decode");
        let router = plan.component(0).expect("router");
        assert_eq!(router.executable(), ROUTER);
        assert_eq!(router.name(), ROUTER_NAME);
    }

    #[test]
    fn a_named_component_keeps_its_name_apart_from_the_program() {
        let bytes = compiled(&plan(&[filesystem("name = \"names\"\n")]));

        let plan = decode_plan(&bytes).expect("compiled plan must decode");
        let router = plan.component(0).expect("router");
        assert_eq!(router.executable(), ROUTER);
        assert_eq!(router.name(), "names");
    }

    #[test]
    fn one_program_carries_two_named_instances() {
        let source = plan(&[
            filesystem("name = \"names-boot\"\n"),
            filesystem("name = \"names-spare\"\n"),
        ]);

        let bytes = compiled(&source);

        let plan = decode_plan(&bytes).expect("compiled plan must decode");
        assert_eq!(plan.len(), 2);
        let first = plan.component(0).expect("first instance");
        let second = plan.component(1).expect("second instance");
        assert_eq!(first.executable(), second.executable());
        assert_eq!(first.name(), "names-boot");
        assert_eq!(second.name(), "names-spare");
        // Каталог у экземпляров общий, а значит и файлы: запись каталога
        // называет программа, а не композиция.
        assert_eq!(
            first.slots().collect::<Vec<_>>(),
            second.slots().collect::<Vec<_>>()
        );
    }

    #[test]
    fn an_after_edge_names_another_instance_of_the_same_program() {
        let source = plan(&[
            filesystem("name = \"names-boot\"\n"),
            filesystem("name = \"names-spare\"\nafter = [\"names-boot\"]\n"),
        ]);

        let bytes = compiled(&source);

        let plan = decode_plan(&bytes).expect("compiled plan must decode");
        assert_eq!(
            plan.component(1)
                .expect("second instance")
                .after()
                .collect::<Vec<_>>(),
            ["names-boot"]
        );
    }

    #[test]
    fn plan_rejects_a_name_outside_the_grammar() {
        let long = format!("\"{}\"", "n".repeat(MAX_PROCESS_NAME_LEN + 1));
        for name in ["\"\"", "\"boot/names\"", "\"имя\"", &long] {
            let source = plan(&[filesystem(&format!("name = {name}\n"))]);
            assert!(
                rejected(&source).starts_with("boot plan component name"),
                "'{name}' must fail on the component name"
            );
        }
    }

    #[test]
    fn plan_rejects_a_repeated_name() {
        let source = plan(&[filesystem(""), rtc("").replace(DRIVER, ROUTER)]);
        assert_eq!(
            rejected(&source),
            format!("boot plan names component '{ROUTER_NAME}' more than once")
        );
    }

    #[test]
    fn plan_rejects_an_empty_composition() {
        assert_eq!(
            rejected("version = 1\ncomponents = []\n"),
            "boot plan must contain at least one component; found none"
        );
    }

    #[test]
    fn plan_rejects_a_memory_limit_outside_the_grammar() {
        for limit in ["\"0\"", "\"8MB\"", "\"5000\"", "\"\""] {
            let source = plan(&[filesystem("")])
                .replace("memory_limit = \"8M\"", &format!("memory_limit = {limit}"));
            assert!(
                rejected(&source).starts_with(&format!(
                    "boot plan component '{ROUTER_NAME}' sets 'memory_limit'"
                )),
                "'{limit}' must fail on the memory limit"
            );
        }
    }

    #[test]
    fn plan_rejects_a_startup_timeout_outside_the_grammar() {
        let source =
            plan(&[filesystem("")]).replace("startup_timeout = \"2s\"", "startup_timeout = \"0s\"");
        assert!(rejected(&source).starts_with(&format!(
            "boot plan component '{ROUTER_NAME}' sets 'startup_timeout'"
        )));
    }

    #[test]
    fn plan_rejects_a_failure_policy_outside_the_grammar() {
        for policy in [
            "\"\"",
            "\"reboot\"",
            "\"restart 2\"",
            "\"restart two then halt\"",
            "\"restart 0 then halt\"",
            "\"restart 2 then reboot\"",
        ] {
            let source = plan(&[filesystem("")]).replace(
                "on_failure = \"restart 2 then halt\"",
                &format!("on_failure = {policy}"),
            );
            assert!(
                rejected(&source).starts_with(&format!(
                    "boot plan component '{ROUTER_NAME}' sets 'on_failure'"
                )),
                "'{policy}' must fail on the failure policy"
            );
        }
    }

    #[test]
    fn plan_rejects_an_executable_outside_the_image() {
        let source = plan(&[filesystem("")]).replace(ROUTER, "/boot/ghost-router");
        assert_eq!(
            rejected(&source),
            "boot plan names executable '/boot/ghost-router' that the image does not contain"
        );
    }

    #[test]
    fn a_program_without_a_manifest_declares_nothing() {
        let source = plan(&[rtc("").replace(DRIVER, ROOTKEEPER)]);

        let bytes = compiled(&source);
        let plan = decode_plan(&bytes).expect("compiled plan must decode");
        let component = plan.component(0).expect("component");
        assert_eq!(component.executable(), ROOTKEEPER);
        assert_eq!(component.slots().count(), 0);
    }

    #[test]
    fn plan_rejects_a_cycle_over_after() {
        let source = plan(&[
            filesystem(&format!("after = [\"{DRIVER_NAME}\"]\n")),
            rtc(&format!("after = [\"{ROUTER_NAME}\"]\n")),
        ]);
        assert_eq!(
            rejected(&source),
            format!(
                "boot plan dependencies form a cycle over components: {ROUTER_NAME}, {DRIVER_NAME}"
            )
        );
    }

    #[test]
    fn plan_rejects_a_self_dependency() {
        let source = plan(&[filesystem(&format!("after = [\"{ROUTER_NAME}\"]\n"))]);
        assert_eq!(
            rejected(&source),
            format!("boot plan component '{ROUTER_NAME}' names itself in 'after'")
        );
    }

    #[test]
    fn plan_rejects_an_unknown_edge() {
        let source = plan(&[filesystem("after = [\"logger\"]\n")]);
        assert_eq!(
            rejected(&source),
            format!(
                "boot plan component '{ROUTER_NAME}' names 'logger' in 'after', \
                 which the composition does not contain"
            )
        );
    }

    #[test]
    fn plan_rejects_an_unknown_key() {
        let source = plan(&[filesystem("slots = [{ source = \"image\" }]\n")]);
        assert!(rejected(&source).starts_with("Failed to parse boot plan TOML"));
    }

    #[test]
    fn plan_rejects_more_slots_than_the_startup_transaction_carries() {
        const CROWDED: &str = "/boot/crowded";
        let image = Image::new().with(
            CROWDED,
            declaration(
                (0..=COMPONENT_SLOT_MAX)
                    .map(|_| slot(SlotKind::Userland, 0, &[]))
                    .collect(),
                None,
            ),
        );
        let source = plan(&[rtc("").replace(DRIVER, CROWDED)]);

        let error = image
            .compile(&source)
            .expect_err("slots past the transaction must fail");
        assert_eq!(
            format!("{error:#}"),
            format!(
                "boot plan component 'crowded' declares {} slots; \
                 the startup transaction carries {COMPONENT_SLOT_MAX}",
                COMPONENT_SLOT_MAX + 1
            )
        );
    }

    #[test]
    fn plan_allows_two_userland_slot_consumers() {
        const TWIN: &str = "/boot/filesystem-imagefs-twin";
        let userland = declaration(vec![slot(SlotKind::Userland, 0, &[])], None);
        let image = Image::new().with(TWIN, userland);
        let twin = image_filesystem("").replace(IMAGE_FS, TWIN);
        let source = plan(&[image_filesystem(""), twin]);
        image
            .compile(&source)
            .expect("userland image may have several readers");
    }

    #[test]
    fn components_fit_the_resource_depth_ceiling() {
        validate_resource_depth(ROUTER_NAME, COMPONENT_RESOURCE_LEVELS)
            .expect("a component fits the ceiling");

        let too_deep = validate_resource_depth(ROUTER_NAME, MAX_RESOURCE_DEPTH + 1)
            .expect_err("a component past the ceiling must fail")
            .to_string();
        assert_eq!(
            too_deep,
            format!(
                "boot plan component '{ROUTER_NAME}' needs {} resource levels under depth 0; \
                 the ceiling is {MAX_RESOURCE_DEPTH}",
                MAX_RESOURCE_DEPTH + 1
            )
        );
    }

    #[test]
    fn plan_rejects_a_composition_past_the_supervisor_wait_ceiling() {
        let mut components = vec![filesystem("")];
        for index in 1..WAIT_MANY_MAX_COUNT {
            components.push(rtc("").replace(DRIVER, &driver_family_member(index)));
        }
        let source = plan(&components);
        assert_eq!(
            rejected(&source),
            format!(
                "boot plan composition of {WAIT_MANY_MAX_COUNT} components needs \
                 {} supervisor wait objects; the ceiling is {WAIT_MANY_MAX_COUNT}",
                WAIT_MANY_MAX_COUNT + SUPERVISOR_WAITS
            )
        );
    }

    #[test]
    fn plan_rejects_a_composition_past_the_handle_estimate() {
        let components = (HANDLE_TABLE_CAPACITY as usize - SUPERVISOR_HANDLES) / COMPONENT_HANDLES;
        let mut entries = vec![filesystem("")];
        for index in 1..=components {
            entries.push(rtc("").replace(DRIVER, &driver_family_member(index)));
        }
        let source = plan(&entries);
        assert!(
            rejected(&source).contains("rootkeeper handles; the table holds"),
            "handle estimate must be the failing check"
        );
    }
}
