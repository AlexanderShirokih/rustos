//! Подготовка библиотек C и C++ программ userland: picolibc и рантаймы C++ LLVM из закреплённых
//! релизов, собранные под каждую архитектуру реестра.
//!
//! Загрузка закреплённого кода раскладывает исходники picolibc в `target/libc-src`, а нужные
//! сборке каталоги LLVM - в `target/llvm-src`. Сборка каждой архитектуры кладёт в
//! `target/libc-sysroot/<arch>` заголовки, `lib/libc.a`, файл `cflags` с флагами компилятора, под
//! которыми собрана библиотека, и файл `ar` с архиватором. К заголовкам библиотеки добавляется
//! `pthread.h` системного слоя `c-library`.
//!
//! Вторая стадия собирает поверх библиотеки C той же архитектуры libc++, libc++abi и libunwind
//! CMake-сборкой runtimes. Она кладёт заголовки в `include/c++/v1`, архивы в `lib` и файл
//! `cxxflags` с флагами C++-кода программы. Ожидания libc++ со сроком идут через
//! `pthread_cond_clockwait` по монотонным часам.
//!
//! Отпечаток релизов, версий `clang` и `cmake`, пути архиватора, опций сборки, флагов архитектур
//! и содержимого `pthread.h` лежит в `target/libc-sysroot/.stamp`, и повторный вызов с тем же
//! отпечатком ничего не делает. Одновременные вызовы разводит замок `target/libc-sysroot.lock`.
//!
//! Библиотека собирается многопоточной, без своей кучи, `crt0` и semihosting, с консолью поверх
//! `read`/`write`, замком на каждый вызов ввода-вывода и конструкторами через
//! `.init_array`/`.fini_array`. Адрес `errno` отдаёт функция `__errno_location` системного слоя.
//! Замки библиотеки даёт системный слой, и заглушки замков из архива удаляются.
//!
//! Рантаймы C++ собираются с исключениями, RTTI, потоками поверх pthread API, локализацией и
//! широкими символами, без `<filesystem>` и `random_device`. libc++abi раскручивает стек
//! статическим libunwind bare-metal, хранит состояние исключений потока в `thread_local` и
//! регистрирует деструкторы `thread_local` через `__cxa_thread_atexit_impl` библиотеки C.
//!
//! Таблица флагов по имени архитектуры - единственная платформенная часть подготовки. Флаги C
//! задают тот же набор инструкций, что и системный таргет архитектуры, и запрещают
//! невыровненные обращения.

use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::{
    context::{lock_tree, project_root},
    fetch::{self, Format, Pin},
    layers::{ArchEntry, arch_registry},
    shell,
};

const VERSION: &str = "1.8.12";
const ARCHIVE_SHA256: &str = "64e8c412e1c40fa6eb1a72d2b5cdbcbfe6ceca4cbea454edbad54557ffc747fa";
const RELEASE: Pin = Pin::Archive {
    url: "https://github.com/picolibc/picolibc/releases/download/1.8.12/picolibc-1.8.12.tar.xz",
    sha256: ARCHIVE_SHA256,
    format: Format::TarXz,
    members: &[],
};

const LLVM_RELEASE: Pin = Pin::Archive {
    url: "https://github.com/llvm/llvm-project/releases/download/llvmorg-18.1.8/llvm-project-18.1.8.src.tar.xz",
    sha256: "0b58557a6d32ceee97c8d533a59b9212d87e0fc4d2833924eb6c611247db2f2a",
    format: Format::TarXz,
    members: &[
        "llvm-project-18.1.8.src/cmake",
        "llvm-project-18.1.8.src/libcxx",
        "llvm-project-18.1.8.src/libcxxabi",
        "llvm-project-18.1.8.src/libunwind",
        "llvm-project-18.1.8.src/runtimes",
        "llvm-project-18.1.8.src/llvm/cmake",
        "llvm-project-18.1.8.src/llvm/utils",
        "llvm-project-18.1.8.src/third-party",
    ],
};

const LLVM_TREE: &str = "llvm-project-18.1.8.src";

const MIN_CLANG_MAJOR: u32 = 18;

const SOURCE_DIR: &str = "target/libc-src";

const LLVM_SOURCE_DIR: &str = "target/llvm-src";

const BUILD_DIR: &str = "target/libc-build";

const SYSROOT_DIR: &str = "target/libc-sysroot";

const LOCK: &str = "target/libc-sysroot.lock";

const CFLAGS_FILE: &str = "cflags";

const ARCHIVER_FILE: &str = "ar";

