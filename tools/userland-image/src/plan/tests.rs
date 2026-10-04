use bootstrap_abi::image;
use component_abi::{PROTOCOL_ID_MAX, RequirementMode};
use ipc::Protocol as _;
use syscall::{HANDLE_TABLE_CAPACITY, MAX_PROCESS_NAME_LEN};
use userland_image::{BootPlan, decode_plan};

use super::{
    PlanProgram, compile_plan,
    limits::{CHILD_HANDLES, ROOTKEEPER_HANDLES, SEARCH_ROOTS, rootkeeper_handles},
    source::BOOT_PLAN_MANIFEST_VERSION,
};
use crate::manifest::{ManifestRequirement, ProgramExport, ProgramManifest};

const ROUTER: &str = "namespace-router";
const TWIN: &str = "namespace-router-twin";
const DRIVER: &str = "driver-pl031";
const IMAGE_FS: &str = "filesystem-imagefs";
const RAM_FS: &str = "filesystem-ramfs";
const ROOTKEEPER: &str = "rootkeeper";
const CONSUMER: &str = "namespace-consumer-fixture";
const HELPER: &str = "helper";
const PLATFORM: &str = "platform";

const NAMESPACE_PROTOCOL: &str = "Namespace";
const FILE_SYSTEM_MOUNT_PROTOCOL: &str = "FileSystemMount";
const FILE_SYSTEM_PROTOCOL: &str = "FileSystem";
const CLOCK_PROTOCOL: &str = "Clock";
const HARDWARE_PROTOCOL: &str = "Hardware";
const IMAGE_PROTOCOL: &str = image::Protocol::ID;

const DEFAULT_MEMORY_LIMIT: &str = "256K";

struct Component {
    executable: String,
    memory_limit: String,
    restarts: Option<u32>,
    name: Option<String>,
    mountpoint: Option<String>,
}

