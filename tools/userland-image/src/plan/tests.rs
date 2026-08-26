use component_abi::{COMPONENT_SLOT_MAX, SlotKind};
use filesystem_abi::NAME_MAX;
use supervisor_abi::PLAN_RECORD_MAX;
use syscall::{
    HANDLE_TABLE_CAPACITY, MAX_PROCESS_NAME_LEN, MAX_RESOURCE_DEPTH, WAIT_MANY_MAX_COUNT,
};
use userland_image::{BootPlan, SlotSource, decode_plan};

use super::{
    CompiledStages, PlanProgram, compile_stages,
    limits::{
        COMPONENT_HANDLES, COMPONENT_RESOURCE_LEVELS, SUPERVISOR_HANDLES, SUPERVISOR_WAITS,
        validate_resource_depth,
    },
    source::BOOT_PLAN_MANIFEST_VERSION,
};
use crate::manifest::{ManifestSlot, ProgramManifest};

const ROUTER: &str = "namespace-router";
const DRIVER: &str = "driver-pl031";
const IMAGE_FS: &str = "filesystem-imagefs";
const RAM_FS: &str = "filesystem-ramfs";
const ROOTKEEPER: &str = "rootkeeper";
const CONSUMER: &str = "namespace-consumer-fixture";
const AREA: &str = "area-consumer";
const HELPER: &str = "helper";

const NAMESPACE_PROTOCOL: &str = "Namespace";
const NAMESPACE_ADMIN_PROTOCOL: &str = "NamespaceAdmin";
const FILE_SYSTEM_PROTOCOL: &str = "FileSystem";

const RTC_COMPATIBLE: &[&str] = &["arm,pl031", "arm,primecell"];

const DRIVER_FAMILY: usize = WAIT_MANY_MAX_COUNT;

const DEFAULT_MEMORY_LIMIT: &str = "256K";
const DEFAULT_FAILURE_POLICY: &str = "detach";

struct Component {
    executable: String,
    memory_limit: String,
    startup_timeout: Option<String>,
    on_failure: String,
    name: Option<String>,
    after: Vec<String>,
}

impl Component {
    fn new(executable: &str) -> Self {
        Self {
            executable: String::from(executable),
            memory_limit: String::from(DEFAULT_MEMORY_LIMIT),
            startup_timeout: None,
            on_failure: String::from(DEFAULT_FAILURE_POLICY),
            name: None,
            after: Vec::new(),
        }
    }

    fn executable(mut self, executable: &str) -> Self {
        self.executable = String::from(executable);
        self
    }

    fn memory_limit(mut self, limit: &str) -> Self {
        self.memory_limit = String::from(limit);
        self
    }

    fn startup_timeout(mut self, timeout: &str) -> Self {
        self.startup_timeout = Some(String::from(timeout));
        self
    }

    fn on_failure(mut self, policy: &str) -> Self {
        self.on_failure = String::from(policy);
        self
    }

    fn name(mut self, name: &str) -> Self {
        self.name = Some(String::from(name));
        self
    }

    fn after(mut self, edges: &[&str]) -> Self {
        self.after = edges.iter().map(|edge| String::from(*edge)).collect();
        self
    }

    fn render(&self) -> String {
        let startup_timeout = match &self.startup_timeout {
            Some(timeout) => format!("startup_timeout = \"{timeout}\"\n"),
            None => String::new(),
        };
        let name = match &self.name {
            Some(name) => format!("name = \"{name}\"\n"),
            None => String::new(),
        };
        let after = match self.after.as_slice() {
            [] => String::new(),
            edges => {
                let quoted: Vec<String> = edges.iter().map(|edge| format!("\"{edge}\"")).collect();
                format!("after = [{}]\n", quoted.join(", "))
            }
        };
        format!(
            "\n[[components]]\n\
             executable = \"{}\"\n\
             memory_limit = \"{}\"\n\
             on_failure = \"{}\"\n\
             {startup_timeout}{name}{after}",
            self.executable, self.memory_limit, self.on_failure
        )
    }
}

fn versioned(version: u16, components: &[Component]) -> String {
    let records: String = components.iter().map(Component::render).collect();
    format!("version = {version}\n{records}")
}

fn plan(components: &[Component]) -> String {
    versioned(BOOT_PLAN_MANIFEST_VERSION, components)
}

fn router() -> Component {
    Component::new(ROUTER)
        .memory_limit("8M")
        .startup_timeout("2s")
        .on_failure("restart 2 then halt")
}

fn image_filesystem() -> Component {
    Component::new(IMAGE_FS)
        .memory_limit("8M")
        .startup_timeout("2s")
        .on_failure("halt")
}

fn ram_filesystem() -> Component {
    Component::new(RAM_FS).memory_limit("4M")
}

