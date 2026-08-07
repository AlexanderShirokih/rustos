use anyhow::{Context, Result, anyhow, bail, ensure};
use component_abi::{COMPONENT_SLOT_MAX, ComponentManifest, SlotKind};
use serde::Deserialize;
use syscall::{HANDLE_TABLE_CAPACITY, MAX_RESOURCE_DEPTH, Timeout, WAIT_MANY_MAX_COUNT};
use userland_image::{ComponentSpec, PlanDevice, SlotSource, encode_plan};

use crate::host::validate_image_path;

const BOOT_PLAN_MANIFEST_VERSION: u16 = 2;

const BOOT_COMPOSITION_DEPTH: u16 = 0;

const COMPONENT_RESOURCE_LEVELS: u16 = 3;

const SUPERVISOR_HANDLES: usize = 16;

const COMPONENT_HANDLES: usize = 12;

const SLOT_IMAGE: &str = "image";
const SLOT_IRQ: &str = "irq";
const SLOT_MMIO: &str = "mmio";
const SLOT_SERVICE: &str = "service";

#[derive(Clone, Copy, Debug)]
pub(crate) struct PlanProgram<'a> {
    pub path: &'a str,
    pub manifest: Option<ComponentManifest>,
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
    id: String,
    program: String,
    #[serde(default)]
    config: Option<String>,
    budget_pages: u64,
    startup_timeout_ms: u64,
    critical: bool,
    restart: u32,
    #[serde(default)]
    slots: Vec<SlotEntry>,
    #[serde(default)]
    requires: Vec<String>,
    #[serde(default)]
    after: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SlotEntry {
    source: String,
    compatible: Option<String>,
    instance: Option<u16>,
    resource: Option<u16>,
    provider: Option<String>,
}

fn default_plan_version() -> u16 {
    BOOT_PLAN_MANIFEST_VERSION
}

