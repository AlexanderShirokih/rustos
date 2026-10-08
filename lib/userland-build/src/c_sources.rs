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

const SYSROOT_VARIABLE: &str = "MOLECULAR_LIBC_SYSROOT";

const PREPARE_COMMAND: &str = "cargo xtask libc-src";

/// Компилирует C-исходники пакета в статическую библиотеку `name` и передаёт её компоновщику.
///
/// Пути `sources` берутся от каталога пакета. Уровень оптимизации и отладочная информация
/// следуют профилю cargo.
#[allow(clippy::expect_used, clippy::panic)]
pub fn compile_c(name: &str, sources: &[&str]) {
    // Отказ окружения, компилятора или файловой системы прекращает сборку, и `cargo` читает
    // именно панику скрипта.
    let arch = ArchSysroot::from_env();
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("out dir"));
    let opt_level = env::var("OPT_LEVEL").expect("opt level");
    let debug = env::var("DEBUG").is_ok_and(|value| value != "false" && value != "0");

    let mut objects = Vec::new();
    for source in sources {
        println!("cargo:rerun-if-changed={source}");
        let stem = Path::new(source)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("C source file name");
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
        run(&mut clang, &format!("clang {source}"));
        objects.push(object);
    }

    let archive = out_dir.join(format!("lib{name}.a"));
    // Архив собирается заново: `ar` дописывает члены в существующий.
    let _ = fs::remove_file(&archive);
    let mut ar = Command::new(&arch.archiver);
    ar.arg("crs").arg(&archive).args(&objects);
    run(&mut ar, &format!("ar lib{name}.a"));

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static={name}");
}

/// Передаёт компоновщику `libc.a` библиотеки C.
///
/// Компоновщик берёт библиотеку из sysroot при сборке программы, в rlib пакета она не входит.
pub fn link_libc() {
    let arch = ArchSysroot::from_env();
    println!(
        "cargo:rustc-link-search=native={}",
        arch.dir.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static:-bundle=c");
}

struct ArchSysroot {
    dir: PathBuf,
    cflags: Vec<String>,
    archiver: String,
}

impl ArchSysroot {
    #[allow(clippy::expect_used, clippy::panic)]
    fn from_env() -> Self {
        println!("cargo:rerun-if-env-changed={SYSROOT_VARIABLE}");
        let root = env::var(SYSROOT_VARIABLE).unwrap_or_else(|_| {
            panic!("{SYSROOT_VARIABLE} is not set; build the package through `cargo xtask`")
        });
        let arch = env::var("CARGO_CFG_TARGET_ARCH").expect("target arch");
        let root = PathBuf::from(root);
        let dir = root.join(&arch);
        // Отпечаток меняется с каждой пересборкой библиотеки, и пакет пересобирается с ней.
        println!("cargo:rerun-if-changed={}", root.join(".stamp").display());

        let cflags_path = dir.join("cflags");
        let cflags = read_sysroot_file(&cflags_path, &arch);
        let archiver = read_sysroot_file(&dir.join("ar"), &arch);
        Self {
            dir,
            cflags: cflags.split_whitespace().map(String::from).collect(),
            archiver: archiver.trim().to_string(),
        }
    }
}

#[allow(clippy::panic)]
fn read_sysroot_file(path: &Path, arch: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|_| {
        panic!(
            "{} is missing: run `{PREPARE_COMMAND}` to build the C library for {arch}",
            path.display()
        )
    })
}

#[allow(clippy::panic)]
fn run(command: &mut Command, what: &str) {
    match command.status() {
        Ok(status) if status.success() => {}
        Ok(status) => panic!("{what} failed with {status}"),
        Err(error) => panic!("failed to run {what}: {error}"),
    }
}
