use anyhow::{Result, ensure};
use bootstrap_abi::image;
use component_abi::{ComponentManifest, PROTOCOL_ID_MAX, RequirementMode};
use ipc::Protocol as _;
use syscall::{HANDLE_TABLE_CAPACITY, IPC_BUFFER_MAX_CAPS, MAX_SEGMENTS_PER_IMG};

const BASE: usize = 7;
const LAUNCH: usize = MAX_SEGMENTS_PER_IMG + 3;
const ROOTKEEPER_THREADS: usize = 2;
const FRAME: usize = ROOTKEEPER_THREADS * (1 + IPC_BUFFER_MAX_CAPS);

pub(super) const ROOTKEEPER_HANDLES: usize = BASE + LAUNCH + FRAME;
pub(super) const CHILD_HANDLES: usize = 3;
pub(super) const SEARCH_ROOTS: usize = 1;

pub(super) fn validate_composition(
    names: &[&str],
    declarations: &[ComponentManifest<'_>],
) -> Result<()> {
    for (name, declared) in names.iter().zip(declarations) {
        validate_component(name, declared)?;
    }
    Ok(())
}

fn validate_component(name: &str, declared: &ComponentManifest<'_>) -> Result<()> {
    for export in declared.exports() {
        let protocol = export.protocol();
        ensure!(
            protocol.len() <= PROTOCOL_ID_MAX,
            "boot plan component '{name}' exports protocol '{protocol}' of {} bytes; \
             a protocol id holds at most {PROTOCOL_ID_MAX}",
            protocol.len()
        );
    }
    Ok(())
}

pub(super) fn rootkeeper_handles(declarations: &[ComponentManifest<'_>]) -> usize {
    // `BASE` покрывает порт `Bootstrap`, сессию `Hardware`, ресурс, регионы образа, порты
    // `ComponentStartup` и журнала и поток relay. `LAUNCH` покрывает один запуск, `FRAME` -
    // принятый кадр на каждый принимающий поток. `CHILD_HANDLES` покрывает процесс, резерв и
    // источник журнала записи. Каждый провайдер требования занимает слот заявки либо копии
    // выдачи записи 0, и регион образа по сессии `Image` - ещё один слот копии выдачи. Требование
    // `all` держит ещё один корень выдачи сессий поиска. Провайдер считается по
    // протоколу экспорта один раз при любом числе его фильтров. Журнал занимает слот копии
    // выдачи, экспорт - слот `accept`.
    let per_component: usize = declarations
        .iter()
        .skip(1)
        .map(|component| {
            let requirements: usize = component
                .requirements()
                .map(|requirement| match requirement.mode() {
                    RequirementMode::One => 1,
                    RequirementMode::All => {
                        SEARCH_ROOTS
                            + declarations
                                .iter()
                                .filter(|provider| {
                                    provider
                                        .exports()
                                        .any(|export| export.protocol() == requirement.protocol())
                                })
                                .count()
                    }
                })
                .sum();
            let image_region = usize::from(component.requirements().any(|requirement| {
                requirement.mode() == RequirementMode::One
                    && requirement.protocol() == image::Protocol::ID
            }));

            CHILD_HANDLES + requirements + image_region + component.exports().count() + 1
        })
        .sum();

    ROOTKEEPER_HANDLES + per_component
}

pub(super) fn validate_rootkeeper_limits(declarations: &[ComponentManifest<'_>]) -> Result<()> {
    let handles = rootkeeper_handles(declarations);
    ensure!(
        handles <= HANDLE_TABLE_CAPACITY as usize,
        "boot plan composition of {} components requires up to {handles} rootkeeper handles; \
         the table holds {HANDLE_TABLE_CAPACITY}",
        declarations.len().saturating_sub(1)
    );
    Ok(())
}
