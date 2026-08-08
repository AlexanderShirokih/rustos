use anyhow::{Context, Result, anyhow, bail, ensure};
use component_abi::{COMPONENT_SLOT_MAX, SlotKind};
use serde::Deserialize;
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

const SUPERVISOR_HANDLES: usize = 16;

const COMPONENT_HANDLES: usize = 12;

const DEFAULT_STARTUP_TIMEOUT: Timeout = Timeout::from_secs(2);

const RESTART_KEYWORD: &str = "restart";
const THEN_KEYWORD: &str = "then";
const OUTCOME_HALT: &str = "halt";
const OUTCOME_DETACH: &str = "detach";

#[derive(Clone, Copy, Debug)]
pub(crate) struct PlanProgram<'a> {
    pub path: &'a str,
    pub manifest: Option<&'a ProgramManifest>,
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

fn default_plan_version() -> u16 {
    BOOT_PLAN_MANIFEST_VERSION
}

pub(crate) fn compile_boot_plan(
    source: &str,
    programs: &[PlanProgram<'_>],
    records: &[&str],
) -> Result<Vec<u8>> {
    let manifest = parse_system_manifest(source)?;
    let names = resolve_names(&manifest)?;
    let declarations = resolve_declarations(&manifest, programs)?;
    validate_composition(&manifest, &names, &declarations)?;

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
    // Файл компонент называет именем в своём каталоге: путь наружу оттуда не
    // выразим, и чужую запись имя не назовёт.
    let files: Vec<Vec<Vec<String>>> = manifest
        .components
        .iter()
        .zip(&declarations)
        .map(|(component, declared)| {
            declared
                .slots
                .iter()
                .map(|slot| {
                    slot.names
                        .iter()
                        .map(|name| format!("{}/{name}", component.executable))
                        .collect()
                })
                .collect()
        })
        .collect();
    let slots = declarations
        .iter()
        .zip(selectors.iter().zip(&files))
        .zip(&names)
        .map(|((declared, (selectors, files)), name)| {
            compile_slots(
                name,
                declared,
                selectors,
                files,
                &declarations,
                &names,
                records,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let requires: Vec<Vec<u16>> = slots.iter().map(|slots| service_edges(slots)).collect();
    let after = manifest
        .components
        .iter()
        .zip(&names)
        .map(|(component, name)| resolve_after(component, name, &names))
        .collect::<Result<Vec<_>>>()?;
    validate_acyclic(&requires, &after, &names)?;

    let policies = manifest
        .components
        .iter()
        .zip(&names)
        .map(|(component, name)| compile_policy(component, name))
        .collect::<Result<Vec<_>>>()?;
    let components: Vec<ComponentSpec<'_>> = manifest
        .components
        .iter()
        .enumerate()
        .map(|(index, component)| ComponentSpec {
            executable: &component.executable,
            name: names[index],
            slots: &slots[index],
            requires: &requires[index],
            after: &after[index],
            memory_pages: policies[index].memory_pages,
            startup_timeout: policies[index].startup_timeout,
            launches: policies[index].launches,
            on_exhausted: policies[index].on_exhausted,
        })
        .collect();
    encode_plan(&components).map_err(|err| anyhow!("failed to serialize boot plan: {err:?}"))
}

fn resolve_names(manifest: &SystemManifest) -> Result<Vec<&str>> {
    let names: Vec<&str> = manifest
        .components
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

fn validate_name(name: &str) -> Result<()> {
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
    manifest: &SystemManifest,
    programs: &[PlanProgram<'a>],
) -> Result<Vec<&'a ProgramManifest>> {
    manifest
        .components
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
            program.manifest.with_context(|| {
                format!("boot plan component '{executable}' has no component manifest section")
            })
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
    manifest: &SystemManifest,
    names: &[&str],
    declarations: &[&ProgramManifest],
) -> Result<()> {
    for (name, declared) in names.iter().zip(declarations) {
        validate_component(name, declared)?;
    }
    validate_supervisor_limits(manifest, declarations)
}

fn validate_component(name: &str, declared: &ProgramManifest) -> Result<()> {
    ensure!(
        declared.slots.len() <= COMPONENT_SLOT_MAX,
        "boot plan component '{name}' declares {} slots; \
         the startup transaction carries {COMPONENT_SLOT_MAX}",
        declared.slots.len()
    );
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

fn validate_supervisor_limits(
    manifest: &SystemManifest,
    declarations: &[&ProgramManifest],
) -> Result<()> {
    // Supervisor ждёт процессы компонентов вместе с потоком relay одним
    // массивом ожидания.
    let waits = manifest.components.len() + 1;
    ensure!(
        waits <= WAIT_MANY_MAX_COUNT,
        "boot plan composition of {} components needs {waits} supervisor wait objects; \
         the ceiling is {WAIT_MANY_MAX_COUNT}",
        manifest.components.len()
    );

    let slots: usize = declarations
        .iter()
        .map(|declared| declared.slots.len())
        .sum();
    // Оценка сверху: у supervisor bootstrap-капа, регион образа с отображением,
    // лог-порт relay и его поток, собственный ресурс, а у каждого компонента -
    // резерв бюджета, ресурс incarnation, процесс, две стороны control-порта,
    // минтер service-порта и лог-сессия.
    let handles = SUPERVISOR_HANDLES + manifest.components.len() * COMPONENT_HANDLES + slots;
    ensure!(
        handles <= HANDLE_TABLE_CAPACITY as usize,
        "boot plan composition of {} components needs up to {handles} rootkeeper handles; \
         the table holds {HANDLE_TABLE_CAPACITY}",
        manifest.components.len()
    );
    Ok(())
}

fn compile_slots<'a>(
    name: &'a str,
    declared: &ProgramManifest,
    selectors: &'a [Vec<&'a str>],
    files: &'a [Vec<String>],
    declarations: &[&ProgramManifest],
    names: &[&str],
    records: &[&str],
) -> Result<Vec<SlotSpec<'a>>> {
    declared
        .slots
        .iter()
        .zip(selectors)
        .zip(files)
        .map(|((slot, compatible), file)| {
            let device = DeviceSpec {
                compatible,
                // Экземпляр среди совместимых устройств всегда нулевой:
                // назвать второй композиции нечем.
                instance: 0,
                resource: slot.resource,
            };
            match slot.kind {
                SlotKind::Image => Ok(SlotSpec::Image),
                SlotKind::Irq => Ok(SlotSpec::Irq(device)),
                SlotKind::Mmio => Ok(SlotSpec::Mmio(device)),
                SlotKind::Service => {
                    let protocol = compatible.first().copied().with_context(|| {
                        format!(
                            "boot plan component '{name}' declares a service slot without a protocol"
                        )
                    })?;
                    Ok(SlotSpec::Service {
                        provider: find_provider(name, protocol, declarations, names)?,
                    })
                }
                SlotKind::File => {
                    let path = file.first().map(String::as_str).with_context(|| {
                        format!("boot plan component '{name}' declares a file slot without a name")
                    })?;
                    ensure!(
                        records.contains(&path),
                        "boot plan component '{name}' takes file '{path}', \
                         which its directory does not contain"
                    );
                    Ok(SlotSpec::File { path })
                }
            }
        })
        .collect()
}

fn find_provider(
    name: &str,
    protocol: &str,
    declarations: &[&ProgramManifest],
    names: &[&str],
) -> Result<u16> {
    let exporters: Vec<usize> = declarations
        .iter()
        .enumerate()
        .filter(|(_, declared)| declared.export.as_deref() == Some(protocol))
        .map(|(index, _)| index)
        .collect();
    let [index] = exporters[..] else {
        if exporters.is_empty() {
            bail!(
                "boot plan component '{name}' imports protocol '{protocol}', \
                 which no component of the composition exports"
            );
        }
        let exporters: Vec<&str> = exporters.iter().map(|index| names[*index]).collect();
        bail!(
            "boot plan component '{name}' imports protocol '{protocol}', \
             which {} components export: {}",
            exporters.len(),
            exporters.join(", ")
        );
    };
    ensure!(
        names[index] != name,
        "boot plan component '{name}' imports the protocol it exports itself"
    );
    u16::try_from(index).context("component index fits the plan")
}

fn service_edges(slots: &[SlotSpec<'_>]) -> Vec<u16> {
    // Импорт сервиса поднимает провайдера раньше потребителя: ребро выводится
    // из связывания.
    let mut edges: Vec<u16> = slots
        .iter()
        .filter_map(|slot| match slot {
            SlotSpec::Service { provider } => Some(*provider),
            _ => None,
        })
        .collect();
    edges.sort_unstable();
    edges.dedup();
    edges
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

fn validate_acyclic(requires: &[Vec<u16>], after: &[Vec<u16>], names: &[&str]) -> Result<()> {
    // Порядок запуска - топологический по объединению requires и after: цикл
    // делает его невыразимым.
    let edges = |index: usize| requires[index].iter().chain(&after[index]);
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

    const ROUTER_NAME: &str = "namespace-router";
    const DRIVER_NAME: &str = "driver-pl031";
    const CONSUMER_NAME: &str = "namespace-consumer-fixture";

    const RTC_COMPATIBLE: &[&str] = &["arm,pl031", "arm,primecell"];
    const NAMESPACE: &str = "Namespace";

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
        }
    }

    fn router_declaration() -> ProgramManifest {
        declaration(
            vec![
                slot(SlotKind::Image, 0, &[]),
                slot(SlotKind::File, 0, &["mounts"]),
            ],
            Some(NAMESPACE),
        )
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
        declaration(vec![slot(SlotKind::Service, 0, &[NAMESPACE])], None)
    }

    struct Image {
        entries: Vec<(String, Option<ProgramManifest>)>,
        records: Vec<String>,
    }

    impl Image {
        fn new() -> Self {
            let mut entries = vec![
                (String::from(ROOTKEEPER), None),
                (String::from(ROUTER), Some(router_declaration())),
                (String::from(DRIVER), Some(device_declaration())),
                (String::from(CONSUMER), Some(consumer_declaration())),
                (
                    String::from(IMAGE_FS),
                    Some(declaration(vec![slot(SlotKind::Image, 0, &[])], None)),
                ),
                (String::from(RAM_FS), None),
            ];
            for index in 0..DRIVER_FAMILY {
                entries.push((driver_family_member(index), Some(device_declaration())));
            }
            Self {
                entries,
                records: vec![format!("{ROUTER}/mounts")],
            }
        }

        fn with(mut self, path: &str, manifest: ProgramManifest) -> Self {
            self.entries.push((String::from(path), Some(manifest)));
            self
        }

        fn with_record(mut self, path: &str) -> Self {
            self.records.push(String::from(path));
            self
        }

        fn programs(&self) -> Vec<PlanProgram<'_>> {
            self.entries
                .iter()
                .map(|(path, manifest)| PlanProgram {
                    path,
                    manifest: manifest.as_ref(),
                })
                .collect()
        }

        fn compile(&self, source: &str) -> Result<Vec<u8>> {
            let records: Vec<&str> = self.records.iter().map(String::as_str).collect();
            compile_boot_plan(source, &self.programs(), &records)
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
    fn plan_takes_every_slot_from_the_program_declaration() {
        let bytes = compiled(&plan(&[filesystem(""), rtc("")]));

        let plan = decode_plan(&bytes).expect("compiled plan must decode");
        assert_eq!(plan.len(), 2);

        let router = plan.component(0).expect("router");
        assert_eq!(router.executable(), ROUTER);
        assert_eq!(
            router.slots().collect::<Vec<_>>(),
            [
                SlotSource::Image,
                SlotSource::File {
                    path: "/boot/namespace-router/mounts"
                }
            ]
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
    fn plan_resolves_a_file_under_the_component_directory() {
        const OWNER: &str = "/boot/owner";
        let image = Image::new()
            .with(
                OWNER,
                declaration(vec![slot(SlotKind::File, 0, &["data"])], None),
            )
            .with_record("/boot/owner/data");
        let source = plan(&[rtc("").replace(DRIVER, OWNER)]);

        let bytes = image.compile(&source).expect("compile must succeed");
        let plan = decode_plan(&bytes).expect("compiled plan must decode");
        assert_eq!(
            plan.component(0).expect("owner").slots().next(),
            Some(SlotSource::File {
                path: "/boot/owner/data"
            })
        );
    }

    #[test]
    fn plan_rejects_a_file_the_component_directory_does_not_contain() {
        const OWNER: &str = "/boot/owner";
        let image = Image::new().with(
            OWNER,
            declaration(vec![slot(SlotKind::File, 0, &["ghost"])], None),
        );
        let source = plan(&[rtc("").replace(DRIVER, OWNER)]);

        let error = image.compile(&source).expect_err("the record is absent");
        assert_eq!(
            format!("{error:#}"),
            "boot plan component 'owner' takes file '/boot/owner/ghost', \
             which its directory does not contain"
        );
    }

    #[test]
    fn plan_binds_an_imported_protocol_to_its_exporter() {
        let bytes = compiled(&plan(&[filesystem(""), consumer("")]));

        let plan = decode_plan(&bytes).expect("compiled plan must decode");
        let consumer = plan.component(1).expect("consumer");
        assert_eq!(
            consumer.slots().collect::<Vec<_>>(),
            [SlotSource::Service { provider: 0 }]
        );
        // Ребро зависимости выведено из связывания: рукой оно не написано.
        assert_eq!(consumer.requires().collect::<Vec<_>>(), [0]);
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
                .requires()
                .collect::<Vec<_>>(),
            [1]
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
        assert_eq!(rtc.after().collect::<Vec<_>>(), [0]);
        assert_eq!(rtc.requires().count(), 0);
    }

    #[test]
    fn plan_rejects_an_import_no_component_exports() {
        let source = plan(&[rtc(""), consumer("")]);
        assert_eq!(
            rejected(&source),
            format!(
                "boot plan component '{CONSUMER_NAME}' imports protocol '{NAMESPACE}', \
                 which no component of the composition exports"
            )
        );
    }

    #[test]
    fn plan_rejects_an_import_two_components_export() {
        const TWIN: &str = "/boot/namespace-router-twin";
        let image = Image::new()
            .with(TWIN, router_declaration())
            .with_record("/boot/namespace-router-twin/mounts");
        let twin = filesystem("").replace(ROUTER, TWIN);
        let source = plan(&[filesystem(""), twin, consumer("")]);

        let error = image.compile(&source).expect_err("the choice is ambiguous");
        assert_eq!(
            format!("{error:#}"),
            format!(
                "boot plan component '{CONSUMER_NAME}' imports protocol '{NAMESPACE}', \
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
                vec![slot(SlotKind::Service, 0, &[NAMESPACE])],
                Some(NAMESPACE),
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
            [0]
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
    fn plan_rejects_a_program_without_a_manifest() {
        let source = plan(&[filesystem("")]).replace(ROUTER, ROOTKEEPER);
        assert_eq!(
            rejected(&source),
            format!("boot plan component '{ROOTKEEPER}' has no component manifest section")
        );
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
                    .map(|_| slot(SlotKind::Image, 0, &[]))
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
                WAIT_MANY_MAX_COUNT + 1
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
