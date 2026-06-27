mod layers;

use std::{
    collections::BTreeSet,
    env,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use flate2::{Compression, write::GzEncoder};
use serde::Deserialize;
use userland_image_tool::build_userland;

#[derive(Parser)]
#[command(name = "xtask")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Собрать ядро для устройства
    Build {
        /// Путь к YAML-спеке устройства
        spec: PathBuf,
        /// Запустить после сборки (команды из 'run' в YAML)
        #[arg(long)]
        run: bool,
        /// Отладка после сборки (команды из 'debug' в YAML)
        #[arg(long)]
        debug: bool,
        /// Cargo features `hal-aarch64` (через запятую).
        #[arg(long)]
        features: Option<String>,
    },
    /// Запустить QEMU integration tests
    QemuTest {
        /// Таймаут в секундах
        #[arg(long, default_value_t = 60)]
        timeout: u64,
    },
    /// Собрать userland.img из композиции user/images/<имя>.toml
    BuildUserland {
        /// Имя образа; резолвится в user/images/<имя>.toml
        #[arg(long, default_value = "default")]
        image: String,
    },
    /// Проверить правила слоев между workspace-крейтами
    CheckLayers,
}

#[derive(Debug, Deserialize)]
struct DeviceSpec {
    boot: BootSection,
    #[serde(default)]
    features: Vec<String>,
    #[serde(default)]
    run: Vec<String>,
    #[serde(default)]
    debug: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct BootSection {
    format: String,
    #[allow(dead_code)]
    offset: u64,
    dtb: Option<String>,
    base: Option<u64>,
}

struct BuildContext {
    project_root: PathBuf,
    build_dir: PathBuf,
    spec_path: PathBuf,
    spec: DeviceSpec,
    features: Option<String>,
}

impl BuildContext {
    fn new(spec_path: PathBuf, features: Option<String>) -> Result<Self> {
        let project_root = project_root();

        let spec_path = if spec_path.is_absolute() {
            spec_path
        } else {
            project_root.join(&spec_path)
        };

        if !spec_path.exists() {
            bail!("Device spec '{}' not found", spec_path.display());
        }

        let spec = load_spec(&spec_path)?;

        let build_dir = project_root.join("target/build");
        fs::create_dir_all(&build_dir)?;

        Ok(Self {
            project_root,
            build_dir,
            spec_path,
            spec,
            features,
        })
    }

    fn kernel_bin(&self) -> PathBuf {
        self.build_dir.join("kernel.bin")
    }

    fn userland_img(&self) -> PathBuf {
        self.build_dir.join("userland.img")
    }

    fn kernel_gz(&self) -> PathBuf {
        self.build_dir.join("kernel.gz")
    }

    fn kernel_gz_dtb(&self) -> PathBuf {
        self.build_dir.join("kernel.gz+dtb")
    }

    fn boot_img(&self) -> PathBuf {
        self.build_dir.join("boot.img")
    }

    fn dtb_path(&self) -> Result<PathBuf> {
        let dtb = self
            .spec
            .boot
            .dtb
            .as_ref()
            .context("boot.dtb is required for android boot format")?;

        let path = self.project_root.join(dtb.trim_start_matches('/'));
        if !path.exists() {
            bail!("DTB not found at {}", path.display());
        }
        Ok(path)
    }
}

fn project_root() -> PathBuf {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let path = Path::new(&manifest_dir);

    if path.ends_with("xtask") {
        path.parent().unwrap().to_path_buf()
    } else {
        path.to_path_buf()
    }
}

fn image_manifest_path(project_root: &Path, image: &str) -> Result<PathBuf> {
    let path = project_root
        .join("user/images")
        .join(format!("{image}.toml"));
    if !path.exists() {
        bail!("Userland image '{image}' not found at {}", path.display());
    }
    Ok(path)
}

fn load_spec(path: &Path) -> Result<DeviceSpec> {
    let file = File::open(path).context("Failed to open spec file")?;
    let spec: DeviceSpec = serde_yaml::from_reader(file).context("Failed to parse YAML")?;

    match spec.boot.format.as_str() {
        "linux_arm64" | "android_boot_v1" | "android_boot_v2" | "uefi" => {}
        other => bail!(
            "Unknown boot.format '{other}'. Expected: linux_arm64, android_boot_v1, android_boot_v2, uefi"
        ),
    }

    Ok(spec)
}

fn protocol_feature(boot_format: &str) -> &'static str {
    match boot_format {
        "linux_arm64" | "android_boot_v1" | "android_boot_v2" => "boot-linux-arm64",
        "uefi" => "boot-uefi",
        other => {
            unreachable!("invalid boot.format '{other}' should have been rejected by load_spec")
        }
    }
}

