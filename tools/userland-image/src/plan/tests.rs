use component_abi::{PROTOCOL_ID_MAX, RequirementKind, SESSION_SCOPE_MAX};
use syscall::{HANDLE_TABLE_CAPACITY, MAX_PROCESS_NAME_LEN};
use userland_image::{BootPlan, PlanRequirement, RequirementSource, decode_plan};

use super::{
    PlanProgram, compile_plan,
    limits::{CHILD_HANDLES, ROOTKEEPER_HANDLES, rootkeeper_handles},
    source::BOOT_PLAN_MANIFEST_VERSION,
};
use crate::manifest::{ManifestRequirement, ProgramManifest};

const ROUTER: &str = "namespace-router";
const TWIN: &str = "namespace-router-twin";
const DRIVER: &str = "driver-pl031";
const IMAGE_FS: &str = "filesystem-imagefs";
const RAM_FS: &str = "filesystem-ramfs";
const ROOTKEEPER: &str = "rootkeeper";
const CONSUMER: &str = "namespace-consumer-fixture";
const AREA: &str = "area-consumer";
const AREA_REQUIREMENT: &str = "area";
const HELPER: &str = "helper";
const PLATFORM: &str = "platform";

const NAMESPACE_PROTOCOL: &str = "Namespace";
const NAMESPACE_ADMIN_PROTOCOL: &str = "NamespaceAdmin";
const FILE_SYSTEM_PROTOCOL: &str = "FileSystem";
const CLOCK_PROTOCOL: &str = "Clock";
const HARDWARE_PROTOCOL: &str = "Hardware";

const DEFAULT_MEMORY_LIMIT: &str = "256K";

struct Component {
    executable: String,
    memory_limit: String,
    restarts: Option<u32>,
    name: Option<String>,
    bind: Vec<(String, String)>,
    scope: Vec<(String, String)>,
}

impl Component {
    fn new(executable: &str) -> Self {
        Self {
            executable: String::from(executable),
            memory_limit: String::from(DEFAULT_MEMORY_LIMIT),
            restarts: None,
            name: None,
            bind: Vec::new(),
            scope: Vec::new(),
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

    fn restarts(mut self, restarts: u32) -> Self {
        self.restarts = Some(restarts);
        self
    }

    fn name(mut self, name: &str) -> Self {
        self.name = Some(String::from(name));
        self
    }

    fn bind(mut self, requirement: &str, instance: &str) -> Self {
        self.bind
            .push((String::from(requirement), String::from(instance)));
        self
    }

    fn scope(mut self, requirement: &str, scope: &str) -> Self {
        self.scope
            .push((String::from(requirement), String::from(scope)));
        self
    }

    fn render(&self) -> String {
        let restarts = match self.restarts {
            Some(restarts) => format!("restarts = {restarts}\n"),
            None => String::new(),
        };
        let name = match &self.name {
            Some(name) => format!("name = \"{name}\"\n"),
            None => String::new(),
        };
        let bind = table(
            "bind",
            self.bind
                .iter()
                .map(|(requirement, to)| (requirement, format!("\"{to}\""))),
        );
        let scope = table(
            "scope",
            self.scope
                .iter()
                .map(|(requirement, area)| (requirement, format!("\"{area}\""))),
        );
        format!(
            "\n[[components]]\n\
             executable = \"{}\"\n\
             memory_limit = \"{}\"\n\
             {restarts}{name}{bind}{scope}",
            self.executable, self.memory_limit
        )
    }
}

fn table<'a>(key: &str, entries: impl Iterator<Item = (&'a String, String)>) -> String {
    let entries: Vec<String> = entries
        .map(|(requirement, value)| format!("{requirement} = {value}"))
        .collect();
    if entries.is_empty() {
        return String::new();
    }
    format!("{key} = {{ {} }}\n", entries.join(", "))
}

fn versioned(version: u16, components: &[Component]) -> String {
    let records: String = components.iter().map(Component::render).collect();
    format!("version = {version}\n{records}")
}