fn rtc() -> Component {
    Component::new(DRIVER)
}

fn consumer() -> Component {
    Component::new(CONSUMER)
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
    declaration(vec![slot(SlotKind::Directory, 0, &[IMAGE_FS])], None)
}

struct ImageEntry {
    package: String,
    bin: String,
    path: String,
    manifest: ProgramManifest,
}

struct Image {
    entries: Vec<ImageEntry>,
}

fn image_path(bin: &str) -> String {
    format!("/boot/{bin}")
}

impl Image {
    fn new() -> Self {
        let mut image = Self {
            entries: Vec::new(),
        };
        for (bin, manifest) in [
            (ROOTKEEPER, ProgramManifest::default()),
            (ROUTER, router_declaration()),
            (DRIVER, device_declaration()),
            (CONSUMER, consumer_declaration()),
            (IMAGE_FS, image_filesystem_declaration()),
            (RAM_FS, ProgramManifest::default()),
        ] {
            image = image.with(bin, manifest);
        }
        for index in 0..DRIVER_FAMILY {
            image = image.with(&driver_family_member(index), device_declaration());
        }
        image
    }

    fn with(self, bin: &str, manifest: ProgramManifest) -> Self {
        let path = image_path(bin);
        self.with_path(bin, &path, manifest)
    }

    fn with_path(self, bin: &str, path: &str, manifest: ProgramManifest) -> Self {
        self.with_package(bin, bin, path, manifest)
    }

    fn with_package(
        mut self,
        package: &str,
        bin: &str,
        path: &str,
        manifest: ProgramManifest,
    ) -> Self {
        self.entries.push(ImageEntry {
            package: String::from(package),
            bin: String::from(bin),
            path: String::from(path),
            manifest,
        });
        self
    }

    fn programs(&self) -> Vec<PlanProgram<'_>> {
        self.entries
            .iter()
            .map(|entry| PlanProgram {
                package: &entry.package,
                bin: &entry.bin,
                path: &entry.path,
                manifest: &entry.manifest,
            })
            .collect()
    }

    fn compiled(&self, source: &str) -> Vec<u8> {
        compile_stages(source, None, &self.programs())
            .expect("compile must succeed")
            .plan
    }

    fn rejected(&self, source: &str) -> String {
        let error = compile_stages(source, None, &self.programs())
            .expect_err("invalid composition must fail");
        reason(&error)
    }

    fn staged(&self, plan: &str, stage_one: &str) -> CompiledStages {
        compile_stages(plan, Some(stage_one), &self.programs()).expect("compile must succeed")
    }

    fn staging_rejected(&self, plan: &str, stage_one: &str) -> String {
        let error = compile_stages(plan, Some(stage_one), &self.programs())
            .expect_err("invalid composition must fail");
        reason(&error)
    }
}

fn image_with_two_helpers() -> Image {
    Image::new()
        .with_package(
            "foo",
            HELPER,
            "/boot/foo-helper",
            ProgramManifest::default(),
        )
        .with_package(
            "bar",
            HELPER,
            "/boot/bar-helper",
            ProgramManifest::default(),
        )
}

fn driver_family_member(index: usize) -> String {
    format!("driver-{index}")
}

fn driver_family(drivers: usize) -> Vec<Component> {
    let mut components = vec![router()];
    components.extend((1..=drivers).map(|index| Component::new(&driver_family_member(index))));
    components
}

fn compiled(source: &str) -> Vec<u8> {
    Image::new().compiled(source)
}

fn rejected(source: &str) -> String {
    Image::new().rejected(source)
}

fn decoded(bytes: &[u8]) -> BootPlan<'_> {
    decode_plan(bytes).expect("compiled plan must decode")
}

fn slots_of(bytes: &[u8], index: u16) -> Vec<SlotSource<'_>> {
    decoded(bytes)
        .component(index)
        .expect("the plan carries the component")
        .slots()
        .collect()
}

fn reason(error: &anyhow::Error) -> String {
    // Причина читается вместе со своей цепочкой: значение и компонент,
    // который его назвал, лежат на разных её звеньях.
    format!("{error:#}")
}

#[test]
fn stage_one_record_carries_one_component() {
    let stages = Image::new().staged(&plan(&[image_filesystem()]), &plan(&[ram_filesystem()]));

    assert_eq!(decoded(&stages.plan).len(), 1);
    // `define` принимает запись плана ровно с одним компонентом, и владелец политики
    // загрузки подаёт её байты как есть.
    assert_eq!(stages.stage_one.len(), 1);
    assert_eq!(stages.stage_one[0].name, RAM_FS);
    let record = decoded(&stages.stage_one[0].bytes);
    assert_eq!(record.len(), 1);
    let component = record.component(0).expect("record carries its component");
    assert_eq!(component.executable(), image_path(RAM_FS));
    assert_eq!(component.name(), RAM_FS);
}