const CXXFLAGS_FILE: &str = "cxxflags";

const CONFIG_SITE: &str = "include/c++/v1/__config_site";

const COND_CLOCKWAIT_DEFINE: &str = "\n#define _LIBCPP_HAS_COND_CLOCKWAIT\n";

const THREADS_HEADER: &str = "lib/c-library/include/pthread.h";

const LOCK_STUBS_MEMBER: &str = "libc_misc_lock.c.o";

const MESON_OPTIONS: &[&str] = &[
    "-Dmultilib=false",
    "-Dpicocrt=false",
    "-Dpicocrt-lib=false",
    "-Dsemihost=false",
    "-Dsingle-thread=false",
    "-Dstdio-locking=true",
    "-Dthread-local-storage=false",
    "-Denable-malloc=false",
    "-Dposix-console=true",
    "-Dinitfini-array=true",
    // Компилятор считает, что `malloc` не пишет в глобалы модуля, и сворачивает чтение
    // глобальной `errno` после вызова. Вызов функции сохраняет это чтение.
    "-Derrno-function=__errno_location",
    "-Dspecsdir=none",
    "-Dtests=false",
];

const COMMON_CFLAGS: &[&str] = &["-femulated-tls"];

const RUNTIMES_FLAGS: &[&str] = &[
    "-fPIC",
    // Открывает `strto*_l`, `locale_t` и `isascii` picolibc.
    "-D_GNU_SOURCE",
    // picolibc задаёт макрос только для RTEMS, а без него libc++ не собирает `steady_clock`.
    "-D_POSIX_TIMERS=1",
];

const CMAKE_OPTIONS: &[&str] = &[
    "-DLLVM_ENABLE_RUNTIMES=libcxx;libcxxabi;libunwind",
    "-DCMAKE_BUILD_TYPE=Release",
    "-DCMAKE_SYSTEM_NAME=Generic",
    // Только для UNIX libc++abi собирает `cxa_thread_atexit.cpp` с деструкторами `thread_local`.
    "-DUNIX=ON",
    // Пробы компилятора только компилируют, так как на хосте нет среды компоновки программ.
    "-DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY",
    "-DLLVM_ENABLE_PER_TARGET_RUNTIME_DIR=OFF",
    "-DLLVM_INCLUDE_TESTS=OFF",
    "-DLIBCXX_ENABLE_SHARED=OFF",
    "-DLIBCXXABI_ENABLE_SHARED=OFF",
    "-DLIBUNWIND_ENABLE_SHARED=OFF",
    "-DLIBCXX_ENABLE_EXCEPTIONS=ON",
    "-DLIBCXX_ENABLE_RTTI=ON",
    "-DLIBCXX_ENABLE_THREADS=ON",
    "-DLIBCXXABI_ENABLE_THREADS=ON",
    "-DLIBUNWIND_ENABLE_THREADS=ON",
    "-DLIBCXX_HAS_PTHREAD_API=ON",
    "-DLIBCXX_ENABLE_LOCALIZATION=ON",
    "-DLIBCXX_ENABLE_WIDE_CHARACTERS=ON",
    "-DLIBCXX_ENABLE_FILESYSTEM=OFF",
    "-DLIBCXX_ENABLE_RANDOM_DEVICE=OFF",
    "-DLIBCXX_INCLUDE_BENCHMARKS=OFF",
    "-DLIBCXX_INCLUDE_DOCS=OFF",
    "-DLIBUNWIND_INCLUDE_DOCS=OFF",
    "-DLIBCXX_CXX_ABI=libcxxabi",
    "-DLIBCXXABI_USE_LLVM_UNWINDER=ON",
    "-DLIBCXXABI_ENABLE_STATIC_UNWINDER=ON",
    "-DLIBCXXABI_STATICALLY_LINK_UNWINDER_IN_STATIC_LIBRARY=OFF",
    "-DLIBUNWIND_IS_BAREMETAL=ON",
    "-DLIBCXX_USE_COMPILER_RT=ON",
    "-DLIBCXXABI_USE_COMPILER_RT=ON",
    "-DLIBCXXABI_ADDITIONAL_COMPILE_FLAGS=-DHAS_THREAD_LOCAL",
    "-DLIBUNWIND_USE_COMPILER_RT=ON",
    // Пробы библиотек без компоновки проходят ложно, поэтому их результаты заданы явно.
    // `__cxa_thread_atexit_impl` даёт `c-library`.
    "-DLIBCXX_HAS_PTHREAD_LIB=OFF",
    "-DLIBCXX_HAS_RT_LIB=OFF",
    "-DLIBCXX_HAS_ATOMIC_LIB=OFF",
    "-DLIBCXXABI_HAS_C_LIB=OFF",
    "-DLIBCXXABI_HAS_PTHREAD_LIB=OFF",
    "-DLIBCXXABI_HAS_CXA_THREAD_ATEXIT_IMPL=ON",
    "-DLIBUNWIND_HAS_DL_LIB=OFF",
    "-DLIBUNWIND_HAS_PTHREAD_LIB=OFF",
];