fn plan(components: &[Component]) -> String {
    versioned(BOOT_PLAN_MANIFEST_VERSION, components)
}

fn router() -> Component {
    Component::new(ROUTER).memory_limit("8M").restarts(2)
}

fn image_filesystem() -> Component {
    Component::new(IMAGE_FS).memory_limit("8M")
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

fn platform() -> Component {
    Component::new(PLATFORM)
}

fn requirement(name: &str, kind: RequirementKind, protocol: Option<&str>) -> ManifestRequirement {
    ManifestRequirement {
        name: String::from(name),
        kind,
        protocol: protocol.map(String::from),
    }
}

fn session(name: &str, protocol: &str) -> ManifestRequirement {
    requirement(name, RequirementKind::Session, Some(protocol))
}

fn region(name: &str) -> ManifestRequirement {
    requirement(name, RequirementKind::Region, None)
}

fn declaration(requirements: Vec<ManifestRequirement>, exports: &[&str]) -> ProgramManifest {
    ProgramManifest {
        requirements,
        exports: exports.iter().map(|export| String::from(*export)).collect(),
    }
}

fn router_declaration() -> ProgramManifest {
    declaration(
        vec![region("image")],
        &[NAMESPACE_PROTOCOL, NAMESPACE_ADMIN_PROTOCOL],
    )
}

fn consumer_declaration() -> ProgramManifest {
    declaration(vec![session("names", NAMESPACE_PROTOCOL)], &[])
}

fn image_filesystem_declaration() -> ProgramManifest {
    declaration(vec![region("image")], &[FILE_SYSTEM_PROTOCOL])
}

fn ram_filesystem_declaration() -> ProgramManifest {
    declaration(
        vec![session("namespace", NAMESPACE_ADMIN_PROTOCOL)],
        &[FILE_SYSTEM_PROTOCOL],
    )
}

fn area_declaration() -> ProgramManifest {
    declaration(vec![session(AREA_REQUIREMENT, FILE_SYSTEM_PROTOCOL)], &[])
}

struct ImageEntry {
    package: String,
    bin: String,
    path: String,
    boot: bool,
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
            entries: vec![ImageEntry {
                package: String::from(ROOTKEEPER),
                bin: String::from(ROOTKEEPER),
                path: image_path(ROOTKEEPER),
                boot: true,
                manifest: declaration(Vec::new(), &[HARDWARE_PROTOCOL]),
            }],
        };
        for (bin, manifest) in [
            (ROUTER, router_declaration()),
            (DRIVER, declaration(Vec::new(), &[])),
            (CONSUMER, consumer_declaration()),
            (IMAGE_FS, image_filesystem_declaration()),
            (RAM_FS, ram_filesystem_declaration()),
        ] {
            image = image.with(bin, manifest);
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
            boot: false,
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
                boot: entry.boot,
                manifest: &entry.manifest,
            })
            .collect()
    }

    fn compiled(&self, source: &str) -> Vec<u8> {
        compile_plan(source, &self.programs()).expect("compile must succeed")
    }

    fn rejected(&self, source: &str) -> String {
        let error =
            compile_plan(source, &self.programs()).expect_err("invalid composition must fail");
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

fn image_with_platform() -> Image {
    Image::new().with(
        PLATFORM,
        declaration(vec![session("hardware", HARDWARE_PROTOCOL)], &[]),
    )
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

fn requirements_of(bytes: &[u8], index: u16) -> Vec<PlanRequirement<'_>> {
    decoded(bytes)
        .component(index)
        .expect("the plan carries the component")
        .requirements()
        .collect()
}

fn session_of<'a>(
    name: &'a str,
    provider: u16,
    protocol: &'a str,
    scope: &'a str,
) -> PlanRequirement<'a> {
    PlanRequirement {
        name,
        source: RequirementSource::Export {
            provider,
            protocol,
            scope,
        },
    }
}