#[test]
fn stage_one_binds_a_slot_to_an_exporter_of_stage_zero() {
    let stages = Image::new().staged(&plan(&[router()]), &plan(&[consumer()]));

    // Имена экземпляров работают сквозь ступени: провайдер слота стоит в плане.
    assert_eq!(
        slots_of(&stages.stage_one[0].bytes, 0),
        [SlotSource::Service { provider: ROUTER }]
    );
}

#[test]
fn stage_one_rejects_a_name_taken_by_stage_zero() {
    assert_eq!(
        Image::new().staging_rejected(
            &plan(&[image_filesystem(), ram_filesystem()]),
            &plan(&[ram_filesystem()])
        ),
        format!("boot plan names component '{RAM_FS}' more than once")
    );
}

#[test]
fn stage_zero_rejects_a_dependency_on_stage_one() {
    assert_eq!(
        Image::new().staging_rejected(
            &plan(&[image_filesystem().after(&[RAM_FS])]),
            &plan(&[ram_filesystem()])
        ),
        format!(
            "boot plan component '{IMAGE_FS}' depends on '{RAM_FS}', \
             which stage 1 raises"
        )
    );
}

#[test]
fn stage_one_rejects_a_name_longer_than_an_image_record() {
    let name = "n".repeat(NAME_MAX + 1);

    assert_eq!(
        Image::new().staging_rejected(
            &plan(&[image_filesystem()]),
            &plan(&[ram_filesystem().name(&name)])
        ),
        format!(
            "stage 1 component name '{name}' is longer than {NAME_MAX} bytes, \
             which an image record name holds"
        )
    );
}

#[test]
fn stage_one_rejects_a_record_past_the_transaction_bound() {
    const BULKY: &str = "bulky-driver";
    let compatible = format!("arm,{}", "d".repeat(200));
    let slots = (0..COMPONENT_SLOT_MAX)
        .map(|_| slot(SlotKind::Irq, 0, &[&compatible, &compatible]))
        .collect();

    let error = Image::new()
        .with(BULKY, declaration(slots, None))
        .staging_rejected(
            &plan(&[image_filesystem()]),
            &plan(&[Component::new(BULKY)]),
        );

    assert!(
        error.contains(&format!(
            "the extension transaction carries {PLAN_RECORD_MAX}"
        )),
        "the record bound must be the failing check: {error}"
    );
}

#[test]
fn stage_one_rejects_a_cycle_of_its_own() {
    let first = ram_filesystem().after(&[DRIVER]);
    let second = rtc().after(&[RAM_FS]);

    // Порядок запуска топологический по объединению ступеней, и цикл внутри ступени 1
    // делает его невыразимым так же, как цикл в плане.
    assert_eq!(
        Image::new().staging_rejected(&plan(&[image_filesystem()]), &plan(&[first, second])),
        format!(
            "boot plan dependencies form a cycle over components: \
             {RAM_FS}, {DRIVER}"
        )
    );
}

#[test]
fn plan_takes_every_slot_from_the_program_declaration() {
    let bytes = compiled(&plan(&[image_filesystem(), rtc()]));

    let plan = decoded(&bytes);
    assert_eq!(plan.len(), 2);

    let filesystem = plan.component(0).expect("image filesystem");
    assert_eq!(filesystem.executable(), image_path(IMAGE_FS));
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
    const FILE_SYSTEM: &str = "filesystem-establishing";
    const READER: &str = "file-reader";

    let provider = ProgramManifest {
        establishes_sessions: true,
        ..declaration(vec![], Some(FILE_SYSTEM_PROTOCOL))
    };
    let reader = declaration(
        vec![slot(SlotKind::Service, 0, &[FILE_SYSTEM_PROTOCOL])],
        None,
    );
    let bytes = Image::new()
        .with(FILE_SYSTEM, provider)
        .with(READER, reader)
        .compiled(&plan(&[
            router().executable(FILE_SYSTEM),
            Component::new(READER),
        ]));

    let plan = decoded(&bytes);
    let provider = plan.component(0).expect("file system");
    assert!(provider.establishes_sessions());
    let reader = plan.component(1).expect("reader");
    assert!(!reader.establishes_sessions());
    assert_eq!(
        reader.slots().collect::<Vec<_>>(),
        [SlotSource::Service {
            provider: FILE_SYSTEM
        }]
    );
}

#[test]
fn plan_rejects_own_sessions_without_an_export() {
    const MUTE: &str = "mute-provider";
    let mute = ProgramManifest {
        establishes_sessions: true,
        ..declaration(vec![], None)
    };

    assert_eq!(
        Image::new()
            .with(MUTE, mute)
            .rejected(&plan(&[Component::new(MUTE)])),
        "boot plan component 'mute-provider' establishes its own sessions \
         without exporting a protocol"
    );
}