impl Component {
    fn new(executable: &str) -> Self {
        Self {
            executable: String::from(executable),
            memory_limit: String::from(DEFAULT_MEMORY_LIMIT),
            restarts: None,
            name: None,
            mountpoint: None,
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

    fn mounted(mut self, path: &str) -> Self {
        self.mountpoint = Some(path.into());
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
        let mountpoint = self
            .mountpoint
            .as_ref()
            .map_or_else(String::new, |path| format!("mountpoint = {path:?}\n"));
        format!(
            "\n[[components]]\n\
             executable = \"{}\"\n\
             memory_limit = \"{}\"\n\
             {restarts}{name}{mountpoint}",
            self.executable, self.memory_limit
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

fn requirement(name: &str, mode: RequirementMode, protocol: &str) -> ManifestRequirement {
    ManifestRequirement {
        name: String::from(name),
        mode,
        protocol: String::from(protocol),
    }
}

fn session(name: &str, protocol: &str) -> ManifestRequirement {
    requirement(name, RequirementMode::One, protocol)
}

fn declaration(requirements: Vec<ManifestRequirement>, exports: &[&str]) -> ProgramManifest {
    ProgramManifest {
        requirements,
        exports: exports
            .iter()
            .map(|export| ProgramExport {
                protocol: String::from(*export),
                filters: Vec::new(),
            })
            .collect(),
    }
}

fn router_declaration() -> ProgramManifest {
    declaration(
        vec![session("image", IMAGE_PROTOCOL)],
        &[NAMESPACE_PROTOCOL, FILE_SYSTEM_MOUNT_PROTOCOL],
    )
}

fn consumer_declaration() -> ProgramManifest {
    declaration(vec![session("names", NAMESPACE_PROTOCOL)], &[])
}

fn image_filesystem_declaration() -> ProgramManifest {
    declaration(
        vec![session("image", IMAGE_PROTOCOL)],
        &[FILE_SYSTEM_PROTOCOL],
    )
}

fn ram_filesystem_declaration() -> ProgramManifest {
    declaration(vec![], &[FILE_SYSTEM_PROTOCOL])
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
                manifest: declaration(Vec::new(), &[HARDWARE_PROTOCOL, IMAGE_PROTOCOL]),
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
                data: Vec::leak(entry.manifest.program_bytes()),
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

fn compiled(source: &str) -> Vec<u8> {
    Image::new().compiled(source)
}

fn rejected(source: &str) -> String {
    Image::new().rejected(source)
}

fn decoded(bytes: &[u8]) -> BootPlan<'_> {
    decode_plan(bytes).expect("compiled plan must decode")
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
    assert_eq!(filesystem.mountpoint(), None);
}

#[test]
fn an_unbound_session_is_resolved_by_the_parent_instead_of_the_initial_composition() {
    for components in [
        vec![consumer()],
        vec![router(), consumer()],
        vec![router(), router().name(TWIN), consumer()],
    ] {
        let bytes = compiled(&plan(&components));
        let plan = decoded(&bytes);
        let index = u16::try_from(components.len()).expect("component count");
        assert_eq!(
            plan.component(index).expect("consumer").executable(),
            image_path(CONSUMER)
        );
    }
}

#[test]
fn plan_carries_the_restart_count_of_a_component() {
    let bytes = compiled(&plan(&[router(), rtc()]));

    let plan = decoded(&bytes);
    assert_eq!(plan.component(1).expect("router").restarts(), 2);
    assert_eq!(plan.component(2).expect("rtc").restarts(), 0);
}

#[test]
fn the_rootkeeper_handle_estimate_counts_each_provider_and_grant() {
    let pair = |value: &str| (String::from("compatible"), String::from(value));
    let root = ProgramManifest {
        requirements: Vec::new(),
        exports: vec![ProgramExport {
            protocol: String::from(CLOCK_PROTOCOL),
            filters: vec![vec![pair("a")], vec![pair("b")]],
        }],
    }
    .program_bytes();
    let consumer = declaration(
        vec![
            requirement("clock", RequirementMode::One, CLOCK_PROTOCOL),
            requirement("clocks", RequirementMode::All, CLOCK_PROTOCOL),
            requirement("image", RequirementMode::One, IMAGE_PROTOCOL),
        ],
        &[CLOCK_PROTOCOL],
    )
    .program_bytes();

    let root = component_abi::decode_manifest(userland::ProgramView::manifest(
        &userland::decode_program(&root).unwrap(),
    ))
    .unwrap();
    let consumer = component_abi::decode_manifest(userland::ProgramView::manifest(
        &userland::decode_program(&consumer).unwrap(),
    ))
    .unwrap();

    assert_eq!(rootkeeper_handles(&[root]), ROOTKEEPER_HANDLES);
    assert_eq!(
        rootkeeper_handles(&[root, consumer]),
        ROOTKEEPER_HANDLES + CHILD_HANDLES + 1 + SEARCH_ROOTS + 2 + 1 + 1 + 1 + 1
    );
}

#[test]
fn plan_counts_boot_exports_before_accepting_all_requirements() {
    const IMPORTERS: usize = 10;
    const REQUIREMENTS: usize = 50;

    let importer = || {
        declaration(
            (0..REQUIREMENTS)
                .map(|index| {
                    requirement(
                        &format!("requirement-{index}"),
                        RequirementMode::All,
                        HARDWARE_PROTOCOL,
                    )
                })
                .collect(),
            &[],
        )
    };
    let mut image = Image::new();
    let mut components = Vec::new();
    for index in 0..IMPORTERS {
        let name = format!("importer-{index}");
        image = image.with(&name, importer());
        components.push(Component::new(&name));
    }
    let per_importer = CHILD_HANDLES + REQUIREMENTS * (1 + SEARCH_ROOTS) + 1;
    let handles = ROOTKEEPER_HANDLES + IMPORTERS * per_importer;
    assert!(handles > HANDLE_TABLE_CAPACITY as usize);
    assert!(ROOTKEEPER_HANDLES + (IMPORTERS - 1) * per_importer <= HANDLE_TABLE_CAPACITY as usize);

    image.compiled(&plan(&components[..IMPORTERS - 1]));
    assert_eq!(
        image.rejected(&plan(&components)),
        format!(
            "boot plan composition of {IMPORTERS} components requires up to {handles} rootkeeper handles; \
             the table holds {HANDLE_TABLE_CAPACITY}"
        )
    );
}

#[test]
fn plan_rejects_an_export_past_the_protocol_id_bound() {
    const WIDE: &str = "wide-exporter";
    let protocol = "P".repeat(PROTOCOL_ID_MAX + 1);
    let mut bytes = declaration(Vec::new(), &[&protocol[..PROTOCOL_ID_MAX]]).program_bytes();
    let at = 31 + 40 + 16;
    bytes[at] += 1;
    bytes.insert(at + 1, b'P');
    bytes[27..31].copy_from_slice(
        &(16u32 + 1 + u32::try_from(protocol.len()).expect("protocol length fits") + 1)
            .to_le_bytes(),
    );
    let image = Image::new();
    let mut programs = image.programs();
    programs.push(PlanProgram {
        package: WIDE,
        bin: WIDE,
        path: "/wide",
        boot: false,
        data: &bytes,
    });
    let error = compile_plan(&plan(&[Component::new(WIDE)]), &programs).expect_err("wide export");
    assert_eq!(
        reason(&error),
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
    assert_eq!(first.mountpoint(), second.mountpoint());
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
    assert_eq!(component.mountpoint(), None);
}

#[test]
fn record_zero_preserves_boot_program_and_launch_parameters() {
    let bytes = compiled(&plan(&[router()]));

    let boot = decoded(&bytes).component(0).expect("record 0");
    assert_eq!(boot.executable(), image_path(ROOTKEEPER));
    assert_eq!(boot.name(), ROOTKEEPER);

    assert_eq!(boot.mountpoint(), None);
    assert_eq!(boot.memory_limit(), 0);
    assert_eq!(boot.restarts(), 0);
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

    let bytes = image.compiled(&plan(&[platform()]));

    assert_eq!(
        decoded(&bytes).component(0).expect("record 0").name(),
        "init"
    );
    assert_eq!(
        decoded(&bytes).component(1).expect("platform").mountpoint(),
        None
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

#[test]
fn all_preserves_runtime_selection_instead_of_serializing_initial_exporters() {
    let image = Image::new().with(
        "multiplicity-consumer",
        declaration(
            vec![requirement(
                "names",
                RequirementMode::All,
                NAMESPACE_PROTOCOL,
            )],
            &[NAMESPACE_PROTOCOL],
        ),
    );
    let bytes = image.compiled(&plan(&[
        router(),
        Component::new("multiplicity-consumer"),
        router().name(TWIN),
    ]));
    assert_eq!(
        decoded(&bytes).component(2).expect("consumer").mountpoint(),
        None
    );
}

#[test]
fn session_and_all_preserve_their_selection_without_initial_exporters() {
    for mode in [RequirementMode::One, RequirementMode::All] {
        let image = Image::new().with(
            "multiplicity-consumer",
            declaration(vec![requirement("names", mode, NAMESPACE_PROTOCOL)], &[]),
        );
        let bytes = image.compiled(&plan(&[Component::new("multiplicity-consumer")]));
        assert_eq!(
            decoded(&bytes).component(1).expect("consumer").mountpoint(),
            None
        );
    }
}

#[test]
fn plan_carries_explicit_mount_paths_and_refuses_non_filesystem_mounts() {
    let bytes = compiled(&plan(&[router(), ram_filesystem().mounted("/tmp")]));
    assert_eq!(
        decoded(&bytes).component(2).expect("ramfs").mountpoint(),
        Some("/tmp")
    );
    assert!(rejected(&plan(&[router().mounted("/tmp")])).contains("must export FileSystem"));
    for path in ["", "relative", "/../outside"] {
        assert!(rejected(&plan(&[ram_filesystem().mounted(path)])).contains("mountpoint"));
    }
}

#[test]
fn plan_rejects_the_removed_generic_scope_configuration() {
    let source = format!(
        "{}scope = {{ namespace = \"/boot\" }}\n",
        plan(&[image_filesystem()])
    );
    assert!(rejected(&source).starts_with("Failed to parse boot plan TOML"));
}