struct ArchFlags {
    name: &'static str,
    cpu_family: &'static str,
    cflags: &'static [&'static str],
}

const ARCH_FLAGS: &[ArchFlags] = &[ArchFlags {
    name: "aarch64",
    cpu_family: "aarch64",
    cflags: &["--target=aarch64-none-elf", "-march=armv8-a"],
}];

pub(crate) fn materialize(force: bool) -> Result<PathBuf> {
    let root = project_root();
    // Вызов, пришедший вторым, ждёт освобождения и получает готовый sysroot.
    let _lock = lock_tree(&root.join(LOCK), "libc-src")?;

    let registry = arch_registry(&root)?;
    let archs = registry
        .iter()
        .map(arch_flags)
        .collect::<Result<Vec<_>>>()?;
    let clang = clang_version()?;
    check_clang(&clang)?;
    let archiver = archiver()?;
    let cmake = cmake_version()?;
    let threads_header = root.join(THREADS_HEADER);
    let header = fs::read(&threads_header)
        .with_context(|| format!("Failed to read {}", threads_header.display()))?;

    let sysroot = root.join(SYSROOT_DIR);
    let stamp_path = sysroot.join(".stamp");
    let stamp = stamp_for(&clang, &cmake, &archiver, &header, &LLVM_RELEASE, &archs);
    if !force && fs::read_to_string(&stamp_path).ok().as_deref() == Some(stamp.as_str()) {
        return Ok(sysroot);
    }

    let sources = fetch::materialize("picolibc", &root.join(SOURCE_DIR), &RELEASE, force)?
        .join(format!("picolibc-{VERSION}"));
    let llvm = fetch::materialize("llvm", &root.join(LLVM_SOURCE_DIR), &LLVM_RELEASE, force)?
        .join(LLVM_TREE);
    if sysroot.exists() {
        fs::remove_dir_all(&sysroot)
            .with_context(|| format!("Failed to clear {}", sysroot.display()))?;
    }
    for arch in &archs {
        let prefix = sysroot.join(arch.name);
        build_arch(arch, &archiver, &sources, &root.join(BUILD_DIR), &prefix)?;
        install_threads_header(&header, &prefix)?;
        build_runtimes(arch, &archiver, &llvm, &root.join(BUILD_DIR), &prefix)?;
    }
    fs::write(&stamp_path, &stamp)
        .with_context(|| format!("Failed to write {}", stamp_path.display()))?;

    println!(
        "libc-src: picolibc {VERSION} and the C++ runtimes built under {}",
        sysroot.display()
    );
    Ok(sysroot)
}

pub(crate) fn run(force: bool) -> Result<()> {
    let sysroot = materialize(force)?;
    println!("libc-src: {}", sysroot.display());
    Ok(())
}

fn arch_flags(entry: &ArchEntry) -> Result<&'static ArchFlags> {
    ARCH_FLAGS
        .iter()
        .find(|flags| flags.name == entry.name)
        .with_context(|| {
            format!(
                "architecture `{}` has no C compiler flags in xtask/src/libc_src.rs",
                entry.name
            )
        })
}

fn clang_version() -> Result<String> {
    let mut command = Command::new("clang");
    command.arg("-dumpversion");
    let version = shell::output(&mut command, "clang -dumpversion")
        .context("clang is required to build the C library, see docs/environment.md")?;
    Ok(version.trim().to_string())
}

fn cmake_version() -> Result<String> {
    let mut command = Command::new("cmake");
    command.arg("--version");
    let version = shell::output(&mut command, "cmake --version")
        .context("cmake is required to build the C++ runtimes, see docs/environment.md")?;
    Ok(version.lines().next().unwrap_or_default().to_string())
}

fn check_clang(version: &str) -> Result<()> {
    let major = clang_major(version)
        .with_context(|| format!("clang -dumpversion printed `{version}`, not a version"))?;
    if major < MIN_CLANG_MAJOR {
        bail!(
            "clang {version} is too old: picolibc {VERSION} needs clang {MIN_CLANG_MAJOR} or newer"
        );
    }
    Ok(())
}