fn reason(error: &anyhow::Error) -> String {
    // Причина читается вместе со своей цепочкой: значение и компонент,
    // который его назвал, лежат на разных её звеньях.
    format!("{error:#}")
}

#[test]
fn plan_takes_every_requirement_from_the_program_declaration() {
    let bytes = compiled(&plan(&[image_filesystem(), rtc()]));

    let plan = decoded(&bytes);
    assert_eq!(plan.len(), 3);

    let filesystem = plan.component(1).expect("image filesystem");
    assert_eq!(filesystem.executable(), image_path(IMAGE_FS));
    assert_eq!(
        filesystem.requirements().collect::<Vec<_>>(),
        [PlanRequirement {
            name: "image",
            source: RequirementSource::Region
        }]
    );
}

#[test]
fn plan_binds_an_imported_protocol_to_its_exporter_in_either_order() {
    for (components, importer, exporter) in [
        ([router(), consumer()], 2, 1),
        ([consumer(), router()], 1, 2),
    ] {
        let bytes = compiled(&plan(&components));

        assert_eq!(
            requirements_of(&bytes, importer),
            [session_of("names", exporter, NAMESPACE_PROTOCOL, "")]
        );
    }
}

#[test]
fn two_instances_of_one_program_bind_one_requirement_to_two_providers() {
    let bytes = compiled(&plan(&[
        router().name("names-a"),
        router().name("names-b"),
        consumer().name("consumer-a").bind("names", "names-a"),
        consumer().name("consumer-b").bind("names", "names-b"),
    ]));

    assert_eq!(
        requirements_of(&bytes, 3),
        [session_of("names", 1, NAMESPACE_PROTOCOL, "")]
    );
    assert_eq!(
        requirements_of(&bytes, 4),
        [session_of("names", 2, NAMESPACE_PROTOCOL, "")]
    );
}

#[test]
fn plan_rejects_an_import_no_component_exports() {
    assert_eq!(
        rejected(&plan(&[rtc(), consumer()])),
        format!(
            "boot plan component '{CONSUMER}' imports protocol '{NAMESPACE_PROTOCOL}' \
             for requirement 'names', which no component of the composition exports"
        )
    );
}

#[test]
fn plan_rejects_an_import_two_components_export() {
    assert_eq!(
        Image::new()
            .with(TWIN, router_declaration())
            .rejected(&plan(&[router(), router().executable(TWIN), consumer()])),
        format!(
            "boot plan component '{CONSUMER}' imports protocol '{NAMESPACE_PROTOCOL}' \
             for requirement 'names', which 2 components export: {ROUTER}, {TWIN}; \
             name one with 'bind'"
        )
    );
}

#[test]
fn plan_rejects_a_key_on_a_name_outside_the_manifest() {
    for (key, component) in [
        ("bind", consumer().bind("files", ROUTER)),
        ("scope", consumer().scope("files", "/")),
    ] {
        assert_eq!(
            rejected(&plan(&[router(), component])),
            format!(
                "boot plan component '{CONSUMER}' sets '{key}' on requirement 'files', \
                 which its manifest does not declare"
            )
        );
    }
}

#[test]
fn plan_rejects_a_session_key_on_a_requirement_that_takes_no_session() {
    for (key, component) in [
        ("bind", image_filesystem().bind("image", ROUTER)),
        ("scope", image_filesystem().scope("image", "/")),
    ] {
        assert_eq!(
            rejected(&plan(&[router(), component])),
            format!(
                "boot plan component '{IMAGE_FS}' sets '{key}' on requirement 'image', \
                 which takes no session"
            )
        );
    }
}

#[test]
fn plan_rejects_a_bind_to_an_instance_outside_the_composition() {
    assert_eq!(
        rejected(&plan(&[router(), consumer().bind("names", "ghost")])),
        format!(
            "boot plan component '{CONSUMER}' binds requirement 'names' to instance 'ghost', \
             which the composition does not contain"
        )
    );
}

