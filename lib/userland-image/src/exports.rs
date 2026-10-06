#[cfg(any(test, feature = "alloc"))]
use alloc::{format, string::String};

use crate::DecodedImage;

/// Имя каталога индекса экспортов в каталоге плана.
pub const EXPORTS_DIRECTORY: &str = "exports";

/// Индекс экспортов программ образа. На каждый экспорт программы в образе есть запись
/// `<каталог плана>/exports/<протокол>/<имя>`, её содержимое - каталог программы.
#[derive(Debug, Clone, Copy)]
pub struct ExportIndex<'a> {
    image: Option<DecodedImage<'a>>,
    plan: &'a str,
}

/// Запись индекса экспортов одного протокола.
#[derive(Debug, Clone, Copy)]
pub struct IndexedExport<'a> {
    name: &'a str,
    program: &'a [u8],
}

impl<'a> ExportIndex<'a> {
    /// Индекс без записей.
    pub const EMPTY: Self = Self {
        image: None,
        plan: "",
    };

    /// Индекс образа `image`, разложенный в каталоге плана с путём `plan`.
    #[must_use]
    pub const fn new(image: DecodedImage<'a>, plan: &'a str) -> Self {
        Self {
            image: Some(image),
            plan,
        }
    }

    /// Записи индекса протокола `protocol` в порядке записей образа.
    pub fn providers<'p>(
        &self,
        protocol: &'p str,
    ) -> impl Iterator<Item = IndexedExport<'a>> + use<'a, 'p> {
        let directory = plan_directory(self.plan);
        self.image
            .into_iter()
            .flat_map(|image| image.entries())
            .filter_map(move |entry| {
                let name = entry
                    .path()
                    .strip_prefix(directory)?
                    .strip_prefix('/')?
                    .strip_prefix(EXPORTS_DIRECTORY)?
                    .strip_prefix('/')?
                    .strip_prefix(protocol)?
                    .strip_prefix('/')?;
                (!name.is_empty() && !name.contains('/')).then_some(IndexedExport {
                    name,
                    program: entry.data(),
                })
            })
    }
}

impl<'a> IndexedExport<'a> {
    /// Имя записи. Родитель называет им экземпляр, который создаёт по заявке.
    #[must_use]
    pub const fn name(&self) -> &'a str {
        self.name
    }

    /// Каталог программы в образе. `None`, если содержимое записи не UTF-8.
    #[must_use]
    pub fn program(&self) -> Option<&'a str> {
        core::str::from_utf8(self.program).ok()
    }
}

/// Путь записи индекса протокола `protocol` с именем `name` при плане с путём `plan`.
#[cfg(any(test, feature = "alloc"))]
#[must_use]
pub fn export_entry_path(plan: &str, protocol: &str, name: &str) -> String {
    format!(
        "{}/{EXPORTS_DIRECTORY}/{protocol}/{name}",
        plan_directory(plan)
    )
}

fn plan_directory(plan: &str) -> &str {
    plan.rsplit_once('/').map_or("", |(directory, _)| directory)
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;
    use crate::{BOOT_PLAN_PATH, ImageRecord, decode, encode};

    const BOOT_PATH: &str = "/rootkeeper/exec";

    #[test]
    fn providers_of_a_protocol_skip_other_protocols_and_directories() {
        let paths = [
            export_entry_path(BOOT_PLAN_PATH, "Rtc", "driver-pl031"),
            export_entry_path(BOOT_PLAN_PATH, "Rtc", "rtc::driver"),
            export_entry_path(BOOT_PLAN_PATH, "Probe", "export-fixture"),
            export_entry_path(BOOT_PLAN_PATH, "RtcClock", "clock"),
            export_entry_path("/testrunner/plan", "Rtc", "foreign"),
            String::from("/rootkeeper/exports/Rtc/nested/entry"),
            String::from("/rootkeeper/exports/Rtc"),
        ];
        let programs = [
            "/driver-pl031",
            "/rtc",
            "/export-fixture",
            "/clock",
            "/foreign",
            "/nested",
            "/flat",
        ];
        let records: Vec<_> = [ImageRecord {
            path: BOOT_PATH,
            data: b"boot",
        }]
        .into_iter()
        .chain(
            paths
                .iter()
                .zip(programs)
                .map(|(path, program)| ImageRecord {
                    path,
                    data: program.as_bytes(),
                }),
        )
        .collect();
        let bytes = encode(&records, BOOT_PATH).expect("encode image");
        let index = ExportIndex::new(decode(&bytes).expect("decode"), BOOT_PLAN_PATH);

        let providers: Vec<_> = index
            .providers("Rtc")
            .map(|export| (export.name(), export.program()))
            .collect();

        assert_eq!(paths[0], "/rootkeeper/exports/Rtc/driver-pl031");
        assert_eq!(
            providers,
            [
                ("driver-pl031", Some("/driver-pl031")),
                ("rtc::driver", Some("/rtc")),
            ]
        );
        assert_eq!(ExportIndex::EMPTY.providers("Rtc").count(), 0);
    }
}
