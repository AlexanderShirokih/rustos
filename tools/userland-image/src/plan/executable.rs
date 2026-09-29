use anyhow::{Result, anyhow, bail, ensure};

use super::{PlanProgram, source::ComponentEntry};

const IDENTITY_SEPARATOR: &str = "::";

pub(super) fn boot_program<'a>(programs: &[PlanProgram<'a>]) -> Result<PlanProgram<'a>> {
    let boot: Vec<PlanProgram<'a>> = programs
        .iter()
        .copied()
        .filter(|program| program.boot)
        .collect();
    let [boot] = boot[..] else {
        bail!(
            "boot plan requires one boot program in the image; found {}",
            boot.len()
        );
    };
    if let Some(requirement) = boot.manifest()?.requirements().next() {
        bail!(
            "boot program '{}' declares requirement '{}'; a program without a parent takes none",
            boot.bin,
            requirement.name()
        );
    }
    Ok(boot)
}

pub(super) fn resolve_executables<'a>(
    entries: &[&ComponentEntry],
    programs: &[PlanProgram<'a>],
) -> Result<Vec<PlanProgram<'a>>> {
    entries
        .iter()
        .map(|component| resolve_executable(&component.executable, programs))
        .collect()
}

fn resolve_executable<'a>(
    executable: &str,
    programs: &[PlanProgram<'a>],
) -> Result<PlanProgram<'a>> {
    ensure!(
        !executable.starts_with('/'),
        "boot plan component names executable '{executable}' by its image path; \
         name the program identity instead"
    );
    let missing =
        || anyhow!("boot plan names executable '{executable}' that the image does not contain");
    if let Some((package, bin)) = executable.split_once(IDENTITY_SEPARATOR) {
        ensure!(
            !package.is_empty() && !bin.is_empty(),
            "boot plan names executable '{executable}' whose qualified form must name \
             a package and a bin target as '<package>{IDENTITY_SEPARATOR}<bin>'"
        );
        return programs
            .iter()
            .copied()
            .find(|program| program.package == package && program.bin == bin)
            .ok_or_else(missing);
    }
    let matched: Vec<PlanProgram<'a>> = programs
        .iter()
        .copied()
        .filter(|program| program.bin == executable)
        .collect();
    match matched.as_slice() {
        [single] => Ok(*single),
        [] => Err(missing()),
        several => {
            let mut packages: Vec<&str> = several.iter().map(|program| program.package).collect();
            packages.sort_unstable();
            bail!(
                "boot plan names executable '{executable}' that {} packages of the image declare; \
                 name it '<package>{IDENTITY_SEPARATOR}{executable}' with one of: {}",
                several.len(),
                packages.join(", ")
            )
        }
    }
}