#[test]
fn plan_rejects_a_bind_to_an_instance_that_does_not_export_the_protocol() {
    assert_eq!(
        rejected(&plan(&[router(), rtc(), consumer().bind("names", DRIVER)])),
        format!(
            "boot plan component '{CONSUMER}' binds requirement 'names' to instance '{DRIVER}', \
             which exports no protocol '{NAMESPACE_PROTOCOL}'"
        )
    );
}

#[test]
fn plan_rejects_a_component_importing_what_it_exports() {
    const SELF_BOUND: &str = "self-bound";
    let self_bound = declaration(
        vec![session("names", NAMESPACE_PROTOCOL)],
        &[NAMESPACE_PROTOCOL],
    );

    assert_eq!(
        Image::new()
            .with(SELF_BOUND, self_bound)
            .rejected(&plan(&[Component::new(SELF_BOUND)])),
        "boot plan component 'self-bound' imports the protocol it exports itself"
    );
}

#[test]
fn plan_rejects_a_bind_to_the_component_itself() {
    const SELF_BOUND: &str = "self-bound";
    let self_bound = declaration(
        vec![session("names", NAMESPACE_PROTOCOL)],
        &[NAMESPACE_PROTOCOL],
    );

    assert_eq!(
        Image::new().with(SELF_BOUND, self_bound).rejected(&plan(&[
            router(),
            Component::new(SELF_BOUND).bind("names", SELF_BOUND),
        ])),
        "boot plan component 'self-bound' binds requirement 'names' to itself"
    );
}

#[test]
fn a_plan_binding_the_log_requirement_is_refused() {
    assert_eq!(
        rejected(&plan(&[router(), consumer().bind("log", ROUTER)])),
        format!(
            "boot plan component '{CONSUMER}' sets 'bind' on requirement 'log', \
             the name reserved for the log its parent serves"
        )
    );
}

#[test]
fn plan_carries_the_mount_prefix_as_the_scope_of_the_namespace_requirement() {
    let bytes = compiled(&plan(&[
        router(),
        ram_filesystem().scope("namespace", "/tmp"),
    ]));

    assert_eq!(
        requirements_of(&bytes, 2),
        [session_of("namespace", 1, NAMESPACE_ADMIN_PROTOCOL, "/tmp")]
    );
}

#[test]
fn plan_rejects_a_scope_past_its_bound() {
    let wide = format!("/{}", "a".repeat(SESSION_SCOPE_MAX));

    assert_eq!(
        Image::new()
            .with(AREA, area_declaration())
            .rejected(&plan(&[
                image_filesystem(),
                Component::new(AREA).scope(AREA_REQUIREMENT, &wide),
            ])),
        format!(
            "boot plan component '{AREA}' sets 'scope' on requirement '{AREA_REQUIREMENT}' with {} bytes; \
             a scope is non-empty and at most {SESSION_SCOPE_MAX}",
            wide.len()
        )
    );
}

#[test]
fn plan_carries_the_restart_count_of_a_component() {
    let bytes = compiled(&plan(&[router(), rtc()]));

    let plan = decoded(&bytes);
    assert_eq!(plan.component(1).expect("router").restarts(), 2);
    assert_eq!(plan.component(2).expect("rtc").restarts(), 0);
}

#[test]
fn the_rootkeeper_handle_estimate_counts_session_requirements_and_exports() {
    let provider = declaration(Vec::new(), &[CLOCK_PROTOCOL]);
    let two = declaration(
        vec![
            session("first", CLOCK_PROTOCOL),
            session("second", CLOCK_PROTOCOL),
        ],
        &[],
    );
    let one = declaration(vec![session("only", CLOCK_PROTOCOL)], &[]);

    assert_eq!(
        rootkeeper_handles(&[&provider, &two, &one]),
        ROOTKEEPER_HANDLES + 3 * CHILD_HANDLES + 3 + 1 + 3
    );
}

#[test]
fn the_rootkeeper_handle_estimate_counts_the_grants_a_component_holds() {
    assert_eq!(
        rootkeeper_handles(&[&image_filesystem_declaration()]),
        ROOTKEEPER_HANDLES + CHILD_HANDLES + 1 + 2
    );
}