pub(crate) fn compile_boot_plan(
    source: &str,
    programs: &[PlanProgram<'_>],
    configs: &[&str],
) -> Result<Vec<u8>> {
    let manifest = parse_system_manifest(source)?;
    validate_composition(&manifest, programs, configs)?;

    let ids: Vec<&str> = manifest
        .components
        .iter()
        .map(|component| component.id.as_str())
        .collect();
    let slots = manifest
        .components
        .iter()
        .map(|component| compile_slots(component, &ids))
        .collect::<Result<Vec<_>>>()?;
    for (component, slots) in manifest.components.iter().zip(&slots) {
        validate_declaration(component, slots, programs)?;
    }
    let requires = manifest
        .components
        .iter()
        .map(|component| resolve_edges(component, &component.requires, "requires", &ids))
        .collect::<Result<Vec<_>>>()?;
    let after = manifest
        .components
        .iter()
        .map(|component| resolve_edges(component, &component.after, "after", &ids))
        .collect::<Result<Vec<_>>>()?;
    validate_acyclic(&requires, &after, &ids)?;

    let components: Vec<ComponentSpec<'_>> = manifest
        .components
        .iter()
        .enumerate()
        .map(|(index, component)| ComponentSpec {
            id: &component.id,
            program: &component.program,
            config: component.config.as_deref(),
            slots: &slots[index],
            requires: &requires[index],
            after: &after[index],
            budget_pages: component.budget_pages,
            startup_timeout: Timeout::from_millis(component.startup_timeout_ms),
            restart_limit: component.restart,
            critical: component.critical,
        })
        .collect();
    encode_plan(&components).map_err(|err| anyhow!("failed to serialize boot plan: {err:?}"))
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
    Ok(manifest)
}

fn validate_composition(
    manifest: &SystemManifest,
    programs: &[PlanProgram<'_>],
    configs: &[&str],
) -> Result<()> {
    // Компонент проверяется целиком, затем композиция: у каждой проверки своя
    // названная причина отказа.
    ensure!(
        !manifest.components.is_empty(),
        "boot plan must contain at least one component; found none"
    );
    for component in &manifest.components {
        validate_component(component, programs, configs)?;
    }
    for (index, component) in manifest.components.iter().enumerate() {
        for earlier in &manifest.components[..index] {
            ensure!(
                earlier.id != component.id,
                "boot plan component id '{}' is used more than once",
                component.id
            );
        }
    }
    validate_supervisor_limits(manifest)
}

fn validate_component(
    component: &ComponentEntry,
    programs: &[PlanProgram<'_>],
    configs: &[&str],
) -> Result<()> {
    ensure!(
        !component.id.is_empty(),
        "boot plan component must set a non-empty 'id'"
    );
    ensure!(
        component.budget_pages != 0,
        "boot plan component '{}' must set a non-zero 'budget_pages'",
        component.id
    );
    ensure!(
        component.startup_timeout_ms != 0,
        "boot plan component '{}' must set a non-zero 'startup_timeout_ms'",
        component.id
    );
    ensure!(
        component.restart >= 1,
        "boot plan component '{}' must set 'restart' to at least one launch",
        component.id
    );
    ensure!(
        component.slots.len() <= COMPONENT_SLOT_MAX,
        "boot plan component '{}' declares {} slots; the startup transaction carries {}",
        component.id,
        component.slots.len(),
        COMPONENT_SLOT_MAX
    );
    validate_program(&component.id, &component.program, programs)?;
    validate_config(component, configs)?;
    validate_resource_depth(&component.id, COMPONENT_RESOURCE_LEVELS)
}

fn validate_config(component: &ComponentEntry, configs: &[&str]) -> Result<()> {
    let Some(config) = &component.config else {
        return Ok(());
    };
    validate_image_path(config).with_context(|| {
        format!(
            "boot plan component '{}' names an invalid config record",
            component.id
        )
    })?;
    ensure!(
        configs.contains(&config.as_str()),
        "boot plan component '{}' names config record '{}' \
         that the image does not compile",
        component.id,
        config
    );
    // Запись конфигурации компонент читает из образа: без слота образа читать
    // её нечем.
    ensure!(
        component.slots.iter().any(|slot| slot.source == SLOT_IMAGE),
        "boot plan component '{}' names a config record without an 'image' slot to read it from",
        component.id
    );
    Ok(())
}

fn validate_program(id: &str, program: &str, programs: &[PlanProgram<'_>]) -> Result<()> {
    validate_image_path(program)
        .with_context(|| format!("boot plan component '{id}' names an invalid program"))?;
    ensure!(
        find_program(program, programs).is_some(),
        "boot plan component '{id}' names program '{program}' \
         that the image does not contain"
    );
    Ok(())
}

fn find_program<'a>(path: &str, programs: &'a [PlanProgram<'_>]) -> Option<&'a PlanProgram<'a>> {
    programs.iter().find(|program| program.path == path)
}

fn validate_declaration(
    component: &ComponentEntry,
    slots: &[SlotSource<'_>],
    programs: &[PlanProgram<'_>],
) -> Result<()> {
    let id = &component.id;
    let program = &component.program;
    let declared = find_program(program, programs)
        .and_then(|program| program.manifest)
        .with_context(|| {
            format!(
                "boot plan component '{id}' names program '{program}' \
                 without a component manifest section"
            )
        })?;

    ensure!(
        declared.slots().len() == slots.len(),
        "boot plan component '{id}' declares {} slots; program '{program}' declares {}",
        slots.len(),
        declared.slots().len()
    );
    for (index, (planned, declared)) in slots.iter().zip(declared.slots()).enumerate() {
        let planned = slot_kind(planned);
        ensure!(
            planned == *declared,
            "boot plan component '{id}' declares a '{}' slot {index}; \
             program '{program}' declares '{}'",
            slot_source_name(planned),
            slot_source_name(*declared)
        );
    }
    match (component.config.is_some(), declared.config()) {
        (true, false) => bail!(
            "boot plan component '{id}' names a config record; \
             program '{program}' declares none"
        ),
        (false, true) => bail!(
            "boot plan component '{id}' names no config record; \
             program '{program}' declares one"
        ),
        _ => Ok(()),
    }
}

const fn slot_kind(slot: &SlotSource<'_>) -> SlotKind {
    match slot {
        SlotSource::Image => SlotKind::Image,
        SlotSource::Irq(_) => SlotKind::Irq,
        SlotSource::Mmio(_) => SlotKind::Mmio,
        SlotSource::Service { .. } => SlotKind::Service,
    }
}

const fn slot_source_name(kind: SlotKind) -> &'static str {
    match kind {
        SlotKind::Image => SLOT_IMAGE,
        SlotKind::Irq => SLOT_IRQ,
        SlotKind::Mmio => SLOT_MMIO,
        SlotKind::Service => SLOT_SERVICE,
    }
}

fn validate_resource_depth(id: &str, levels: u16) -> Result<()> {
    // Дерево метеринга компонента укладывается в потолок глубины split: резерв
    // компонента, ресурс incarnation и ресурс поднятого ею потомка.
    ensure!(
        BOOT_COMPOSITION_DEPTH + levels <= MAX_RESOURCE_DEPTH,
        "boot plan component '{id}' needs {levels} resource levels \
         under depth {BOOT_COMPOSITION_DEPTH}; the ceiling is {MAX_RESOURCE_DEPTH}"
    );
    Ok(())
}

fn validate_supervisor_limits(manifest: &SystemManifest) -> Result<()> {
    // Supervisor ждёт процессы компонентов вместе с потоком relay одним
    // массивом ожидания.
    let waits = manifest.components.len() + 1;
    ensure!(
        waits <= WAIT_MANY_MAX_COUNT,
        "boot plan composition of {} components needs {waits} supervisor wait objects; \
         the ceiling is {WAIT_MANY_MAX_COUNT}",
        manifest.components.len()
    );

    let slots: usize = manifest
        .components
        .iter()
        .map(|component| component.slots.len())
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

fn compile_slots<'a>(component: &'a ComponentEntry, ids: &[&str]) -> Result<Vec<SlotSource<'a>>> {
    component
        .slots
        .iter()
        .map(|slot| compile_slot(component, slot, ids))
        .collect()
}

fn compile_slot<'a>(
    component: &'a ComponentEntry,
    slot: &'a SlotEntry,
    ids: &[&str],
) -> Result<SlotSource<'a>> {
    let id = &component.id;
    match slot.source.as_str() {
        SLOT_IMAGE => {
            reject_device_fields(id, slot)?;
            reject_provider(id, slot)?;
            Ok(SlotSource::Image)
        }
        SLOT_SERVICE => {
            reject_device_fields(id, slot)?;
            let provider = slot.provider.as_deref().with_context(|| {
                format!("boot plan component '{id}' declares a service slot without a 'provider'")
            })?;
            ensure!(
                provider != id,
                "boot plan component '{id}' takes its own service as a slot"
            );
            let index = ids.iter().position(|known| *known == provider);
            let index = index.with_context(|| {
                format!(
                    "boot plan component '{id}' takes the service of '{provider}', \
                     which the composition does not contain"
                )
            })?;
            Ok(SlotSource::Service {
                provider: u16::try_from(index).context("component index fits the plan")?,
            })
        }
        SLOT_IRQ | SLOT_MMIO => {
            reject_provider(id, slot)?;
            let compatible = slot.compatible.as_deref().with_context(|| {
                format!(
                    "boot plan component '{id}' declares a '{}' slot without a 'compatible'",
                    slot.source
                )
            })?;
            ensure!(
                !compatible.is_empty(),
                "boot plan component '{id}' declares a '{}' slot with an empty 'compatible'",
                slot.source
            );
            let device = PlanDevice {
                compatible,
                instance: slot.instance.unwrap_or(0),
                resource: slot.resource.unwrap_or(0),
            };
            Ok(if slot.source == SLOT_IRQ {
                SlotSource::Irq(device)
            } else {
                SlotSource::Mmio(device)
            })
        }
        source => bail!("boot plan component '{id}' declares a slot of unknown source '{source}'"),
    }
}

fn reject_device_fields(id: &str, slot: &SlotEntry) -> Result<()> {
    for (key, present) in [
        ("compatible", slot.compatible.is_some()),
        ("instance", slot.instance.is_some()),
        ("resource", slot.resource.is_some()),
    ] {
        ensure!(
            !present,
            "boot plan component '{id}' sets '{key}' on a '{}' slot",
            slot.source
        );
    }
    Ok(())
}

fn reject_provider(id: &str, slot: &SlotEntry) -> Result<()> {
    ensure!(
        slot.provider.is_none(),
        "boot plan component '{id}' sets 'provider' on a '{}' slot",
        slot.source
    );
    Ok(())
}

fn resolve_edges(
    component: &ComponentEntry,
    edges: &[String],
    key: &str,
    ids: &[&str],
) -> Result<Vec<u16>> {
    let id = &component.id;
    edges
        .iter()
        .map(|edge| {
            ensure!(
                edge != id,
                "boot plan component '{id}' names itself in '{key}'"
            );
            let index = ids
                .iter()
                .position(|known| known == edge)
                .with_context(|| {
                    format!(
                        "boot plan component '{id}' names '{edge}' in '{key}', \
                     which the composition does not contain"
                    )
                })?;
            u16::try_from(index).context("component index fits the plan")
        })
        .collect()
}

fn validate_acyclic(requires: &[Vec<u16>], after: &[Vec<u16>], ids: &[&str]) -> Result<()> {
    // Порядок запуска - топологический по объединению requires и after: цикл
    // делает его невыразимым.
    let edges = |index: usize| requires[index].iter().chain(&after[index]);
    let mut dependents: Vec<usize> = vec![0; ids.len()];
    for index in 0..ids.len() {
        for edge in edges(index) {
            dependents[usize::from(*edge)] += 1;
        }
    }
    let mut ready: Vec<usize> = (0..ids.len())
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
    if ordered != ids.len() {
        let looped: Vec<&str> = (0..ids.len())
            .filter(|index| dependents[*index] != 0)
            .map(|index| ids[index])
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
    use component_abi::{COMPONENT_MANIFEST_HEADER_SIZE, decode_manifest, encode_manifest};
    use userland_image::{PlanDevice, SlotSource, decode_plan};

    use super::*;

    const ROUTER: &str = "/boot/namespace-router";
    const DRIVER: &str = "/boot/driver-pl031";
    const IMAGE_FS: &str = "/boot/filesystem-imagefs";
    const RAM_FS: &str = "/boot/filesystem-ramfs";
    const ROOTKEEPER: &str = "/boot/rootkeeper";
    const CONSUMER: &str = "/boot/namespace-consumer-fixture";

    const CONFIG: &str = "/boot/namespace.mounts";
    const CONFIGS: &[&str] = &[CONFIG];

    const RTC_COMPATIBLE: &str = "arm,pl031";

    fn filesystem(fields: &str) -> String {
        format!(
            "version = 2\n\
             \n\
             [[components]]\n\
             id = \"filesystem\"\n\
             program = \"{ROUTER}\"\n\
             budget_pages = 2048\n\
             startup_timeout_ms = 2000\n\
             critical = true\n\
             restart = 3\n\
             config = \"{CONFIG}\"\n\
             slots = [{{ source = \"image\" }}]\n\
             {fields}"
        )
    }

    fn rtc(fields: &str) -> String {
        format!(
            "\n[[components]]\n\
             id = \"rtc\"\n\
             program = \"{DRIVER}\"\n\
             budget_pages = 64\n\
             startup_timeout_ms = 1000\n\
             critical = false\n\
             restart = 1\n\
             slots = [\n\
             {{ source = \"irq\", compatible = \"{RTC_COMPATIBLE}\" }},\n\
             {{ source = \"mmio\", compatible = \"{RTC_COMPATIBLE}\" }},\n\
             ]\n\
             {fields}"
        )
    }

    fn declaration<const LEN: usize>(
        slots: &[SlotKind],
        config: bool,
    ) -> Option<ComponentManifest> {
        let bytes = encode_manifest::<LEN>(slots, config);
        Some(decode_manifest(&bytes).expect("declaration decodes"))
    }

    fn program(path: &str, manifest: Option<ComponentManifest>) -> PlanProgram<'_> {
        PlanProgram { path, manifest }
    }

    fn programs() -> Vec<PlanProgram<'static>> {
        vec![
            program(ROOTKEEPER, None),
            program(
                ROUTER,
                declaration::<{ COMPONENT_MANIFEST_HEADER_SIZE + 1 }>(&[SlotKind::Image], true),
            ),
            program(
                DRIVER,
                declaration::<{ COMPONENT_MANIFEST_HEADER_SIZE + 2 }>(
                    &[SlotKind::Irq, SlotKind::Mmio],
                    false,
                ),
            ),
            program(
                CONSUMER,
                declaration::<{ COMPONENT_MANIFEST_HEADER_SIZE + 1 }>(&[SlotKind::Service], false),
            ),
            program(
                IMAGE_FS,
                declaration::<{ COMPONENT_MANIFEST_HEADER_SIZE + 1 }>(&[SlotKind::Image], false),
            ),
            program(RAM_FS, None),
        ]
    }

    fn consumer(fields: &str) -> String {
        format!(
            "\n[[components]]\n\
             id = \"consumer\"\n\
             program = \"{CONSUMER}\"\n\
             budget_pages = 64\n\
             startup_timeout_ms = 1000\n\
             critical = false\n\
             restart = 1\n\
             slots = [{{ source = \"service\", provider = \"filesystem\" }}]\n\
             {fields}"
        )
    }

    fn compiled(source: &str) -> Vec<u8> {
        compile_boot_plan(source, &programs(), CONFIGS).expect("compile must succeed")
    }

    fn rejected(source: &str) -> String {
        compile_boot_plan(source, &programs(), CONFIGS)
            .expect_err("invalid composition must fail")
            .to_string()
    }

    #[test]
    fn plan_carries_every_component_with_its_slots_and_edges() {
        let source = format!("{}{}", filesystem(""), rtc("after = [\"filesystem\"]\n"));
        let bytes = compiled(&source);

        let plan = decode_plan(&bytes).expect("compiled plan must decode");
        assert_eq!(plan.len(), 2);

        let filesystem = plan.component(0).expect("first component");
        assert_eq!(filesystem.id(), "filesystem");
        assert_eq!(filesystem.program(), ROUTER);
        assert_eq!(filesystem.budget_pages(), 2048);
        assert_eq!(filesystem.startup_timeout(), Timeout::from_millis(2000));
        assert_eq!(filesystem.restart_limit(), 3);
        assert!(filesystem.critical());
        assert_eq!(filesystem.config(), Some(CONFIG));
        assert_eq!(filesystem.slots().collect::<Vec<_>>(), [SlotSource::Image]);

        let rtc = plan.component(1).expect("second component");
        assert_eq!(rtc.config(), None);
        assert!(!rtc.critical());
        assert_eq!(rtc.after().collect::<Vec<_>>(), [0]);
        let device = PlanDevice {
            compatible: RTC_COMPATIBLE,
            instance: 0,
            resource: 0,
        };
        assert_eq!(
            rtc.slots().collect::<Vec<_>>(),
            [SlotSource::Irq(device), SlotSource::Mmio(device)]
        );
    }

    #[test]
    fn plan_resolves_a_service_slot_to_its_provider() {
        let bytes = compiled(&format!("{}{}", filesystem(""), consumer("")));

        let plan = decode_plan(&bytes).expect("compiled plan must decode");
        assert_eq!(
            plan.component(1)
                .expect("consumer")
                .slots()
                .collect::<Vec<_>>(),
            [SlotSource::Service { provider: 0 }]
        );
    }

    #[test]
    fn plan_rejects_an_unsupported_version() {
        let source = filesystem("").replace("version = 2", "version = 3");
        assert_eq!(
            rejected(&source),
            "Unsupported boot plan version 3; expected 2"
        );
    }

    #[test]
    fn plan_rejects_a_component_without_an_id() {
        let source = filesystem("").replace("id = \"filesystem\"", "id = \"\"");
        assert_eq!(
            rejected(&source),
            "boot plan component must set a non-empty 'id'"
        );
    }

    #[test]
    fn plan_rejects_a_repeated_id() {
        let source = format!(
            "{}{}",
            filesystem(""),
            rtc("").replace("\"rtc\"", "\"filesystem\"")
        );
        assert_eq!(
            rejected(&source),
            "boot plan component id 'filesystem' is used more than once"
        );
    }

    #[test]
    fn plan_rejects_an_empty_composition() {
        assert_eq!(
            rejected("version = 2\ncomponents = []\n"),
            "boot plan must contain at least one component; found none"
        );
    }

    #[test]
    fn plan_rejects_a_zero_budget() {
        let source = filesystem("").replace("budget_pages = 2048", "budget_pages = 0");
        assert_eq!(
            rejected(&source),
            "boot plan component 'filesystem' must set a non-zero 'budget_pages'"
        );
    }

    #[test]
    fn plan_rejects_a_zero_startup_timeout() {
        let source = filesystem("").replace("startup_timeout_ms = 2000", "startup_timeout_ms = 0");
        assert_eq!(
            rejected(&source),
            "boot plan component 'filesystem' must set a non-zero 'startup_timeout_ms'"
        );
    }

    #[test]
    fn plan_rejects_a_component_that_never_launches() {
        let source = filesystem("").replace("restart = 3", "restart = 0");
        assert_eq!(
            rejected(&source),
            "boot plan component 'filesystem' must set 'restart' to at least one launch"
        );
    }

    #[test]
    fn plan_rejects_a_program_outside_the_image() {
        let source = filesystem("").replace(ROUTER, "/boot/ghost-router");
        assert_eq!(
            rejected(&source),
            "boot plan component 'filesystem' names program '/boot/ghost-router' \
             that the image does not contain"
        );
    }

    #[test]
    fn plan_rejects_a_program_without_a_manifest() {
        let source = filesystem("").replace(ROUTER, ROOTKEEPER);
        assert_eq!(
            rejected(&source),
            format!(
                "boot plan component 'filesystem' names program '{ROOTKEEPER}' \
                 without a component manifest section"
            )
        );
    }

    #[test]
    fn plan_rejects_slots_swapped_against_the_declaration() {
        let source = format!("{}{}", filesystem(""), rtc("")).replace(
            "{ source = \"irq\", compatible = \"arm,pl031\" },\n\
             { source = \"mmio\", compatible = \"arm,pl031\" },",
            "{ source = \"mmio\", compatible = \"arm,pl031\" },\n\
             { source = \"irq\", compatible = \"arm,pl031\" },",
        );
        assert_eq!(
            rejected(&source),
            format!(
                "boot plan component 'rtc' declares a 'mmio' slot 0; \
                 program '{DRIVER}' declares 'irq'"
            )
        );
    }

    #[test]
    fn plan_rejects_a_slot_kind_the_declaration_does_not_name() {
        let source = format!("{}{}", filesystem(""), rtc("")).replace(
            "{ source = \"mmio\", compatible = \"arm,pl031\" },",
            "{ source = \"service\", provider = \"filesystem\" },",
        );
        assert_eq!(
            rejected(&source),
            format!(
                "boot plan component 'rtc' declares a 'service' slot 1; \
                 program '{DRIVER}' declares 'mmio'"
            )
        );
    }

    #[test]
    fn plan_rejects_a_slot_count_past_the_declaration() {
        let source = format!("{}{}", filesystem(""), rtc("")).replace(
            "{ source = \"mmio\", compatible = \"arm,pl031\" },",
            "{ source = \"mmio\", compatible = \"arm,pl031\" },\n\
             { source = \"mmio\", compatible = \"arm,pl031\" },",
        );
        assert_eq!(
            rejected(&source),
            format!("boot plan component 'rtc' declares 3 slots; program '{DRIVER}' declares 2")
        );
    }

    #[test]
    fn plan_rejects_a_slot_the_declaration_names_and_the_plan_does_not() {
        let source = format!("{}{}", filesystem(""), rtc(""))
            .replace("{ source = \"mmio\", compatible = \"arm,pl031\" },", "");
        assert_eq!(
            rejected(&source),
            format!("boot plan component 'rtc' declares 1 slots; program '{DRIVER}' declares 2")
        );
    }

    #[test]
    fn plan_rejects_a_config_record_the_plan_does_not_name() {
        let source = filesystem("").replace(&format!("config = \"{CONFIG}\"\n"), "");
        assert_eq!(
            rejected(&source),
            format!(
                "boot plan component 'filesystem' names no config record; \
                 program '{ROUTER}' declares one"
            )
        );
    }

    #[test]
    fn plan_rejects_a_config_record_the_program_does_not_declare() {
        let source = filesystem("").replace(ROUTER, IMAGE_FS);
        assert_eq!(
            rejected(&source),
            format!(
                "boot plan component 'filesystem' names a config record; \
                 program '{IMAGE_FS}' declares none"
            )
        );
    }

    #[test]
    fn plan_rejects_a_config_record_the_image_does_not_compile() {
        let source = filesystem("").replace(CONFIG, "/boot/ghost.mounts");
        assert_eq!(
            rejected(&source),
            "boot plan component 'filesystem' names config record '/boot/ghost.mounts' \
             that the image does not compile"
        );
    }

    #[test]
    fn plan_rejects_a_config_path_outside_the_contract() {
        let source = filesystem("").replace(CONFIG, "boot/namespace.mounts");
        assert!(
            rejected(&source)
                .starts_with("boot plan component 'filesystem' names an invalid config record")
        );
    }

    #[test]
    fn plan_rejects_a_config_record_without_an_image_slot() {
        let source = filesystem("").replace("slots = [{ source = \"image\" }]", "slots = []");
        assert_eq!(
            rejected(&source),
            "boot plan component 'filesystem' names a config record \
             without an 'image' slot to read it from"
        );
    }

    #[test]
    fn plan_rejects_a_cycle_over_requires() {
        let source = format!(
            "{}{}",
            filesystem("requires = [\"rtc\"]\n"),
            rtc("requires = [\"filesystem\"]\n")
        );
        assert_eq!(
            rejected(&source),
            "boot plan dependencies form a cycle over components: filesystem, rtc"
        );
    }

    #[test]
    fn plan_rejects_a_cycle_over_after() {
        let source = format!(
            "{}{}",
            filesystem("after = [\"rtc\"]\n"),
            rtc("after = [\"filesystem\"]\n")
        );
        assert_eq!(
            rejected(&source),
            "boot plan dependencies form a cycle over components: filesystem, rtc"
        );
    }

    #[test]
    fn plan_rejects_a_self_dependency() {
        for key in ["requires", "after"] {
            let source = filesystem(&format!("{key} = [\"filesystem\"]\n"));
            assert_eq!(
                rejected(&source),
                format!("boot plan component 'filesystem' names itself in '{key}'")
            );
        }
    }

    #[test]
    fn plan_rejects_a_component_taking_its_own_service() {
        let source = filesystem("")
            .replace(
                "slots = [{ source = \"image\" }]",
                "slots = [{ source = \"service\", provider = \"filesystem\" }]",
            )
            .replace(&format!("config = \"{CONFIG}\"\n"), "");
        assert_eq!(
            rejected(&source),
            "boot plan component 'filesystem' takes its own service as a slot"
        );
    }

    #[test]
    fn plan_rejects_an_unknown_service_provider() {
        let source = format!(
            "{}{}",
            filesystem(""),
            rtc("").replace(
                "{ source = \"mmio\", compatible = \"arm,pl031\" },",
                "{ source = \"service\", provider = \"logger\" },"
            )
        );
        assert_eq!(
            rejected(&source),
            "boot plan component 'rtc' takes the service of 'logger', \
             which the composition does not contain"
        );
    }

    #[test]
    fn plan_rejects_an_unknown_edge() {
        let source = filesystem("requires = [\"logger\"]\n");
        assert_eq!(
            rejected(&source),
            "boot plan component 'filesystem' names 'logger' in 'requires', \
             which the composition does not contain"
        );
    }

    #[test]
    fn plan_rejects_a_slot_of_an_unknown_source() {
        let source = format!("{}{}", filesystem(""), rtc("")).replace("\"irq\"", "\"disk\"");
        assert_eq!(
            rejected(&source),
            "boot plan component 'rtc' declares a slot of unknown source 'disk'"
        );
    }

    #[test]
    fn plan_rejects_a_device_selector_on_a_slot_that_has_none() {
        let source = filesystem("").replace(
            "{ source = \"image\" }",
            "{ source = \"image\", compatible = \"arm,pl031\" }",
        );
        assert_eq!(
            rejected(&source),
            "boot plan component 'filesystem' sets 'compatible' on a 'image' slot"
        );
    }

    #[test]
    fn plan_rejects_a_device_slot_without_a_selector() {
        let source = filesystem("").replace(
            "{ source = \"image\" }",
            "{ source = \"image\" }, { source = \"irq\" }",
        );
        assert_eq!(
            rejected(&source),
            "boot plan component 'filesystem' declares a 'irq' slot without a 'compatible'"
        );
    }

    #[test]
    fn plan_rejects_more_slots_than_the_startup_transaction_carries() {
        let slots = format!(
            "slots = [{}]",
            ["{ source = \"image\" }"; COMPONENT_SLOT_MAX + 1].join(", ")
        );
        let source = filesystem("").replace("slots = [{ source = \"image\" }]", &slots);
        assert_eq!(
            rejected(&source),
            format!(
                "boot plan component 'filesystem' declares {} slots; \
                 the startup transaction carries {COMPONENT_SLOT_MAX}",
                COMPONENT_SLOT_MAX + 1
            )
        );
    }

    #[test]
    fn plan_rejects_an_unknown_key() {
        let source = filesystem("backoff_ms = 100\n");
        assert!(rejected(&source).starts_with("Failed to parse boot plan TOML"));
    }

    #[test]
    fn components_fit_the_resource_depth_ceiling() {
        validate_resource_depth("filesystem", COMPONENT_RESOURCE_LEVELS)
            .expect("a component fits the ceiling");

        let too_deep = validate_resource_depth("deep", MAX_RESOURCE_DEPTH + 1)
            .expect_err("a component past the ceiling must fail")
            .to_string();
        assert_eq!(
            too_deep,
            format!(
                "boot plan component 'deep' needs {} resource levels under depth 0; \
                 the ceiling is {MAX_RESOURCE_DEPTH}",
                MAX_RESOURCE_DEPTH + 1
            )
        );
    }

    #[test]
    fn plan_rejects_a_composition_past_the_supervisor_wait_ceiling() {
        let mut source = filesystem("");
        for index in 1..WAIT_MANY_MAX_COUNT {
            source.push_str(&rtc("").replace("\"rtc\"", &format!("\"rtc{index}\"")));
        }
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
        let mut source = filesystem("");
        for index in 1..=components {
            source.push_str(&rtc("").replace("\"rtc\"", &format!("\"rtc{index}\"")));
        }
        assert!(
            rejected(&source).contains("rootkeeper handles; the table holds"),
            "handle estimate must be the failing check"
        );
    }
}