#[test]
fn plan_binds_an_imported_protocol_to_its_exporter() {
    let bytes = compiled(&plan(&[router(), consumer()]));

    assert_eq!(
        slots_of(&bytes, 1),
        [SlotSource::Service { provider: ROUTER }]
    );
}

#[test]
fn plan_binds_a_named_import_to_the_instance_it_names() {
    const TWIN: &str = "namespace-router-twin";
    const PICKY: &str = "picky-consumer";

    let picky = declaration(
        vec![slot(SlotKind::Service, 0, &[NAMESPACE_PROTOCOL, TWIN])],
        None,
    );
    let bytes = Image::new()
        .with(TWIN, router_declaration())
        .with(PICKY, picky)
        .compiled(&plan(&[
            router(),
            router().executable(TWIN),
            Component::new(PICKY),
        ]));

    assert_eq!(
        slots_of(&bytes, 2),
        [SlotSource::Service { provider: TWIN }]
    );
}

#[test]
fn plan_rejects_a_named_import_of_an_instance_outside_the_composition() {
    const PICKY: &str = "picky-consumer";
    let picky = declaration(
        vec![slot(SlotKind::Service, 0, &[NAMESPACE_PROTOCOL, "ghost"])],
        None,
    );

    assert_eq!(
        Image::new()
            .with(PICKY, picky)
            .rejected(&plan(&[router(), Component::new(PICKY)])),
        format!(
            "boot plan component 'picky-consumer' imports protocol '{NAMESPACE_PROTOCOL}' \
             from instance 'ghost', which the composition does not contain"
        )
    );
}

#[test]
fn plan_rejects_a_named_import_of_an_instance_that_exports_nothing() {
    const PICKY: &str = "picky-consumer";
    let picky = declaration(
        vec![slot(SlotKind::Service, 0, &[NAMESPACE_PROTOCOL, DRIVER])],
        None,
    );

    assert_eq!(
        Image::new()
            .with(PICKY, picky)
            .rejected(&plan(&[router(), rtc(), Component::new(PICKY)])),
        format!(
            "boot plan component 'picky-consumer' imports protocol '{NAMESPACE_PROTOCOL}' \
             from instance '{DRIVER}', which exports no such protocol"
        )
    );
}

#[test]
fn plan_orders_an_importer_after_its_exporter() {
    // Потребитель стоит в записи первым, а зависит от провайдера: ребро
    // ведёт вперёд по плану, и порядок остаётся выразимым.
    let bytes = compiled(&plan(&[consumer(), router()]));

    assert_eq!(
        slots_of(&bytes, 0),
        [SlotSource::Service { provider: ROUTER }]
    );
}

#[test]
fn plan_keeps_an_after_edge_without_an_import() {
    let bytes = compiled(&plan(&[router(), rtc().after(&[ROUTER])]));

    let plan = decoded(&bytes);
    let rtc = plan.component(1).expect("rtc");
    assert_eq!(rtc.after().collect::<Vec<_>>(), [ROUTER]);
    assert_eq!(rtc.slots().count(), 2);
}

#[test]
fn plan_rejects_an_import_no_component_exports() {
    assert_eq!(
        rejected(&plan(&[rtc(), consumer()])),
        format!(
            "boot plan component '{CONSUMER}' imports protocol '{NAMESPACE_PROTOCOL}', \
             which no component of the composition exports"
        )
    );
}

#[test]
fn plan_rejects_an_import_two_components_export() {
    const TWIN: &str = "namespace-router-twin";

    assert_eq!(
        Image::new()
            .with(TWIN, router_declaration())
            .rejected(&plan(&[router(), router().executable(TWIN), consumer()])),
        format!(
            "boot plan component '{CONSUMER}' imports protocol '{NAMESPACE_PROTOCOL}', \
             which 2 components export: {ROUTER}, namespace-router-twin"
        )
    );
}

#[test]
fn plan_rejects_a_component_importing_what_it_exports() {
    const SELF_BOUND: &str = "self-bound";
    let self_bound = declaration(
        vec![slot(SlotKind::Service, 0, &[NAMESPACE_PROTOCOL])],
        Some(NAMESPACE_PROTOCOL),
    );

    assert_eq!(
        Image::new()
            .with(SELF_BOUND, self_bound)
            .rejected(&plan(&[Component::new(SELF_BOUND)])),
        "boot plan component 'self-bound' imports the protocol it exports itself"
    );
}