#[test]
fn plan_rejects_a_composition_past_the_rootkeeper_handle_estimate() {
    const CLOCK: &str = "clock-source";
    const IMPORTERS: usize = 10;
    const REQUIREMENTS: usize = 100;

    let importer = || {
        declaration(
            (0..REQUIREMENTS)
                .map(|index| session(&format!("requirement-{index}"), CLOCK_PROTOCOL))
                .collect(),
            &[],
        )
    };
    let mut image = Image::new().with(CLOCK, declaration(Vec::new(), &[CLOCK_PROTOCOL]));
    let mut components = vec![Component::new(CLOCK)];
    for index in 0..IMPORTERS {
        let name = format!("importer-{index}");
        image = image.with(&name, importer());
        components.push(Component::new(&name));
    }
    let declarations: Vec<ProgramManifest> = (0..IMPORTERS)
        .map(|_| importer())
        .chain([declaration(Vec::new(), &[CLOCK_PROTOCOL])])
        .collect();
    let handles = rootkeeper_handles(&declarations.iter().collect::<Vec<_>>());
    assert!(handles > HANDLE_TABLE_CAPACITY as usize);

    assert_eq!(
        image.rejected(&plan(&components)),
        format!(
            "boot plan composition of {} components requires up to {handles} rootkeeper handles; \
             the table holds {HANDLE_TABLE_CAPACITY}",
            IMPORTERS + 1
        )
    );
}