fn build_features(ctx: &BuildContext) -> String {
    let mut features = vec![protocol_feature(ctx.spec.boot.format.as_str()).to_string()];
    features.extend(ctx.spec.features.iter().cloned());
    if let Some(extra) = ctx.features.as_deref()
        && !extra.is_empty()
    {
        features.push(extra.to_string());
    }
    features.join(",")
}

fn run_cmd(cmd: &mut Command) -> Result<ExitStatus> {
    let status = cmd.status().context("Failed to execute command")?;
    if !status.success() {
        bail!("Command failed with {status}");
    }
    Ok(status)
}

fn run_shell(command: &str, cwd: &Path) -> Result<()> {
    let mut child = Command::new("sh")
        .args(["-c", command])
        .current_dir(cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .context("Failed to execute shell command")?;

    let status = child.wait().context("Failed to wait for command")?;

    if !status.success() {
        bail!("Command failed: {command}");
    }
    Ok(())
}

fn cargo_build(ctx: &BuildContext) -> Result<()> {
    let features = build_features(ctx);
    let mut cmd = Command::new("cargo");
    cmd.args([
        "build",
        "-p",
        "hal-aarch64",
        "--target",
        "aarch64-unknown-none",
        "--release",
        "--no-default-features",
        "--features",
        &features,
    ]);
    cmd.current_dir(&ctx.project_root)
        .env("DEVICE_SPEC", &ctx.spec_path)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    run_cmd(&mut cmd)?;
    Ok(())
}

fn make_kernel_bin(ctx: &BuildContext) -> Result<()> {
    let features = build_features(ctx);
    let mut cmd = Command::new("cargo");
    cmd.args([
        "objcopy",
        "--release",
        "-p",
        "hal-aarch64",
        "--target",
        "aarch64-unknown-none",
        "--no-default-features",
        "--features",
        &features,
    ]);
    cmd.args([
        "--",
        "--set-section-flags",
        ".bss=alloc,load,data",
        "-O",
        "binary",
    ])
    .arg(ctx.kernel_bin())
    .current_dir(&ctx.project_root)
    .env("DEVICE_SPEC", &ctx.spec_path)
    .stdout(Stdio::inherit())
    .stderr(Stdio::inherit());
    run_cmd(&mut cmd)?;
    Ok(())
}

fn make_kernel_gz(ctx: &BuildContext) -> Result<()> {
    let mut input = Vec::new();
    File::open(ctx.kernel_bin())?.read_to_end(&mut input)?;

    let output = File::create(ctx.kernel_gz())?;
    let mut encoder = GzEncoder::new(output, Compression::best());
    encoder.write_all(&input)?;
    encoder.finish()?;

    Ok(())
}

fn make_boot_img_v1(ctx: &BuildContext) -> Result<()> {
    let dtb_path = ctx.dtb_path()?;

    // Вычисление kernel_offset для boot.img header
    let base = ctx.spec.boot.base.unwrap_or(0);
    let kernel_offset = ctx.spec.boot.offset.saturating_sub(base);
    let kernel_offset_str = format!("{kernel_offset:#x}");

    let mut output = File::create(ctx.kernel_gz_dtb())?;
    let mut kernel = Vec::new();
    let mut dtb = Vec::new();
    File::open(ctx.kernel_gz())?.read_to_end(&mut kernel)?;
    File::open(&dtb_path)?.read_to_end(&mut dtb)?;
    output.write_all(&kernel)?;
    output.write_all(&dtb)?;

    run_cmd(
        Command::new("mkbootimg")
            .args([
                "--kernel",
                ctx.kernel_gz_dtb().to_str().unwrap(),
                "--ramdisk",
                ctx.userland_img().to_str().unwrap(),
                "--base",
                "0x0",
                "--kernel_offset",
                &kernel_offset_str,
                "--ramdisk_offset",
                "0x01000000",
                "--pagesize",
                "4096",
                "--header_version",
                "1",
                "--output",
                ctx.boot_img().to_str().unwrap(),
            ])
            .current_dir(&ctx.project_root),
    )?;

    Ok(())
}

fn make_boot_img_v2(ctx: &BuildContext) -> Result<()> {
    let dtb_path = ctx.dtb_path()?;

    // Вычисление kernel_offset для boot.img header
    let base = ctx.spec.boot.base.unwrap_or(0);
    let kernel_offset = ctx.spec.boot.offset.saturating_sub(base);
    let kernel_offset_str = format!("{kernel_offset:#x}");

    run_cmd(
        Command::new("mkbootimg")
            .args([
                "--kernel",
                ctx.kernel_gz().to_str().unwrap(),
                "--ramdisk",
                ctx.userland_img().to_str().unwrap(),
                "--dtb",
                dtb_path.to_str().unwrap(),
                "--base",
                "0x0",
                "--kernel_offset",
                &kernel_offset_str,
                "--ramdisk_offset",
                "0x01000000",
                "--pagesize",
                "4096",
                "--header_version",
                "2",
                "--output",
                ctx.boot_img().to_str().unwrap(),
            ])
            .current_dir(&ctx.project_root),
    )?;

    Ok(())
}

fn build_binary(ctx: &BuildContext) -> Result<PathBuf> {
    cargo_build(ctx)?;
    make_kernel_bin(ctx)?;
    Ok(ctx.kernel_bin())
}

fn build_android(ctx: &BuildContext) -> Result<PathBuf> {
    cargo_build(ctx)?;
    make_kernel_bin(ctx)?;
    make_kernel_gz(ctx)?;

    match ctx.spec.boot.format.as_str() {
        "android_boot_v1" => make_boot_img_v1(ctx)?,
        "android_boot_v2" => make_boot_img_v2(ctx)?,
        _ => unreachable!(),
    }

    Ok(ctx.boot_img())
}

fn execute_commands(commands: &[String], cwd: &Path) -> Result<()> {
    for cmd in commands {
        run_shell(cmd, cwd)?;
    }
    Ok(())
}

#[derive(Deserialize)]
struct CargoMetadata {
    packages: Vec<MetaPackage>,
    workspace_root: PathBuf,
}

#[derive(Deserialize)]
struct MetaPackage {
    name: String,
    manifest_path: PathBuf,
    dependencies: Vec<MetaDependency>,
}

#[derive(Deserialize)]
struct MetaDependency {
    name: String,
    path: Option<PathBuf>,
}

fn check_layers() -> Result<()> {
    let output = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(project_root())
        .output()
        .context("Failed to run cargo metadata")?;
    if !output.status.success() {
        bail!("cargo metadata failed with {}", output.status);
    }
    let meta: CargoMetadata =
        serde_json::from_slice(&output.stdout).context("Failed to parse cargo metadata")?;

    let crates = meta
        .packages
        .iter()
        .map(|pkg| {
            let rel = pkg
                .manifest_path
                .strip_prefix(&meta.workspace_root)
                .with_context(|| format!("крейт {} вне корня workspace", pkg.name))?;
            let dir = rel
                .components()
                .next()
                .and_then(|c| c.as_os_str().to_str())
                .with_context(|| format!("крейт {}: пустой относительный путь", pkg.name))?;
            Ok((pkg.name.clone(), dir.to_string()))
        })
        .collect::<Result<Vec<_>>>()?;

    let members: BTreeSet<&str> = crates.iter().map(|(name, _)| name.as_str()).collect();
    // dependencies включает normal-, dev- и build-зависимости; path = Some
    // отбирает локальные, дубликаты (normal + dev) схлопываются через BTreeSet.
    let edges: Vec<(String, String)> = meta
        .packages
        .iter()
        .flat_map(|pkg| {
            pkg.dependencies
                .iter()
                .filter(|dep| dep.path.is_some() && members.contains(dep.name.as_str()))
                .map(|dep| (pkg.name.clone(), dep.name.clone()))
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let violations = layers::check_layers(&crates, &edges)?;
    if !violations.is_empty() {
        for violation in &violations {
            eprintln!("{violation}");
        }
        bail!("check-layers: нарушений правил слоев: {}", violations.len());
    }
    println!(
        "check-layers: OK ({} крейтов, {} рёбер)",
        crates.len(),
        edges.len()
    );
    Ok(())
}

fn qemu_test(timeout: u64) -> Result<()> {
    // Pass 1: тесты внутри ядра (feature kernel-tests). Pass 2: тесты в
    // userland-образе testrunner.
    qemu_test_pass(
        "kernel",
        Some(String::from("kernel-tests")),
        "default",
        timeout,
    )?;
    qemu_test_pass("userland", None, "test", timeout)?;
    Ok(())
}

fn qemu_test_pass(label: &str, features: Option<String>, image: &str, timeout: u64) -> Result<()> {
    println!("=== qemu-test pass: {label} ===");

    let spec_path = PathBuf::from("devices/spec/qemu-aarch64-test.yaml");
    let ctx = BuildContext::new(spec_path, features)?;

    let manifest_path = image_manifest_path(&ctx.project_root, image)?;
    build_userland(&ctx.project_root, &manifest_path, &ctx.build_dir)?;
    build_binary(&ctx)?;

    let qemu_cmd = ctx
        .spec
        .run
        .first()
        .context("No run commands in qemu-aarch64-test.yaml")?;

    let full_cmd = format!("timeout {timeout} {qemu_cmd}");
    let status = Command::new("sh")
        .args(["-c", &full_cmd])
        .current_dir(&ctx.project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("Failed to launch QEMU")?;

    if !status.success() {
        let code = status.code().unwrap_or(1);
        if code == 124 {
            bail!("QEMU pass '{label}' timed out after {timeout}s");
        }
        bail!("QEMU pass '{label}' exited with code {code}");
    }

    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Build {
            spec,
            run,
            debug,
            features,
        } => {
            let ctx = BuildContext::new(spec, features)?;
            let manifest_path = image_manifest_path(&ctx.project_root, "default")?;
            build_userland(&ctx.project_root, &manifest_path, &ctx.build_dir)?;

            let output = match ctx.spec.boot.format.as_str() {
                "linux_arm64" => build_binary(&ctx)?,
                "android_boot_v1" | "android_boot_v2" => build_android(&ctx)?,
                "uefi" => bail!("UEFI boot not implemented yet"),
                _ => unreachable!(),
            };

            println!("Done: {}", output.display());

            if run {
                if ctx.spec.run.is_empty() {
                    eprintln!("Warning: no 'run' commands in spec");
                } else {
                    execute_commands(&ctx.spec.run, &ctx.project_root)?;
                }
            }

            if debug {
                if ctx.spec.debug.is_empty() {
                    eprintln!("Warning: no 'debug' commands in spec");
                } else {
                    execute_commands(&ctx.spec.debug, &ctx.project_root)?;
                }
            }
        }
        Commands::QemuTest { timeout } => qemu_test(timeout)?,
        Commands::BuildUserland { image } => {
            let project_root = project_root();
            let build_dir = project_root.join("target/build");
            fs::create_dir_all(&build_dir)?;
            let manifest_path = image_manifest_path(&project_root, &image)?;
            let output = build_userland(&project_root, &manifest_path, &build_dir)?;
            println!("Done: {}", output.display());
        }
        Commands::CheckLayers => check_layers()?,
    }

    Ok(())
}