#[test]
fn plan_binds_a_control_import_to_the_component_serving_it() {
    const ADMIN: &str = "mount-admin";
    let admin = declaration(
        vec![slot(SlotKind::Control, 0, &[NAMESPACE_ADMIN_PROTOCOL])],
        None,
    );
    let bytes = Image::new()
        .with(ADMIN, admin)
        .compiled(&plan(&[router(), Component::new(ADMIN)]));

    // Ребро зависимости выведено из связывания: провайдер поднимается раньше того, кто
    // зовёт его админ-операции.
    assert_eq!(
        slots_of(&bytes, 1),
        [SlotSource::Control { provider: ROUTER }]
    );
}

#[test]
fn plan_binds_a_named_control_import_to_the_instance_it_names() {
    const TWIN: &str = "namespace-router-twin";
    const ADMIN: &str = "mount-admin";

    let admin = declaration(
        vec![slot(
            SlotKind::Control,
            0,
            &[NAMESPACE_ADMIN_PROTOCOL, TWIN],
        )],
        None,
    );
    let bytes = Image::new()
        .with(TWIN, router_declaration())
        .with(ADMIN, admin)
        .compiled(&plan(&[
            router(),
            router().executable(TWIN),
            Component::new(ADMIN),
        ]));

    assert_eq!(
        slots_of(&bytes, 2),
        [SlotSource::Control { provider: TWIN }]
    );
}

#[test]
fn plan_rejects_a_control_import_no_component_serves() {
    const ADMIN: &str = "mount-admin";
    let admin = declaration(
        vec![slot(SlotKind::Control, 0, &[NAMESPACE_ADMIN_PROTOCOL])],
        None,
    );

    assert_eq!(
        Image::new()
            .with(ADMIN, admin)
            .rejected(&plan(&[rtc(), Component::new(ADMIN)])),
        format!(
            "boot plan component 'mount-admin' imports control protocol \
             '{NAMESPACE_ADMIN_PROTOCOL}', \
             which no component of the composition serves"
        )
    );
}

#[test]
fn plan_rejects_a_named_control_import_of_an_instance_that_serves_nothing() {
    const ADMIN: &str = "mount-admin";
    let admin = declaration(
        vec![slot(
            SlotKind::Control,
            0,
            &[NAMESPACE_ADMIN_PROTOCOL, DRIVER],
        )],
        None,
    );

    assert_eq!(
        Image::new()
            .with(ADMIN, admin)
            .rejected(&plan(&[router(), rtc(), Component::new(ADMIN)])),
        format!(
            "boot plan component 'mount-admin' imports control protocol \
             '{NAMESPACE_ADMIN_PROTOCOL}' from instance '{DRIVER}', \
             which serves no such protocol"
        )
    );
}

#[test]
fn plan_rejects_a_control_import_of_an_instance_outside_the_composition() {
    const ADMIN: &str = "mount-admin";
    let admin = declaration(
        vec![slot(
            SlotKind::Control,
            0,
            &[NAMESPACE_ADMIN_PROTOCOL, "ghost"],
        )],
        None,
    );

    assert_eq!(
        Image::new()
            .with(ADMIN, admin)
            .rejected(&plan(&[router(), Component::new(ADMIN)])),
        format!(
            "boot plan component 'mount-admin' imports control protocol \
             '{NAMESPACE_ADMIN_PROTOCOL}' from instance 'ghost', \
             which the composition does not contain"
        )
    );
}

#[test]
fn plan_rejects_a_component_importing_the_control_protocol_it_serves() {
    const SELF_BOUND: &str = "self-administered";
    let manifest = ProgramManifest {
        control: Some(String::from(NAMESPACE_ADMIN_PROTOCOL)),
        ..declaration(
            vec![slot(SlotKind::Control, 0, &[NAMESPACE_ADMIN_PROTOCOL])],
            None,
        )
    };

    assert_eq!(
        Image::new()
            .with(SELF_BOUND, manifest)
            .rejected(&plan(&[Component::new(SELF_BOUND)])),
        "boot plan component 'self-administered' imports the control protocol it serves itself"
    );
}

#[test]
fn plan_rejects_a_control_protocol_beside_own_session_establishment() {
    const CROWDED: &str = "crowded-control";
    let crowded = ProgramManifest {
        control: Some(String::from(NAMESPACE_ADMIN_PROTOCOL)),
        establishes_sessions: true,
        ..declaration(vec![], Some(FILE_SYSTEM_PROTOCOL))
    };

    // Кадры двух протоколов на одном порту разбирает один dispatch, и незнакомый
    // ordinal он отвергает.
    assert_eq!(
        Image::new()
            .with(CROWDED, crowded)
            .rejected(&plan(&[Component::new(CROWDED)])),
        format!(
            "boot plan component 'crowded-control' serves control protocol \
             '{NAMESPACE_ADMIN_PROTOCOL}' and establishes its own sessions; \
             the control port carries one protocol"
        )
    );
}

