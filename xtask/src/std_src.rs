//! Подготовка исходников, из которых собирается `std` под RustOS.
//!
//! Исходники берутся из компонента `rust-src` зафиксированного toolchain, копируются в
//! `target/std-src` и патчатся оверлеем. Сам toolchain не трогается: sysroot общий, а копия
//! принадлежит сборке.
//!
//! Повторный вызов ничего не делает, пока не сменились ни оверлей, ни toolchain: отпечаток
//! того и другого лежит в `target/std-src/.stamp`.
//!
//! Вторая половина подготовки — крейты дерева, которые собираются внутрь `std`. Юниты графа
//! `-Zbuild-std` cargo пересобирает по изменению зависимостей и флагов, но не по изменению
//! исходников: правка в `lib/std-rt` молча уходит в собранный `std` старой версией. Отпечаток
//! их файлов лежит в `target/std-src/.graph-stamp`, и по его смене эти пакеты чистятся.

use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::UNIX_EPOCH,
};

use anyhow::{Context, Result, bail};
use userland_image_tool::USERLAND_WORKSPACE;

use crate::{context::project_root, shell};

const OVERLAY: &str = "patches/std-molecular.patch";

const DEST: &str = "target/std-src";

const LIB_DIR: &str = "lib";

const STD_GRAPH_FEATURE: &str = "rustc-dep-of-std";

pub(crate) fn materialize(force: bool) -> Result<PathBuf> {
    let root = project_root();
    let library = materialize_tree(&root, force)?;
    refresh_std_graph(&root, &library)?;
    Ok(library)
}

fn materialize_tree(root: &Path, force: bool) -> Result<PathBuf> {
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

fn refresh_std_graph(root: &Path, library: &Path) -> Result<()> {
    let packages = std_graph_packages(root)?;
    if packages.is_empty() {
        bail!("no crate under {LIB_DIR}/ declares the '{STD_GRAPH_FEATURE}' feature");
    }
    let stamp = std_graph_stamp(&packages)?;
    let stamp_path = root.join(DEST).join(".graph-stamp");
    if fs::read_to_string(&stamp_path).ok().as_deref() == Some(stamp.as_str()) {
        return Ok(());
    }

    // `cargo clean --package` снимает артефакты одного профиля: образ идёт release, линты dev.
    for profile in ["dev", "release"] {
        let mut command = Command::new("cargo");
        command.args(["clean", "--profile", profile]);
        for package in &packages {
            command.args(["--package", &package.name]);
        }
        command
            .current_dir(root.join(USERLAND_WORKSPACE))
            .env("__CARGO_TESTS_ONLY_SRC_ROOT", library);
        shell::run(&mut command, "cargo clean for the std graph crates")?;
    }

    fs::write(&stamp_path, &stamp)
        .with_context(|| format!("Failed to write {}", stamp_path.display()))?;
    Ok(())
}

struct StdGraphPackage {
    name: String,
    dir: PathBuf,
}

fn std_graph_packages(root: &Path) -> Result<Vec<StdGraphPackage>> {
    let lib = root.join(LIB_DIR);
    let entries =
        fs::read_dir(&lib).with_context(|| format!("Failed to read {}", lib.display()))?;
    let mut packages = Vec::new();
    for entry in entries {
        let dir = entry
            .with_context(|| format!("Failed to walk {}", lib.display()))?
            .path();
        let manifest_path = dir.join("Cargo.toml");
        let Ok(text) = fs::read_to_string(&manifest_path) else {
            continue;
        };
        let manifest = text
            .parse::<toml::Table>()
            .with_context(|| format!("Failed to parse {}", manifest_path.display()))?;
        let declares = manifest
            .get("features")
            .and_then(toml::Value::as_table)
            .is_some_and(|features| features.contains_key(STD_GRAPH_FEATURE));
        if !declares {
            continue;
        }
        let name = manifest
            .get("package")
            .and_then(toml::Value::as_table)
            .and_then(|package| package.get("name"))
            .and_then(toml::Value::as_str)
            .with_context(|| format!("{} declares no package name", manifest_path.display()))?;
        packages.push(StdGraphPackage {
            name: name.to_string(),
            dir,
        });
    }
    packages.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(packages)
}

fn std_graph_stamp(packages: &[StdGraphPackage]) -> Result<String> {
    let mut stamp = String::new();
    for package in packages {
        stamp_tree(&package.dir, &mut stamp)?;
    }
    Ok(stamp)
}

fn stamp_tree(dir: &Path, stamp: &mut String) -> Result<()> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("Failed to read {}", dir.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<_, _>>()
        .with_context(|| format!("Failed to walk {}", dir.display()))?;
    entries.sort();
    for path in entries {
        let metadata =
            fs::metadata(&path).with_context(|| format!("Failed to stat {}", path.display()))?;
        if metadata.is_dir() {
            // Каталог сборки принадлежит cargo и в отпечаток исходников не входит.
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            stamp_tree(&path, stamp)?;
            continue;
        }
        let modified = metadata
            .modified()
            .with_context(|| format!("Failed to read mtime of {}", path.display()))?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        writeln!(
            stamp,
            "{} {} {}",
            path.display(),
            metadata.len(),
            modified.as_nanos()
        )
        .expect("writing to a String does not fail");
    }
    Ok(())
}

pub(crate) fn run(force: bool) -> Result<PathBuf> {
    let library = materialize(force)?;
    println!("std-src: {}", library.display());
    Ok(library)
}

fn rust_src() -> Result<PathBuf> {
    let mut command = Command::new("rustc");
    command.args(["--print", "sysroot"]);
    let sysroot = shell::output(&mut command, "rustc --print sysroot")?;
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
    let mut command = Command::new("git");
    command
        .args(["apply", "-p1"])
        .arg(overlay)
        .current_dir(library);
    shell::run(&mut command, &format!("git apply {}", overlay.display())).context(
        "the overlay does not match the toolchain pinned in rust-toolchain.toml, rebase it",
    )
}
