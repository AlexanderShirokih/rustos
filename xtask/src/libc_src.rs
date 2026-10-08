//! Подготовка библиотеки C программ userland: picolibc из закреплённого релиза, собранная под
//! каждую архитектуру реестра.
//!
//! Архив релиза ложится в `target/libc-src` и сверяется с закреплённой суммой до распаковки.
//! Сборка каждой архитектуры кладёт в `target/libc-sysroot/<arch>` заголовки, `lib/libc.a`,
//! файл `cflags` с флагами компилятора, под которыми собрана библиотека, и файл `ar` с
//! архиватором. К заголовкам библиотеки добавляется `pthread.h` системного слоя `c-library`.
//!
//! Отпечаток релиза, версии `clang`, пути архиватора, опций сборки, флагов архитектур и
//! содержимого `pthread.h` лежит в `target/libc-sysroot/.stamp`, и повторный вызов с тем же
//! отпечатком ничего не делает. Одновременные вызовы разводит замок `target/libc-src.lock`.
//!
//! Библиотека собирается многопоточной, без своей кучи, `crt0` и semihosting, с консолью поверх
//! `read`/`write`, замком на каждый вызов ввода-вывода и конструкторами через
//! `.init_array`/`.fini_array`. Адрес `errno` отдаёт функция `__errno_location` системного слоя.
//! Замки библиотеки даёт системный слой, и заглушки замков из архива удаляются.
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
    layers::{ArchEntry, arch_registry},
    shell,
};

const VERSION: &str = "1.8.12";

const ARCHIVE_SHA256: &str = "64e8c412e1c40fa6eb1a72d2b5cdbcbfe6ceca4cbea454edbad54557ffc747fa";

const RELEASES_URL: &str = "https://github.com/picolibc/picolibc/releases/download";

const MIN_CLANG_MAJOR: u32 = 18;

const SOURCE_DIR: &str = "target/libc-src";

const SYSROOT_DIR: &str = "target/libc-sysroot";

const LOCK: &str = "target/libc-src.lock";

const CFLAGS_FILE: &str = "cflags";

const ARCHIVER_FILE: &str = "ar";

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

struct ArchFlags {
    name: &'static str,
    cpu_family: &'static str,
    cflags: &'static [&'static str],
}