#[test]
fn plan_binds_a_directory_slot_to_the_named_instance() {
    let bytes = Image::new()
        .with(AREA, area_declaration())
        .compiled(&plan(&[image_filesystem(), Component::new(AREA)]));

    // Ребро зависимости выведено из связывания: файловая система поднимается
    // раньше того, кому выдаёт каталог.
    assert_eq!(
        slots_of(&bytes, 1),
        [SlotSource::Directory { provider: IMAGE_FS }]
    );
}

#[test]
fn plan_rejects_a_directory_slot_naming_its_own_component() {
    let area = declaration(vec![slot(SlotKind::Directory, 0, &[AREA])], None);

    assert_eq!(
        Image::new()
            .with(AREA, area)
            .rejected(&plan(&[image_filesystem(), Component::new(AREA)])),
        format!("boot plan component '{AREA}' takes its directory from itself")
    );
}

#[test]
fn plan_rejects_a_directory_slot_naming_an_instance_outside_the_composition() {
    assert_eq!(
        Image::new()
            .with(AREA, area_declaration())
            .rejected(&plan(&[rtc(), Component::new(AREA)])),
        format!(
            "boot plan component '{AREA}' takes its directory from instance \
             '{IMAGE_FS}', which the composition does not contain"
        )
    );
}

#[test]
fn plan_rejects_a_directory_slot_from_an_instance_without_own_sessions() {
    let area = declaration(vec![slot(SlotKind::Directory, 0, &[ROUTER])], None);

    // Область назначает supervisor, а сужает её сам провайдер: сминченный значок
    // области не несёт.
    assert_eq!(
        Image::new()
            .with(AREA, area)
            .rejected(&plan(&[router(), Component::new(AREA)])),
        format!(
            "boot plan component '{AREA}' takes its directory from instance \
             '{ROUTER}', which does not establish its own sessions"
        )
    );
}

#[test]
fn plan_rejects_a_directory_slot_of_a_component_the_provider_waits_for() {
    // Компонент, которого файловая система ждёт сама, каталога от неё не
    // получает: порядок запуска такой композиции невыразим.
    assert_eq!(
        Image::new()
            .with(AREA, area_declaration())
            .rejected(&plan(&[
                image_filesystem().after(&[AREA]),
                Component::new(AREA),
            ])),
        format!(
            "boot plan dependencies form a cycle over components: \
             {IMAGE_FS}, {AREA}"
        )
    );
}

#[test]
fn plan_rejects_an_unsupported_version() {
    assert_eq!(
        rejected(&versioned(3, &[router()])),
        "Unsupported boot plan version 3; expected 1"
    );
}

#[test]
fn a_component_without_a_name_is_called_by_the_program_identity() {
    let bytes = compiled(&plan(&[router()]));

    let plan = decoded(&bytes);
    let router = plan.component(0).expect("router");
    assert_eq!(router.executable(), image_path(ROUTER));
    assert_eq!(router.name(), ROUTER);
}

#[test]
fn a_component_keeps_its_name_when_the_program_moves() {
    const MOVED: &str = "moved-router";
    const ELSEWHERE: &str = "/system/router";

    let bytes = Image::new()
        .with_path(MOVED, ELSEWHERE, router_declaration())
        .compiled(&plan(&[router().executable(MOVED)]));

    let plan = decoded(&bytes);
    let router = plan.component(0).expect("router");
    // Запись плана несёт размещение, а имя экземпляра идёт от идентичности:
    // перемещение программы описание композиции не трогает.
    assert_eq!(router.executable(), ELSEWHERE);
    assert_eq!(router.name(), MOVED);
}

#[test]
fn a_named_component_keeps_its_name_apart_from_the_program() {
    let bytes = compiled(&plan(&[router().name("names")]));

    let plan = decoded(&bytes);
    let router = plan.component(0).expect("router");
    assert_eq!(router.executable(), image_path(ROUTER));
    assert_eq!(router.name(), "names");
}

#[test]
fn one_program_carries_two_named_instances() {
    let bytes = compiled(&plan(&[
        router().name("names-boot"),
        router().name("names-spare"),
    ]));

    let plan = decoded(&bytes);
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
    let bytes = compiled(&plan(&[
        router().name("names-boot"),
        router().name("names-spare").after(&["names-boot"]),
    ]));

    assert_eq!(
        decoded(&bytes)
            .component(1)
            .expect("second instance")
            .after()
            .collect::<Vec<_>>(),
        ["names-boot"]
    );
}

#[test]
fn plan_rejects_a_name_outside_the_grammar() {
    let long = "n".repeat(MAX_PROCESS_NAME_LEN + 1);
    for name in ["", "boot/names", "имя", &long] {
        assert!(
            rejected(&plan(&[router().name(name)])).starts_with("boot plan component name"),
            "'{name}' must fail on the component name"
        );
    }
}