fn clang_major(version: &str) -> Option<u32> {
    version.split('.').next()?.parse().ok()
}

fn archiver() -> Result<String> {
    let mut command = Command::new("clang");
    command.arg("-print-prog-name=llvm-ar");
    let found = shell::output(&mut command, "clang -print-prog-name=llvm-ar")?;
    Ok(archiver_from(found.trim()))
}

fn archiver_from(found: &str) -> String {
    // Для найденной программы `clang` печатает путь, для ненайденной - голое имя.
    if Path::new(found).is_absolute() {
        found.to_string()
    } else {
        "ar".to_string()
    }
}

fn stamp_for(
    clang: &str,
    cmake: &str,
    archiver: &str,
    header: &[u8],
    llvm: &Pin,
    archs: &[&ArchFlags],
) -> String {
    let mut stamp = format!(
        "picolibc {VERSION} {ARCHIVE_SHA256}\n{llvm:?}\nclang {clang}\n{cmake}\narchiver {archiver}\n\
         pthread.h {:x}\n",
        Sha256::digest(header)
    );
    for options in [MESON_OPTIONS, RUNTIMES_FLAGS, CMAKE_OPTIONS] {
        stamp.push_str(&options.join(" "));
        stamp.push('\n');
    }
    for arch in archs {
        // Запись в `String` не отказывает.
        let _ = writeln!(stamp, "{} {}", arch.name, cflags(arch).join(" "));
    }
    stamp
}

fn cflags(arch: &ArchFlags) -> Vec<&'static str> {
    // `-femulated-tls` направляет доступ к thread-local переменным в `__emutls_get_address`
    // системного слоя, потому что регистром потока владеет рантайм процесса.
    arch.cflags.iter().chain(COMMON_CFLAGS).copied().collect()
}

fn build_arch(
    arch: &ArchFlags,
    archiver: &str,
    sources: &Path,
    build_root: &Path,
    prefix: &Path,
) -> Result<()> {
    let build = build_root.join(arch.name);
    if build.exists() {
        fs::remove_dir_all(&build)
            .with_context(|| format!("Failed to clear {}", build.display()))?;
    }
    fs::create_dir_all(&build).with_context(|| format!("Failed to create {}", build.display()))?;
    let cross_file = build.with_extension("cross");
    fs::write(&cross_file, cross_file_text(arch, archiver))
        .with_context(|| format!("Failed to write {}", cross_file.display()))?;

    let mut setup = Command::new("meson");
    setup
        .arg("setup")
        .arg(&build)
        .arg(sources)
        .arg("--cross-file")
        .arg(&cross_file)
        .arg("--prefix")
        .arg(prefix)
        .args(["--libdir", "lib", "--includedir", "include"])
        .args(["--buildtype", "release"])
        .args(MESON_OPTIONS);
    shell::run(&mut setup, &format!("meson setup for {}", arch.name))?;

    let mut install = Command::new("meson");
    install.args(["install", "--quiet", "-C"]).arg(&build);
    shell::run(&mut install, &format!("meson install for {}", arch.name))?;
    remove_lock_stubs(archiver, &prefix.join("lib").join("libc.a"))?;

    let cflags_file = prefix.join(CFLAGS_FILE);
    fs::write(&cflags_file, cflags(arch).join(" "))
        .with_context(|| format!("Failed to write {}", cflags_file.display()))?;
    let archiver_file = prefix.join(ARCHIVER_FILE);
    fs::write(&archiver_file, archiver)
        .with_context(|| format!("Failed to write {}", archiver_file.display()))
}

fn remove_lock_stubs(archiver: &str, library: &Path) -> Result<()> {
    let mut list = Command::new(archiver);
    list.arg("t").arg(library);
    let members = shell::output(&mut list, &format!("{archiver} t {}", library.display()))?;
    if !members.lines().any(|member| member == LOCK_STUBS_MEMBER) {
        bail!(
            "{} has no member {LOCK_STUBS_MEMBER} with the lock stubs",
            library.display()
        );
    }

    let mut delete = Command::new(archiver);
    delete.arg("d").arg(library).arg(LOCK_STUBS_MEMBER);
    shell::run(&mut delete, &format!("{archiver} d {}", library.display()))
}

fn install_threads_header(header: &[u8], prefix: &Path) -> Result<()> {
    let target = prefix.join("include").join("pthread.h");
    fs::write(&target, header).with_context(|| format!("Failed to write {}", target.display()))
}