const ARCH_FLAGS: &[ArchFlags] = &[ArchFlags {
    name: "aarch64",
    cpu_family: "aarch64",
    cflags: &[
        "--target=aarch64-none-elf",
        "-march=armv8-a",
        "-mstrict-align",
    ],
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
    let threads_header = root.join(THREADS_HEADER);
    let header = fs::read(&threads_header)
        .with_context(|| format!("Failed to read {}", threads_header.display()))?;

    let sysroot = root.join(SYSROOT_DIR);
    let stamp_path = sysroot.join(".stamp");
    let stamp = stamp_for(&clang, &archiver, &header, &archs);
    if !force && fs::read_to_string(&stamp_path).ok().as_deref() == Some(stamp.as_str()) {
        return Ok(sysroot);
    }

    let source_dir = root.join(SOURCE_DIR);
    let sources = unpack(&source_dir)?;
    if sysroot.exists() {
        fs::remove_dir_all(&sysroot)
            .with_context(|| format!("Failed to clear {}", sysroot.display()))?;
    }
    for arch in &archs {
        let prefix = sysroot.join(arch.name);
        build_arch(arch, &archiver, &sources, &source_dir, &prefix)?;
        install_threads_header(&header, &prefix)?;
    }
    fs::write(&stamp_path, &stamp)
        .with_context(|| format!("Failed to write {}", stamp_path.display()))?;

    println!(
        "libc-src: picolibc {VERSION} built under {}",
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

fn stamp_for(clang: &str, archiver: &str, header: &[u8], archs: &[&ArchFlags]) -> String {
    let mut stamp = format!(
        "picolibc {VERSION} {ARCHIVE_SHA256}\nclang {clang}\narchiver {archiver}\npthread.h {:x}\n",
        Sha256::digest(header)
    );
    stamp.push_str(&MESON_OPTIONS.join(" "));
    stamp.push('\n');
    for arch in archs {
        // Запись в `String` не отказывает.
        let _ = writeln!(stamp, "{} {}", arch.name, arch.cflags.join(" "));
    }
    stamp
}

fn archive_name() -> String {
    format!("picolibc-{VERSION}.tar.xz")
}

fn unpack(source_dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(source_dir)
        .with_context(|| format!("Failed to create {}", source_dir.display()))?;
    let archive = source_dir.join(archive_name());
    if !archive.is_file() {
        download(&archive)?;
    }
    let bytes =
        fs::read(&archive).with_context(|| format!("Failed to read {}", archive.display()))?;
    verify_sha256(&bytes, ARCHIVE_SHA256).with_context(|| {
        format!(
            "{} does not match the pinned release; delete it to download again",
            archive.display()
        )
    })?;

    let sources = source_dir.join(format!("picolibc-{VERSION}"));
    if sources.exists() {
        fs::remove_dir_all(&sources)
            .with_context(|| format!("Failed to clear {}", sources.display()))?;
    }
    let mut command = Command::new("tar");
    command.arg("-xJf").arg(&archive).arg("-C").arg(source_dir);
    shell::run(&mut command, &format!("tar -xJf {}", archive.display()))?;
    Ok(sources)
}

fn download(archive: &Path) -> Result<()> {
    let url = format!("{RELEASES_URL}/{VERSION}/{}", archive_name());
    let partial = archive.with_extension("part");
    let mut command = Command::new("curl");
    command
        .args(["--proto", "=https", "--tlsv1.2", "-sSfL", "-o"])
        .arg(&partial)
        .arg(&url);
    shell::run(&mut command, &format!("curl {url}")).with_context(|| {
        format!(
            "Failed to download picolibc; put {url} at {} by hand",
            archive.display()
        )
    })?;
    fs::rename(&partial, archive)
        .with_context(|| format!("Failed to move the archive to {}", archive.display()))
}

fn verify_sha256(bytes: &[u8], expected: &str) -> Result<()> {
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual != expected {
        bail!("SHA-256 mismatch: expected {expected}, got {actual}");
    }
    Ok(())
}

fn build_arch(
    arch: &ArchFlags,
    archiver: &str,
    sources: &Path,
    source_dir: &Path,
    prefix: &Path,
) -> Result<()> {
    let build = source_dir.join(format!("build-{}", arch.name));
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

    let cflags = prefix.join(CFLAGS_FILE);
    fs::write(&cflags, arch.cflags.join(" "))
        .with_context(|| format!("Failed to write {}", cflags.display()))?;
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

fn cross_file_text(arch: &ArchFlags, archiver: &str) -> String {
    // `-nostdlib` нужен только пробной компоновке meson: библиотеку собирают без среды хоста.
    let compiler = ["clang"]
        .iter()
        .chain(arch.cflags)
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
    fn a_mismatching_archive_names_both_sums() {
        let expected = "0".repeat(64);
        let error = verify_sha256(b"picolibc", &expected)
            .unwrap_err()
            .to_string();
        assert!(error.contains(&expected), "{error}");
        assert!(
            error.contains(&format!("{:x}", Sha256::digest(b"picolibc"))),
            "{error}"
        );
    }

    #[test]
    fn the_clang_major_comes_from_the_dumped_version() {
        assert_eq!(clang_major("18.1.3"), Some(18));
        assert_eq!(clang_major("19"), Some(19));
        assert_eq!(clang_major("clang"), None);
        assert!(check_clang("14.0.6").is_err());
    }

    #[test]
    fn a_missing_llvm_ar_falls_back_to_ar() {
        assert_eq!(archiver_from("/usr/bin/llvm-ar"), "/usr/bin/llvm-ar");
        assert_eq!(archiver_from("llvm-ar"), "ar");
    }
}
