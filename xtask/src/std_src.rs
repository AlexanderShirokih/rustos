//! Материализация исходников `std` с наложенным оверлеем Molecular OS.
//!
//! Исходники берутся из компонента `rust-src` зафиксированного toolchain, копируются в
//! `target/std-src` и патчатся оверлеем. Сам toolchain не трогается: sysroot общий, а копия
//! принадлежит сборке.
//!
//! Повторный вызов ничего не делает, пока не сменились ни оверлей, ни toolchain: отпечаток
//! того и другого лежит в `target/std-src/.stamp`.
//!
//! [`materialize`] возвращает путь к дереву `library`, готовый для передачи в
//! `__CARGO_TESTS_ONLY_SRC_ROOT`. С `--verify` команда ещё и собирает под таргет программу на
//! `std`: оверлей должен не только ложиться, но и давать рабочее дерево. Исходник программы
//! генерируется, чтобы проверка ничего не требовала снаружи.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};

use crate::context::project_root;

const OVERLAY: &str = "patches/std-molecular.patch";

const DEST: &str = "target/std-src";

const TARGET_SPEC: &str = "targets/aarch64-unknown-molecular.json";

const BUILD_STD_FLAGS: [&str; 3] = [
    "-Zbuild-std=std,panic_abort",
    "-Zbuild-std-features=compiler-builtins-mem",
    "-Zjson-target-spec",
];

const PROBE_MANIFEST: &str = "\
[workspace]

[package]
name = \"std-probe\"
version = \"0.0.0\"
edition = \"2024\"
publish = false
";

const PROBE_MAIN: &str = "\
fn main() {
    println!(\"hello\");
}
";

pub(crate) fn materialize(force: bool) -> Result<PathBuf> {
    let root = project_root();
    let source = rust_src()?;
    let dest = root.join(DEST);
    let library = dest.join("library");

    let overlay_path = root.join(OVERLAY);
    let overlay = fs::read(&overlay_path)
        .with_context(|| format!("Failed to read {}", overlay_path.display()))?;
    let stamp_path = dest.join(".stamp");
    let stamp = stamp_for(&source, &overlay);

    if !force && library.is_dir() && fs::read(&stamp_path).ok().as_deref() == Some(stamp.as_slice())
    {
        return Ok(library);
    }

    if dest.exists() {
        fs::remove_dir_all(&dest).with_context(|| format!("Failed to clear {}", dest.display()))?;
    }
    fs::create_dir_all(&dest).with_context(|| format!("Failed to create {}", dest.display()))?;
    copy_tree(&source, &library)?;
    apply_overlay(&library, &overlay_path)?;
    fs::write(&stamp_path, &stamp)
        .with_context(|| format!("Failed to write {}", stamp_path.display()))?;

    println!("std-src: {} patched by {}", library.display(), OVERLAY);
    Ok(library)
}

pub(crate) fn run(force: bool, verify: bool) -> Result<()> {
    let library = materialize(force)?;
    println!("std-src: {}", library.display());
    if verify {
        verify_build(&library)?;
    }
    Ok(())
}

fn verify_build(library: &Path) -> Result<()> {
    let root = project_root();
    let probe = root.join(DEST).join("probe");
    fs::create_dir_all(probe.join("src"))
        .with_context(|| format!("Failed to create {}", probe.display()))?;
    fs::write(probe.join("Cargo.toml"), PROBE_MANIFEST)
        .context("Failed to write the probe manifest")?;
    fs::write(probe.join("src/main.rs"), PROBE_MAIN).context("Failed to write the probe source")?;

    let target = root.join(TARGET_SPEC);
    let status = Command::new("cargo")
        .args(["build", "--target"])
        .arg(&target)
        .args(BUILD_STD_FLAGS)
        .current_dir(&probe)
        .env("__CARGO_TESTS_ONLY_SRC_ROOT", library)
        .status()
        .context("Failed to run cargo build for the std probe")?;
    if !status.success() {
        bail!("std probe build failed with {status}");
    }

    let binary = probe.join("target/aarch64-unknown-molecular/debug/std-probe");
    if !binary.is_file() {
        bail!("std probe produced no binary at {}", binary.display());
    }
    println!("std-src: probe linked at {}", binary.display());
    Ok(())
}

fn rust_src() -> Result<PathBuf> {
    let output = Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .context("Failed to run rustc --print sysroot")?;
    if !output.status.success() {
        bail!("rustc --print sysroot failed with {}", output.status);
    }
    let sysroot = String::from_utf8(output.stdout).context("sysroot path is not UTF-8")?;
    let library = Path::new(sysroot.trim()).join("lib/rustlib/src/rust/library");
    if !library.is_dir() {
        bail!(
            "{} is missing: install the rust-src component listed in rust-toolchain.toml",
            library.display()
        );
    }
    Ok(library)
}

fn stamp_for(source: &Path, overlay: &[u8]) -> Vec<u8> {
    let mut stamp = source.display().to_string().into_bytes();
    stamp.push(b'\n');
    stamp.extend_from_slice(overlay);
    stamp
}

fn copy_tree(source: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest).with_context(|| format!("Failed to create {}", dest.display()))?;
    let entries =
        fs::read_dir(source).with_context(|| format!("Failed to read {}", source.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("Failed to walk {}", source.display()))?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let kind = entry
            .file_type()
            .with_context(|| format!("Failed to stat {}", from.display()))?;
        if kind.is_dir() {
            copy_tree(&from, &to)?;
        } else if kind.is_file() {
            fs::copy(&from, &to).with_context(|| format!("Failed to copy {}", from.display()))?;
            // Компонент rust-src ставится только на чтение, а копию предстоит патчить.
            let mut permissions = fs::metadata(&to)
                .with_context(|| format!("Failed to stat {}", to.display()))?
                .permissions();
            #[expect(
                clippy::permissions_set_readonly_false,
                reason = "the copy belongs to the build"
            )]
            permissions.set_readonly(false);
            fs::set_permissions(&to, permissions)
                .with_context(|| format!("Failed to unlock {}", to.display()))?;
        }
    }
    Ok(())
}

fn apply_overlay(library: &Path, overlay: &Path) -> Result<()> {
    let status = Command::new("git")
        .args(["apply", "-p1"])
        .arg(overlay)
        .current_dir(library)
        .status()
        .context("Failed to run git apply")?;
    if !status.success() {
        bail!(
            "git apply {} failed with {}: the overlay does not match the toolchain pinned in \
             rust-toolchain.toml, rebase it",
            overlay.display(),
            status
        );
    }
    Ok(())
}