fn build_runtimes(
    arch: &ArchFlags,
    archiver: &str,
    sources: &Path,
    build_root: &Path,
    prefix: &Path,
) -> Result<()> {
    let build = build_root.join(format!("{}-cxx", arch.name));
    if build.exists() {
        fs::remove_dir_all(&build)
            .with_context(|| format!("Failed to clear {}", build.display()))?;
    }
    let include = prefix.join("include");
    let flags = cflags(arch)
        .into_iter()
        .chain(RUNTIMES_FLAGS.iter().copied())
        .map(String::from)
        .chain([
            "-nostdlibinc".to_string(),
            format!("-isystem {}", include.display()),
        ])
        .collect::<Vec<_>>()
        .join(" ");

    let mut configure = Command::new("cmake");
    configure
        .args(["-G", "Ninja", "-S"])
        .arg(sources.join("runtimes"))
        .arg("-B")
        .arg(&build)
        .arg(format!("-DCMAKE_INSTALL_PREFIX={}", prefix.display()))
        .arg(format!("-DCMAKE_SYSTEM_PROCESSOR={}", arch.cpu_family))
        .args(["C", "CXX", "ASM"].map(|lang| format!("-DCMAKE_{lang}_COMPILER=clang")))
        .args(["C", "CXX", "ASM"].map(|lang| format!("-DCMAKE_{lang}_FLAGS={flags}")))
        .args(CMAKE_OPTIONS);
    // Голое имя CMake читает как путь от каталога запуска, а архиватор из `PATH` он сам находит.
    if Path::new(archiver).is_absolute() {
        configure.arg(format!("-DCMAKE_AR={archiver}"));
    }
    shell::run(
        &mut configure,
        &format!("cmake configure for {}", arch.name),
    )?;

    let mut install = Command::new("cmake");
    install
        .arg("--build")
        .arg(&build)
        .args(["--target", "install"]);
    shell::run(&mut install, &format!("cmake install for {}", arch.name))?;

    let config_site = prefix.join(CONFIG_SITE);
    let mut config = fs::read_to_string(&config_site)
        .with_context(|| format!("Failed to read {}", config_site.display()))?;
    // Без макроса libc++ отсчитывает сроки ожиданий по `system_clock`, а `CLOCK_REALTIME` до
    // установки часов реального времени отказывает.
    config.push_str(COND_CLOCKWAIT_DEFINE);
    fs::write(&config_site, config)
        .with_context(|| format!("Failed to write {}", config_site.display()))?;

    let cxxflags = prefix.join(CXXFLAGS_FILE);
    let text = format!(
        "-nostdinc++ -isystem {} -D_GNU_SOURCE",
        include.join("c++").join("v1").display()
    );
    fs::write(&cxxflags, text).with_context(|| format!("Failed to write {}", cxxflags.display()))
}

fn cross_file_text(arch: &ArchFlags, archiver: &str) -> String {
    // `-nostdlib` нужен только пробной компоновке meson: библиотеку собирают без среды хоста.
    let compiler = ["clang"]
        .iter()
        .chain(&cflags(arch))
        .chain(&["-nostdlib"])
        .map(|word| format!("'{word}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "[binaries]\n\
         c = [{compiler}]\n\
         c_ld = 'lld'\n\
         ar = '{archiver}'\n\
         \n\
         [host_machine]\n\
         system = 'none'\n\
         cpu_family = '{family}'\n\
         cpu = '{family}'\n\
         endian = 'little'\n\
         \n\
         [properties]\n\
         skip_sanity_check = true\n",
        family = arch.cpu_family,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clang_major_comes_from_the_dumped_version() {
        assert_eq!(clang_major("18.1.3"), Some(18));
        assert_eq!(clang_major("19"), Some(19));
        assert_eq!(clang_major("clang"), None);
        assert!(check_clang("14.0.6").is_err());
    }

    #[test]
    fn the_stamp_follows_the_llvm_release() {
        let other = Pin::Archive {
            url: "https://example.invalid/llvm-project.src.tar.xz",
            sha256: "0",
            format: Format::TarXz,
            members: &[],
        };
        let stamp = |llvm| stamp_for("18.1.3", "cmake version 3.28.3", "ar", b"", llvm, &[]);
        assert_ne!(stamp(&LLVM_RELEASE), stamp(&other));
    }

    #[test]
    fn a_missing_llvm_ar_falls_back_to_ar() {
        assert_eq!(archiver_from("/usr/bin/llvm-ar"), "/usr/bin/llvm-ar");
        assert_eq!(archiver_from("llvm-ar"), "ar");
    }
}
