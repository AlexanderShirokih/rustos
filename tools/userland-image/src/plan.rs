use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::Deserialize;
use syscall::{MAX_RESOURCE_DEPTH, Timeout};
use userland_image::{ComponentSpec, encode_plan};

use crate::host::validate_image_path;

const BOOT_PLAN_MANIFEST_VERSION: u16 = 1;

const BOOT_COMPOSITION_DEPTH: u16 = 0;

const COMPONENT_RESOURCE_LEVELS: u16 = 3;

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
    config: String,
    budget_pages: u64,
    startup_timeout_ms: u64,
    critical: bool,
    restart: u32,
    requires: Option<toml::Value>,
    after: Option<toml::Value>,
}

fn default_plan_version() -> u16 {
    BOOT_PLAN_MANIFEST_VERSION
}

pub(crate) fn compile_boot_plan(
    source: &str,
    programs: &[&str],
    configs: &[&str],
) -> Result<Vec<u8>> {
    let manifest = parse_system_manifest(source)?;
    let component = validate_composition(&manifest, programs, configs)?;
    let spec = ComponentSpec {
        id: &component.id,
        program: &component.program,
        config: &component.config,
        budget_pages: component.budget_pages,
        startup_timeout: Timeout::from_millis(component.startup_timeout_ms),
        restart_limit: component.restart,
        critical: component.critical,
    };
    encode_plan(&spec).map_err(|err| anyhow!("failed to serialize boot plan: {err:?}"))
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

fn validate_composition<'a>(
    manifest: &'a SystemManifest,
    programs: &[&str],
    configs: &[&str],
) -> Result<&'a ComponentEntry> {
    // Компонент проверяется целиком, затем композиция: у каждой проверки своя
    // названная причина отказа.
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

    match manifest.components.as_slice() {
        [component] => Ok(component),
        [] => bail!("boot plan must contain exactly one component; found none"),
        components => bail!(
            "boot plan must contain exactly one component; found {}",
            components.len()
        ),
    }
}

fn validate_component(
    component: &ComponentEntry,
    programs: &[&str],
    configs: &[&str],
) -> Result<()> {
    ensure!(
        !component.id.is_empty(),
        "boot plan component must set a non-empty 'id'"
    );
    for (key, value) in [
        ("requires", &component.requires),
        ("after", &component.after),
    ] {
        ensure!(
            value.is_none(),
            "boot plan component '{}' sets '{key}': the plan does not carry a dependency graph",
            component.id
        );
    }
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
    validate_program(&component.id, &component.program, programs)?;
    validate_config(component, configs)?;
    validate_resource_depth(&component.id, COMPONENT_RESOURCE_LEVELS)
}

fn validate_config(component: &ComponentEntry, configs: &[&str]) -> Result<()> {
    validate_image_path(&component.config).with_context(|| {
        format!(
            "boot plan component '{}' names an invalid config record",
            component.id
        )
    })?;
    ensure!(
        configs.contains(&component.config.as_str()),
        "boot plan component '{}' names config record '{}' \
         that the image does not compile",
        component.id,
        component.config
    );
    Ok(())
}