#[test]
fn plan_rejects_a_repeated_name() {
    assert_eq!(
        rejected(&plan(&[router(), Component::new(ROUTER)])),
        format!("boot plan names component '{ROUTER}' more than once")
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
    for limit in ["0", "8MB", "5000", ""] {
        assert!(
            rejected(&plan(&[router().memory_limit(limit)])).starts_with(&format!(
                "boot plan component '{ROUTER}' sets 'memory_limit'"
            )),
            "'{limit}' must fail on the memory limit"
        );
    }
}

#[test]
fn plan_rejects_a_startup_timeout_outside_the_grammar() {
    assert!(
        rejected(&plan(&[router().startup_timeout("0s")])).starts_with(&format!(
            "boot plan component '{ROUTER}' sets 'startup_timeout'"
        ))
    );
}

#[test]
fn plan_rejects_a_failure_policy_outside_the_grammar() {
    for policy in [
        "",
        "reboot",
        "restart 2",
        "restart two then halt",
        "restart 0 then halt",
        "restart 2 then reboot",
    ] {
        assert!(
            rejected(&plan(&[router().on_failure(policy)]))
                .starts_with(&format!("boot plan component '{ROUTER}' sets 'on_failure'")),
            "'{policy}' must fail on the failure policy"
        );
    }
}

#[test]
fn plan_rejects_an_executable_outside_the_image() {
    assert_eq!(
        rejected(&plan(&[Component::new("ghost-router")])),
        "boot plan names executable 'ghost-router' that the image does not contain"
    );
}

#[test]
fn plan_rejects_an_executable_named_by_its_image_path() {
    // Реестр объявляет этот путь, и отказ по нему называет миграцию.
    assert_eq!(
        rejected(&plan(&[Component::new(&image_path(ROUTER))])),
        format!(
            "boot plan component names executable '{}' by its image path; \
             name the program identity instead",
            image_path(ROUTER)
        )
    );
}

#[test]
fn two_packages_with_one_bin_target_are_named_by_the_qualified_form() {
    let bytes = image_with_two_helpers().compiled(&plan(&[
        Component::new("foo::helper"),
        Component::new("bar::helper"),
    ]));

    let plan = decoded(&bytes);
    assert_eq!(
        plan.component(0).expect("foo helper").executable(),
        "/boot/foo-helper"
    );
    assert_eq!(
        plan.component(1).expect("bar helper").executable(),
        "/boot/bar-helper"
    );
}

#[test]
fn plan_rejects_a_short_form_that_two_packages_declare() {
    assert_eq!(
        image_with_two_helpers().rejected(&plan(&[Component::new(HELPER)])),
        "boot plan names executable 'helper' that 2 packages of the image declare; \
         name it '<package>::helper' with one of: bar, foo"
    );
}

#[test]
fn a_qualified_form_resolves_when_the_short_form_is_unambiguous() {
    let bytes = compiled(&plan(&[Component::new(&format!("{ROUTER}::{ROUTER}"))]));

    assert_eq!(
        decoded(&bytes).component(0).expect("router").executable(),
        image_path(ROUTER)
    );
}

#[test]
fn a_component_named_by_the_qualified_form_is_called_by_it() {
    let bytes = image_with_two_helpers().compiled(&plan(&[Component::new("foo::helper")]));

    assert_eq!(
        decoded(&bytes).component(0).expect("helper").name(),
        "foo::helper"
    );
}

#[test]
fn a_qualified_name_holds_as_an_image_record_name() {
    let stages = image_with_two_helpers().staged(
        &plan(&[image_filesystem()]),
        &plan(&[Component::new("foo::helper")]),
    );

    assert_eq!(stages.stage_one[0].name, "foo::helper");
    let record = decoded(&stages.stage_one[0].bytes);
    assert_eq!(record.component(0).expect("helper").name(), "foo::helper");
}

#[test]
fn plan_rejects_a_qualified_name_longer_than_a_process_name() {
    let package = "p".repeat(MAX_PROCESS_NAME_LEN);
    let executable = format!("{package}::{HELPER}");
    let image = Image::new().with_package(
        &package,
        HELPER,
        "/boot/long-helper",
        ProgramManifest::default(),
    );

    assert_eq!(
        image.rejected(&plan(&[Component::new(&executable)])),
        format!(
            "boot plan component name '{executable}' is longer than \
             {MAX_PROCESS_NAME_LEN} bytes, which a process name holds"
        )
    );
}

#[test]
fn plan_rejects_a_qualified_form_with_an_empty_half() {
    assert_eq!(
        image_with_two_helpers().rejected(&plan(&[Component::new("::helper")])),
        "boot plan names executable '::helper' whose qualified form must name \
         a package and a bin target as '<package>::<bin>'"
    );
}

#[test]
fn two_forms_of_one_program_give_two_instance_names() {
    let image = Image::new().with_package(
        "foo",
        HELPER,
        "/boot/foo-helper",
        ProgramManifest::default(),
    );

    let bytes = image.compiled(&plan(&[
        Component::new("foo::helper"),
        Component::new(HELPER),
    ]));

    let plan = decoded(&bytes);
    assert_eq!(plan.component(0).expect("qualified").name(), "foo::helper");
    assert_eq!(plan.component(1).expect("short").name(), HELPER);
}

#[test]
fn plan_rejects_two_instances_of_one_identity_without_a_name() {
    assert_eq!(
        rejected(&plan(&[router(), router()])),
        format!("boot plan names component '{ROUTER}' more than once")
    );
}

#[test]
fn a_program_without_a_manifest_declares_nothing() {
    let bytes = compiled(&plan(&[Component::new(ROOTKEEPER)]));

    let plan = decoded(&bytes);
    let component = plan.component(0).expect("component");
    assert_eq!(component.executable(), image_path(ROOTKEEPER));
    assert_eq!(component.slots().count(), 0);
}

#[test]
fn plan_rejects_a_cycle_over_after() {
    assert_eq!(
        rejected(&plan(&[router().after(&[DRIVER]), rtc().after(&[ROUTER])])),
        format!("boot plan dependencies form a cycle over components: {ROUTER}, {DRIVER}")
    );
}

#[test]
fn plan_rejects_a_self_dependency() {
    assert_eq!(
        rejected(&plan(&[router().after(&[ROUTER])])),
        format!("boot plan component '{ROUTER}' names itself in 'after'")
    );
}

#[test]
fn plan_rejects_an_unknown_edge() {
    assert_eq!(
        rejected(&plan(&[router().after(&["logger"])])),
        format!(
            "boot plan component '{ROUTER}' names 'logger' in 'after', \
             which the composition does not contain"
        )
    );
}

#[test]
fn plan_rejects_an_unknown_key() {
    let source = format!("{}slots = [{{ source = \"image\" }}]\n", plan(&[router()]));

    assert!(rejected(&source).starts_with("Failed to parse boot plan TOML"));
}

#[test]
fn plan_rejects_more_slots_than_the_startup_transaction_carries() {
    const CROWDED: &str = "crowded";
    let crowded = declaration(
        (0..=COMPONENT_SLOT_MAX)
            .map(|_| slot(SlotKind::Userland, 0, &[]))
            .collect(),
        None,
    );

    assert_eq!(
        Image::new()
            .with(CROWDED, crowded)
            .rejected(&plan(&[Component::new(CROWDED)])),
        format!(
            "boot plan component 'crowded' declares {} slots; \
             the startup transaction carries {COMPONENT_SLOT_MAX}",
            COMPONENT_SLOT_MAX + 1
        )
    );
}

#[test]
fn plan_allows_two_userland_slot_consumers() {
    const TWIN: &str = "filesystem-imagefs-twin";
    let userland = declaration(vec![slot(SlotKind::Userland, 0, &[])], None);
    let source = plan(&[image_filesystem(), image_filesystem().executable(TWIN)]);

    Image::new().with(TWIN, userland).compiled(&source);
}

#[test]
fn components_fit_the_resource_depth_ceiling() {
    validate_resource_depth(ROUTER, COMPONENT_RESOURCE_LEVELS)
        .expect("a component fits the ceiling");

    let too_deep = validate_resource_depth(ROUTER, MAX_RESOURCE_DEPTH + 1)
        .expect_err("a component past the ceiling must fail")
        .to_string();
    assert_eq!(
        too_deep,
        format!(
            "boot plan component '{ROUTER}' needs {} resource levels under depth 0; \
             the ceiling is {MAX_RESOURCE_DEPTH}",
            MAX_RESOURCE_DEPTH + 1
        )
    );
}

#[test]
fn plan_rejects_a_composition_past_the_supervisor_wait_ceiling() {
    let components = driver_family(WAIT_MANY_MAX_COUNT - 1);

    assert_eq!(
        rejected(&plan(&components)),
        format!(
            "boot plan composition of {WAIT_MANY_MAX_COUNT} components needs \
             {} supervisor wait objects; the ceiling is {WAIT_MANY_MAX_COUNT}",
            WAIT_MANY_MAX_COUNT + SUPERVISOR_WAITS
        )
    );
}

#[test]
fn plan_rejects_a_composition_past_the_handle_estimate() {
    let drivers = (HANDLE_TABLE_CAPACITY as usize - SUPERVISOR_HANDLES) / COMPONENT_HANDLES;
    let components = driver_family(drivers);

    assert!(
        rejected(&plan(&components)).contains("rootkeeper handles; the table holds"),
        "handle estimate must be the failing check"
    );
}
