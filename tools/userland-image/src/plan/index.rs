use anyhow::{Result, bail, ensure};
use syscall::MAX_PROCESS_NAME_LEN;
use userland_image::BootPlan;

use super::{PlanProgram, executable::IDENTITY_SEPARATOR};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct IndexEntry<'a> {
    pub protocol: &'a str,
    pub name: String,
    pub program: &'a str,
}

pub(crate) fn export_index<'a>(
    plan: &BootPlan<'_>,
    programs: &[PlanProgram<'a>],
) -> Result<Vec<IndexEntry<'a>>> {
    let mut index = Vec::new();
    for program in programs {
        let manifest = program.manifest()?;
        if manifest.exports().next().is_none() {
            continue;
        }
        let name = if programs
            .iter()
            .filter(|other| other.bin == program.bin)
            .count()
            == 1
        {
            String::from(program.bin)
        } else {
            format!("{}{IDENTITY_SEPARATOR}{}", program.package, program.bin)
        };
        ensure!(
            name.len() <= MAX_PROCESS_NAME_LEN,
            "export index name '{name}' of program '{}' is longer than \
             {MAX_PROCESS_NAME_LEN} bytes, which a process name holds",
            program.path
        );
        if let Some(instance) = plan
            .components()
            .find(|instance| instance.name() == name && instance.executable() != program.path)
        {
            bail!(
                "export index name '{name}' of program '{}' is the name of a boot plan \
                 instance of program '{}'",
                program.path,
                instance.executable()
            );
        }
        for export in manifest.exports() {
            let protocol = export.protocol();
            ensure!(
                !protocol.is_empty()
                    && !protocol.contains('/')
                    && protocol != "."
                    && protocol != "..",
                "program '{}' exports protocol '{protocol}' that cannot name an export \
                 index directory",
                program.path
            );
            index.push(IndexEntry {
                protocol,
                name: name.clone(),
                program: program.path,
            });
        }
    }
    Ok(index)
}
