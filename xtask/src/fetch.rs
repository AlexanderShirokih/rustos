//! Загрузка закреплённого стороннего кода: архив релиза со сверкой суммы sha256 и дерево git
//! на заданном коммите.
//!
//! Запись раскладывается в свой каталог целиком. Отпечаток пина лежит в `<dir>/.pin`, и
//! подготовка без `force` с тем же пином оставляет каталог как есть. Одновременные подготовки
//! одной записи разводит замок `<dir>.lock`.
//!
//! Архив лежит рядом с каталогом под именем файла из URL и сверяется с суммой до распаковки.
//! Пересоздание каталога распаковывает уже загруженный архив. При отказе загрузки ошибка
//! называет URL и путь, по которому архив кладётся вручную. Дерево git сверяется с пином по
//! хешу полученного коммита.

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::{context::lock_tree, shell};

const PIN_FILE: &str = ".pin";

#[derive(Debug)]
pub(crate) enum Pin {
    Archive {
        url: &'static str,
        sha256: &'static str,
        format: Format,
    },
    GitCommit {
        repo: &'static str,
        commit: &'static str,
    },
}

#[derive(Debug)]
pub(crate) enum Format {
    TarXz,
    Zip,
}

pub(crate) fn materialize(name: &str, dir: &Path, pin: &Pin, force: bool) -> Result<PathBuf> {
    // Вызов, пришедший вторым, ждёт освобождения и получает готовый каталог.
    let _lock = lock_tree(&beside(dir, ".lock"), name)?;

    let pin_path = dir.join(PIN_FILE);
    let fingerprint = format!("{pin:?}");
    if !force && fs::read_to_string(&pin_path).ok().as_deref() == Some(fingerprint.as_str()) {
        return Ok(dir.to_path_buf());
    }

    match pin {
        Pin::Archive {
            url,
            sha256,
            format,
        } => unpack(name, dir, url, sha256, format)?,
        Pin::GitCommit { repo, commit } => checkout(name, dir, repo, commit)?,
    }
    fs::write(&pin_path, &fingerprint)
        .with_context(|| format!("Failed to write {}", pin_path.display()))?;

    println!("{name}: prepared under {}", dir.display());
    Ok(dir.to_path_buf())
}

fn beside(dir: &Path, suffix: &str) -> PathBuf {
    let mut path = OsString::from(dir.as_os_str());
    path.push(suffix);
    PathBuf::from(path)
}

fn recreate(dir: &Path) -> Result<()> {
    if dir.exists() {
        fs::remove_dir_all(dir).with_context(|| format!("Failed to clear {}", dir.display()))?;
    }
    fs::create_dir_all(dir).with_context(|| format!("Failed to create {}", dir.display()))
}

fn unpack(name: &str, dir: &Path, url: &str, sha256: &str, format: &Format) -> Result<()> {
    let file_name = url
        .rsplit('/')
        .next()
        .filter(|file_name| !file_name.is_empty())
        .with_context(|| format!("URL {url} of {name} names no file"))?;
    let archive = dir.with_file_name(file_name);
    if !archive.is_file() {
        download(name, url, &archive)?;
    }
    let bytes =
        fs::read(&archive).with_context(|| format!("Failed to read {}", archive.display()))?;
    verify_sha256(&bytes, sha256).with_context(|| {
        format!(
            "{} does not match the pinned {name}; delete it to download again",
            archive.display()
        )
    })?;

    recreate(dir)?;
    let mut command = match format {
        Format::TarXz => {
            let mut command = Command::new("tar");
            command.arg("-xJf").arg(&archive).arg("-C").arg(dir);
            command
        }
        Format::Zip => {
            let mut command = Command::new("unzip");
            command.arg("-q").arg(&archive).arg("-d").arg(dir);
            command
        }
    };
    shell::run(&mut command, &format!("unpacking {}", archive.display()))
}

fn download(name: &str, url: &str, archive: &Path) -> Result<()> {
    let partial = archive.with_extension("part");
    let mut command = Command::new("curl");
    command
        .args(["--proto", "=https", "--tlsv1.2", "-sSfL", "-o"])
        .arg(&partial)
        .arg(url);
    shell::run(&mut command, &format!("curl {url}")).with_context(|| {
        format!(
            "Failed to download {name}; put {url} at {} by hand",
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

fn checkout(name: &str, dir: &Path, repo: &str, commit: &str) -> Result<()> {
    // Дерево собирается в соседнем каталоге и заменяет каталог записи после сверки коммита.
    let staging = beside(dir, ".partial");
    recreate(&staging)?;
    shell::run(
        &mut git(&staging, &["init", "--quiet"]),
        &format!("git init for {name}"),
    )?;
    shell::run(
        &mut git(
            &staging,
            &["fetch", "--quiet", "--depth", "1", repo, commit],
        ),
        &format!("git fetch for {name}"),
    )
    .with_context(|| format!("Failed to fetch {name} {commit} from {repo}"))?;
    shell::run(
        &mut git(&staging, &["checkout", "--quiet", "--detach", "FETCH_HEAD"]),
        &format!("git checkout for {name}"),
    )?;
    let head = shell::output(
        &mut git(&staging, &["rev-parse", "HEAD"]),
        &format!("git rev-parse for {name}"),
    )?;
    verify_commit(head.trim(), commit)
        .with_context(|| format!("{repo} gave {name} a commit other than the pinned one"))?;

    if dir.exists() {
        fs::remove_dir_all(dir).with_context(|| format!("Failed to clear {}", dir.display()))?;
    }
    fs::rename(&staging, dir)
        .with_context(|| format!("Failed to move the tree to {}", dir.display()))
}

fn git(dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    // В git-хуке git экспортирует эти переменные для репозитория проекта.
    command
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    command
}

fn verify_commit(actual: &str, expected: &str) -> Result<()> {
    if actual != expected {
        bail!("commit mismatch: expected {expected}, got {actual}");
    }
    Ok(())
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
    fn a_mismatching_commit_names_both_hashes() {
        let expected = "a".repeat(40);
        let actual = "b".repeat(40);
        let error = verify_commit(&actual, &expected).unwrap_err().to_string();
        assert!(error.contains(&expected), "{error}");
        assert!(error.contains(&actual), "{error}");
    }

    #[test]
    fn a_corrupt_archive_stops_before_the_tree_is_replaced() {
        let root = std::env::temp_dir().join(format!("xtask-fetch-corrupt-{}", std::process::id()));
        let dir = root.join("entry");
        let pin = Pin::Archive {
            url: "https://example.invalid/entry.zip",
            sha256: "0",
            format: Format::Zip,
        };
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("kept"), b"").unwrap();
        fs::write(root.join("entry.zip"), b"corrupt").unwrap();

        assert!(materialize("entry", &dir, &pin, false).is_err());
        assert!(dir.join("kept").is_file());

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_tree_with_the_same_pin_is_kept_without_a_download() {
        let root = std::env::temp_dir().join(format!("xtask-fetch-{}", std::process::id()));
        let dir = root.join("entry");
        let pin = Pin::Archive {
            url: "https://example.invalid/entry.zip",
            sha256: "0",
            format: Format::Zip,
        };
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(PIN_FILE), format!("{pin:?}")).unwrap();
        fs::write(dir.join("kept"), b"").unwrap();

        assert_eq!(materialize("entry", &dir, &pin, false).unwrap(), dir);
        assert!(dir.join("kept").is_file());

        fs::remove_dir_all(&root).unwrap();
    }
}