fn validate_program(id: &str, program: &str, programs: &[&str]) -> Result<()> {
    validate_image_path(program)
        .with_context(|| format!("boot plan component '{id}' names an invalid program"))?;
    ensure!(
        programs.contains(&program),
        "boot plan component '{id}' names program '{program}' \
         that the image does not contain"
    );
    Ok(())
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

#[cfg(test)]
mod tests {
    use userland_image::decode_plan;

    use super::*;

    const ROUTER: &str = "/boot/namespace-router";
    const IMAGE_FS: &str = "/boot/filesystem-imagefs";
    const RAM_FS: &str = "/boot/filesystem-ramfs";

    const PROGRAMS: &[&str] = &["/boot/rootkeeper", ROUTER, IMAGE_FS, RAM_FS];

    const CONFIG: &str = "/boot/namespace.mounts";
    const CONFIGS: &[&str] = &[CONFIG];

    fn component(fields: &str) -> String {
        format!(
            "version = 1\n\
             \n\
             [[components]]\n\
             id = \"filesystem\"\n\
             program = \"{ROUTER}\"\n\
             budget_pages = 2048\n\
             startup_timeout_ms = 2000\n\
             critical = true\n\
             restart = 3\n\
             config = \"{CONFIG}\"\n\
             {fields}"
        )
    }

    fn rejected(source: &str) -> String {
        compile_boot_plan(source, PROGRAMS, CONFIGS)
            .expect_err("invalid composition must fail")
            .to_string()
    }

    #[test]
    fn plan_carries_the_component_and_its_params() {
        let bytes =
            compile_boot_plan(&component(""), PROGRAMS, CONFIGS).expect("compile must succeed");

        let plan = decode_plan(&bytes).expect("compiled plan must decode");
        let component = plan.component();
        assert_eq!(component.id(), "filesystem");
        assert_eq!(component.program(), ROUTER);
        assert_eq!(component.budget_pages(), 2048);
        assert_eq!(component.startup_timeout(), Timeout::from_millis(2000));
        assert_eq!(component.restart_limit(), 3);
        assert!(component.critical());

        assert_eq!(component.config(), CONFIG);
    }

    #[test]
    fn plan_rejects_an_unsupported_version() {
        let source = component("").replace("version = 1", "version = 2");
        assert_eq!(
            rejected(&source),
            "Unsupported boot plan version 2; expected 1"
        );
    }

    #[test]
    fn plan_rejects_a_component_without_an_id() {
        let source = component("").replace("id = \"filesystem\"", "id = \"\"");
        assert_eq!(
            rejected(&source),
            "boot plan component must set a non-empty 'id'"
        );
    }

    #[test]
    fn plan_rejects_a_repeated_id() {
        let mut source = component("");
        source.push_str(&component("").replace("version = 1\n\n", ""));
        assert_eq!(
            rejected(&source),
            "boot plan component id 'filesystem' is used more than once"
        );
    }

    #[test]
    fn plan_rejects_a_composition_of_more_than_one_component() {
        let mut source = component("");
        source.push_str(
            &component("")
                .replace("version = 1\n\n", "")
                .replace("id = \"filesystem\"", "id = \"second\""),
        );
        assert_eq!(
            rejected(&source),
            "boot plan must contain exactly one component; found 2"
        );
    }

    #[test]
    fn plan_rejects_an_empty_composition() {
        assert_eq!(
            rejected("version = 1\ncomponents = []\n"),
            "boot plan must contain exactly one component; found none"
        );
    }

    #[test]
    fn plan_rejects_a_zero_budget() {
        let source = component("").replace("budget_pages = 2048", "budget_pages = 0");
        assert_eq!(
            rejected(&source),
            "boot plan component 'filesystem' must set a non-zero 'budget_pages'"
        );
    }

    #[test]
    fn plan_rejects_a_zero_startup_timeout() {
        let source = component("").replace("startup_timeout_ms = 2000", "startup_timeout_ms = 0");
        assert_eq!(
            rejected(&source),
            "boot plan component 'filesystem' must set a non-zero 'startup_timeout_ms'"
        );
    }

    #[test]
    fn plan_rejects_a_component_that_never_launches() {
        let source = component("").replace("restart = 3", "restart = 0");
        assert_eq!(
            rejected(&source),
            "boot plan component 'filesystem' must set 'restart' to at least one launch"
        );
    }

    #[test]
    fn plan_rejects_a_program_outside_the_image() {
        let source = component("").replace(ROUTER, "/boot/ghost-router");
        assert_eq!(
            rejected(&source),
            "boot plan component 'filesystem' names program '/boot/ghost-router' \
             that the image does not contain"
        );
    }

    #[test]
    fn plan_rejects_a_program_that_is_not_a_program_record() {
        // Компонент поднимается только с записи программы образа.
        let source = component("").replace(ROUTER, "/boot/sample");
        assert_eq!(
            rejected(&source),
            "boot plan component 'filesystem' names program '/boot/sample' \
             that the image does not contain"
        );
    }

    #[test]
    fn plan_rejects_a_config_record_the_image_does_not_compile() {
        let source = component("").replace(CONFIG, "/boot/ghost.mounts");
        assert_eq!(
            rejected(&source),
            "boot plan component 'filesystem' names config record '/boot/ghost.mounts' \
             that the image does not compile"
        );
    }

    #[test]
    fn plan_rejects_a_config_path_outside_the_contract() {
        let source = component("").replace(CONFIG, "boot/namespace.mounts");
        assert!(
            rejected(&source)
                .starts_with("boot plan component 'filesystem' names an invalid config record")
        );
    }

    #[test]
    fn plan_rejects_the_dependency_graph_keys() {
        for key in ["requires", "after"] {
            let source = component(&format!("{key} = [\"logger\"]\n"));
            assert_eq!(
                rejected(&source),
                format!(
                    "boot plan component 'filesystem' sets '{key}': \
                     the plan does not carry a dependency graph"
                )
            );
        }
    }

    #[test]
    fn plan_rejects_an_unknown_key() {
        let source = component("backoff_ms = 100\n");
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
}
