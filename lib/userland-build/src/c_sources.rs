//! Компиляция C-исходников пакета userland под библиотеку C.
//!
//! Корень sysroot библиотеки задаёт переменная `MOLECULAR_LIBC_SYSROOT`. Подкаталог архитектуры
//! содержит заголовки, `lib/libc.a`, файл `cflags` с флагами, под которыми собрана библиотека,
//! и файл `ar` с архиватором.
//! Sysroot должна подготовить `cargo xtask libc-src` до сборки пакета.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{Error, error::cargo_variable};

pub(crate) const SYSROOT_VARIABLE: &str = "MOLECULAR_LIBC_SYSROOT";

/// Компилирует C-исходники пакета в статическую библиотеку `name` и передаёт её компоновщику.
///
/// Пути `sources` берутся от каталога пакета. Уровень оптимизации и отладочная информация
/// следуют профилю cargo.
pub fn compile_c(name: &str, sources: &[&str]) -> Result<(), Error> {
    let arch = ArchSysroot::from_env()?;
    let out_dir = PathBuf::from(cargo_variable("OUT_DIR")?);
    let opt_level = cargo_variable("OPT_LEVEL")?;
    let debug = env::var("DEBUG").is_ok_and(|value| value != "false" && value != "0");

    let mut objects = Vec::new();
    for source in sources {
        println!("cargo:rerun-if-changed={source}");
        let stem = Path::new(source)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| Error::SourceName((*source).to_string()))?;
        let object = out_dir.join(format!("{name}-{stem}.o"));
        let mut clang = Command::new("clang");
        clang
            .args(&arch.cflags)
            .arg("-nostdlibinc")
            .arg("-isystem")
            .arg(arch.dir.join("include"))
            .arg(format!("-O{opt_level}"))
            .args(["-ffunction-sections", "-fdata-sections", "-Wall", "-Werror"])
            .args(debug.then_some("-g"))
            .arg("-c")
            .arg(source)
            .arg("-o")
            .arg(&object);
        run(&mut clang, format!("clang {source}"))?;
        objects.push(object);
    }

    let archive = out_dir.join(format!("lib{name}.a"));
    // Архив собирается заново: `ar` дописывает члены в существующий.
    let _ = fs::remove_file(&archive);
    let mut ar = Command::new(&arch.archiver);
    ar.arg("crs").arg(&archive).args(&objects);
    run(&mut ar, format!("ar lib{name}.a"))?;

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static={name}");
    Ok(())
}

/// Передаёт компоновщику `libc.a` библиотеки C.
///
/// Компоновщик берёт библиотеку из sysroot при сборке программы, в rlib пакета она не входит.
pub fn link_libc() -> Result<(), Error> {
    let arch = ArchSysroot::from_env()?;
    println!(
        "cargo:rustc-link-search=native={}",
        arch.dir.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static:-bundle=c");
    Ok(())
}

struct ArchSysroot {
    dir: PathBuf,
    cflags: Vec<String>,
    archiver: String,
}

impl ArchSysroot {
    fn from_env() -> Result<Self, Error> {
        println!("cargo:rerun-if-env-changed={SYSROOT_VARIABLE}");
        let root = env::var(SYSROOT_VARIABLE).map_err(|_| Error::SysrootVariableUnset)?;
        let arch = cargo_variable("CARGO_CFG_TARGET_ARCH")?;
        let root = PathBuf::from(root);
        let dir = root.join(&arch);
        // Отпечаток меняется с каждой пересборкой библиотеки, и пакет пересобирается с ней.
        println!("cargo:rerun-if-changed={}", root.join(".stamp").display());

        let cflags = read_sysroot_file(dir.join("cflags"))?;
        let archiver = read_sysroot_file(dir.join("ar"))?;
        Ok(Self {
            dir,
            cflags: cflags.split_whitespace().map(String::from).collect(),
            archiver: archiver.trim().to_string(),
        })
    }
}

fn read_sysroot_file(path: PathBuf) -> Result<String, Error> {
    fs::read_to_string(&path).map_err(|_| Error::LibcNotBuilt { path })
}

fn run(command: &mut Command, what: String) -> Result<(), Error> {
    match command.status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(Error::Failed { what, status }),
        Err(source) => Err(Error::Spawn { what, source }),
    }
}