#[test]
fn plan_rejects_an_export_past_the_protocol_id_bound() {
    const WIDE: &str = "wide-exporter";
    let protocol = "P".repeat(PROTOCOL_ID_MAX + 1);

    assert_eq!(
        Image::new()
            .with(WIDE, declaration(Vec::new(), &[&protocol]))
            .rejected(&plan(&[Component::new(WIDE)])),
        format!(
            "boot plan component '{WIDE}' exports protocol '{protocol}' of {} bytes; \
             a protocol id holds at most {PROTOCOL_ID_MAX}",
            protocol.len()
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
    let router = plan.component(1).expect("router");
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
    let router = plan.component(1).expect("router");
    // Запись плана хранит размещение, а имя экземпляра идёт от идентичности.
    // Перемещение программы описание композиции не трогает.
    assert_eq!(router.executable(), ELSEWHERE);
    assert_eq!(router.name(), MOVED);
}

#[test]
fn a_named_component_keeps_its_name_apart_from_the_program() {
    let bytes = compiled(&plan(&[router().name("names")]));

    let plan = decoded(&bytes);
    let router = plan.component(1).expect("router");
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
    assert_eq!(plan.len(), 3);
    let first = plan.component(1).expect("first instance");
    let second = plan.component(2).expect("second instance");
    assert_eq!(first.executable(), second.executable());
    assert_eq!(first.name(), "names-boot");
    assert_eq!(second.name(), "names-spare");
    assert_eq!(
        first.requirements().collect::<Vec<_>>(),
        second.requirements().collect::<Vec<_>>()
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
    for limit in ["0", "8MB", ""] {
        assert!(
            rejected(&plan(&[router().memory_limit(limit)])).starts_with(&format!(
                "boot plan component '{ROUTER}' sets 'memory_limit'"
            )),
            "'{limit}' must fail on the memory limit"
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
        plan.component(1).expect("foo helper").executable(),
        "/boot/foo-helper"
    );
    assert_eq!(
        plan.component(2).expect("bar helper").executable(),
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
        decoded(&bytes).component(1).expect("router").executable(),
        image_path(ROUTER)
    );
}

#[test]
fn a_component_named_by_the_qualified_form_is_called_by_it() {
    let bytes = image_with_two_helpers().compiled(&plan(&[Component::new("foo::helper")]));

    assert_eq!(
        decoded(&bytes).component(1).expect("helper").name(),
        "foo::helper"
    );
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
    assert_eq!(plan.component(1).expect("qualified").name(), "foo::helper");
    assert_eq!(plan.component(2).expect("short").name(), HELPER);
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
    let bytes = Image::new()
        .with(HELPER, ProgramManifest::default())
        .compiled(&plan(&[Component::new(HELPER)]));

    let plan = decoded(&bytes);
    let component = plan.component(1).expect("component");
    assert_eq!(component.executable(), image_path(HELPER));
    assert_eq!(component.requirements().count(), 0);
    assert_eq!(component.exports().count(), 0);
}

#[test]
fn record_zero_is_the_boot_program_with_the_exports_of_its_manifest() {
    let bytes = compiled(&plan(&[router()]));

    let boot = decoded(&bytes).component(0).expect("record 0");
    assert_eq!(boot.executable(), image_path(ROOTKEEPER));
    assert_eq!(boot.name(), ROOTKEEPER);
    assert_eq!(boot.exports().collect::<Vec<_>>(), [HARDWARE_PROTOCOL]);
    assert_eq!(boot.requirements().count(), 0);
    assert_eq!(boot.memory_limit(), 0);
    assert_eq!(boot.restarts(), 0);
}

#[test]
fn a_requirement_to_the_export_of_the_boot_program_binds_to_record_zero() {
    for component in [platform(), platform().bind("hardware", ROOTKEEPER)] {
        let bytes = image_with_platform().compiled(&plan(&[component]));

        assert_eq!(
            requirements_of(&bytes, 1),
            [session_of("hardware", 0, HARDWARE_PROTOCOL, "")]
        );
    }
}

#[test]
fn plan_rejects_a_scope_on_a_requirement_to_record_zero() {
    assert_eq!(
        image_with_platform().rejected(&plan(&[platform().scope("hardware", "all")])),
        format!(
            "boot plan component '{PLATFORM}' sets 'scope' on requirement 'hardware', \
             which the boot program '{ROOTKEEPER}' provides"
        )
    );
}

#[test]
fn plan_rejects_a_requirement_in_the_manifest_of_the_boot_program() {
    let image = Image {
        entries: vec![ImageEntry {
            package: String::from(ROOTKEEPER),
            bin: String::from(ROOTKEEPER),
            path: image_path(ROOTKEEPER),
            boot: true,
            manifest: consumer_declaration(),
        }],
    }
    .with(ROUTER, router_declaration());

    assert_eq!(
        image.rejected(&plan(&[router()])),
        format!(
            "boot program '{ROOTKEEPER}' declares requirement 'names'; \
             a program without a parent takes none"
        )
    );
}

#[test]
fn record_zero_is_named_by_the_image_path_of_the_boot_program() {
    let image = Image {
        entries: vec![ImageEntry {
            package: String::from(ROOTKEEPER),
            bin: String::from(ROOTKEEPER),
            path: String::from("/boot/init"),
            boot: true,
            manifest: declaration(Vec::new(), &[HARDWARE_PROTOCOL]),
        }],
    }
    .with(
        PLATFORM,
        declaration(vec![session("hardware", HARDWARE_PROTOCOL)], &[]),
    );

    let bytes = image.compiled(&plan(&[platform().bind("hardware", "init")]));

    assert_eq!(
        decoded(&bytes).component(0).expect("record 0").name(),
        "init"
    );
    assert_eq!(
        requirements_of(&bytes, 1),
        [session_of("hardware", 0, HARDWARE_PROTOCOL, "")]
    );
}

#[test]
fn plan_rejects_a_component_named_as_record_zero() {
    assert_eq!(
        rejected(&plan(&[router().name(ROOTKEEPER)])),
        format!("boot plan names component '{ROOTKEEPER}' more than once")
    );
}

#[test]
fn plan_rejects_an_unknown_key() {
    let source = format!("{}slots = [{{ source = \"image\" }}]\n", plan(&[router()]));

    assert!(rejected(&source).starts_with("Failed to parse boot plan TOML"));
}
