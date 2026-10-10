//! Компиляция C- и C++-исходников пакета userland под библиотеку C.
//!
//! Корень sysroot библиотеки задаёт переменная `MOLECULAR_LIBC_SYSROOT`. Подкаталог архитектуры
//! содержит заголовки, `lib/libc.a`, файл `cflags` с флагами, под которыми собрана библиотека,
//! и файл `ar` с архиватором. Там же лежат заголовки и архивы стандартной библиотеки C++ и
//! файл `cxxflags` с флагами C++-кода.
//! Sysroot должна подготовить `cargo xtask libc-src` до сборки пакета.

use std::{
    env,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{Error, error::cargo_variable};

pub(crate) const SYSROOT_VARIABLE: &str = "MOLECULAR_LIBC_SYSROOT";

const CXX_EXTENSIONS: &[&str] = &["cpp", "cc"];

const CXX_LIBRARIES: &[&str] = &["c++", "c++abi", "unwind"];

/// C- и C++-исходники пакета, которые компилируются в его статическую библиотеку.
///
/// Исходники компилируются с флагами архитектуры из sysroot и заголовками библиотеки C.
/// Исходники `.cpp` и `.cc` компилируются как C++, и пакет с ними компонуется со стандартной
/// библиотекой C++. Уровень оптимизации и отладочная информация следуют профилю cargo. Пути
/// исходников и каталогов заголовков берутся от каталога пакета.
pub struct CSources<'a> {
    name: &'a str,
    sources: &'a [&'a str],
    definitions: Vec<&'a str>,
    include_dirs: Vec<PathBuf>,
    foreign: bool,
}

struct ArchSysroot {
    dir: PathBuf,
    cflags: Vec<String>,
    cxxflags: Vec<String>,
    archiver: String,
}

impl<'a> CSources<'a> {
    /// Библиотека `name` из исходников `sources` собственного кода пакета.
    ///
    /// Предупреждение компилятора в собственном коде останавливает сборку.
    pub fn new(name: &'a str, sources: &'a [&'a str]) -> Self {
        Self {
            name,
            sources,
            definitions: Vec::new(),
            include_dirs: Vec::new(),
            foreign: false,
        }
    }

    /// Определяет макрос во всех исходниках библиотеки.
    ///
    /// `definition` имеет форму аргумента `-D` у `clang`: `NAME` или `NAME=value`.
    pub fn define(mut self, definition: &'a str) -> Self {
        self.definitions.push(definition);
        self
    }

    /// Добавляет каталог поиска заголовков перед заголовками библиотеки C.
    ///
    /// Изменение файла в каталоге пересобирает библиотеку.
    pub fn include_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.include_dirs.push(dir.as_ref().to_path_buf());
        self
    }

    /// Отмечает исходники как сторонний код, предупреждения в котором сборку не останавливают.
    pub fn foreign(mut self) -> Self {
        self.foreign = true;
        self
    }

    /// Компилирует исходники в статическую библиотеку и передаёт её компоновщику.
    pub fn compile(self) -> Result<(), Error> {
        let name = self.name;
        let arch = ArchSysroot::from_env()?;
        let out_dir = PathBuf::from(cargo_variable("OUT_DIR")?);
        let opt_level = cargo_variable("OPT_LEVEL")?;
        let debug = env::var("DEBUG").is_ok_and(|value| value != "false" && value != "0");

        let warnings: &[&str] = if self.foreign {
            &["-w"]
        } else {
            &["-Wall", "-Werror"]
        };

        for dir in &self.include_dirs {
            println!("cargo:rerun-if-changed={}", dir.display());
        }

        let mut objects = Vec::new();
        let mut has_cxx = false;
        for source in self.sources {
            println!("cargo:rerun-if-changed={source}");
            let path = Path::new(source);
            let stem = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| Error::SourceName((*source).to_string()))?;
            let object = out_dir.join(format!("{name}-{stem}.o"));
            let mut clang = Command::new("clang");
            clang.args(&arch.cflags).arg("-nostdlibinc");

            let cxx = path
                .extension()
                .is_some_and(|extension| CXX_EXTENSIONS.iter().any(|cxx| extension == *cxx));
            if cxx {
                clang.args(["-x", "c++"]).args(&arch.cxxflags);
                has_cxx = true;
            }

            clang
                .args(
                    self.include_dirs
                        .iter()
                        .flat_map(|dir| [OsStr::new("-I"), dir.as_os_str()]),
                )
                .arg("-isystem")
                .arg(arch.dir.join("include"))
                .args(
                    self.definitions
                        .iter()
                        .map(|definition| format!("-D{definition}")),
                )
                .arg(format!("-O{opt_level}"))
                .args(["-ffunction-sections", "-fdata-sections"])
                .args(warnings)
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
        if has_cxx {
            println!(
                "cargo:rustc-link-search=native={}",
                arch.dir.join("lib").display()
            );
            for library in CXX_LIBRARIES {
                println!("cargo:rustc-link-lib=static:-bundle={library}");
            }
        }
        Ok(())
    }
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
        let cxxflags = read_sysroot_file(dir.join("cxxflags"))?;
        let archiver = read_sysroot_file(dir.join("ar"))?;
        Ok(Self {
            dir,
            cflags: cflags.split_whitespace().map(String::from).collect(),
            cxxflags: cxxflags.split_whitespace().map(String::from).collect(),
            archiver: archiver.trim().to_string(),
        })
    }
}

/// Подключает `libc.a` из sysroot к компоновке каждой программы, зависящей от пакета.
///
/// Архив не копируется в rlib пакета.
pub fn link_libc() -> Result<(), Error> {
    let arch = ArchSysroot::from_env()?;
    println!(
        "cargo:rustc-link-search=native={}",
        arch.dir.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static:-bundle=c");
    Ok(())
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
