use std::path::{Path, PathBuf};

/// Слой, которому принадлежит пакет.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Domain {
    /// Крейты ядра.
    Kernel,
    /// Разделяемые библиотеки.
    Library,
    /// Userspace-программы.
    User,
    /// Host-инструменты сборки.
    Tool,
}

impl Domain {
    /// Определяет слой по пути файла от корня репозитория.
    pub fn of(path: &Path) -> Option<Domain> {
        match path.components().next()?.as_os_str().to_str()? {
            "kernel" => Some(Domain::Kernel),
            "lib" => Some(Domain::Library),
            "user" => Some(Domain::User),
            "tools" | "xtask" => Some(Domain::Tool),
            _ => None,
        }
    }
}

/// Паспорт файла, по которому вычисляется область действия правила.
pub struct FileMeta {
    /// Путь от корня репозитория.
    pub path: PathBuf,
    /// Имя пакета, которому принадлежит файл.
    pub package: Option<String>,
    /// Слой пакета.
    pub domain: Option<Domain>,
}

/// Условие на файл.
#[derive(Clone, Debug)]
pub enum Selector {
    /// Расширение имени файла.
    Extension(String),
    /// Расширение имени файла совпадает с одним из перечисленных.
    ExtensionAny(Vec<String>),
    /// Путь начинается с одного из префиксов.
    UnderAny(Vec<String>),
    /// Путь не начинается ни с одного из префиксов.
    NotUnderAny(Vec<String>),
    /// Файл принадлежит одному из пакетов.
    PackageAny(Vec<String>),
    /// Файл не принадлежит ни одному из пакетов.
    NotPackageAny(Vec<String>),
    /// Имя файла совпадает с одним из перечисленных.
    FileNameAny(Vec<String>),
    /// Имя файла не совпадает ни с одним из перечисленных.
    NotFileNameAny(Vec<String>),
    /// Путь не содержит ни одного из перечисленных сегментов.
    NotSegmentAny(Vec<String>),
    /// Пакет принадлежит одному из слоёв.
    DomainAny(Vec<Domain>),
}

/// Конъюнкция условий на файл.
#[derive(Clone, Debug)]
pub struct Scope(pub Vec<Selector>);

/// Ось контекста: производственный код или тестовый.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Context {
    /// Только вне тестовых диапазонов.
    Prod,
    /// Только внутри тестовых диапазонов.
    Test,
    /// Обе стороны.
    Any,
}

/// Ось тел макро-объявлений и макро-вызовов.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MacroBodies {
    /// Находки внутри тел сохраняются.
    Include,
    /// Находки внутри тел отбрасываются.
    Exclude,
}

/// Область действия правила целиком.
#[derive(Clone, Debug)]
pub struct Applies {
    /// Условие на файл.
    pub scope: Scope,
    /// Условие на контекст.
    pub context: Context,
    /// Отношение к телам макросов.
    pub macro_bodies: MacroBodies,
}

impl Applies {
    /// Область по умолчанию: все файлы Rust, любой контекст, без тел макросов.
    pub fn rust() -> Applies {
        Applies {
            scope: Scope::rust(),
            context: Context::Any,
            macro_bodies: MacroBodies::Exclude,
        }
    }
}

impl Scope {
    /// Область всех файлов Rust.
    pub fn rust() -> Scope {
        Scope(vec![Selector::Extension("rs".to_owned())])
    }

    /// Сообщает, удовлетворяет ли файл всем условиям.
    pub fn matches(&self, meta: &FileMeta) -> bool {
        self.0.iter().all(|selector| selector.matches(meta))
    }
}

impl Selector {
    /// Сообщает, удовлетворяет ли файл условию.
    pub fn matches(&self, meta: &FileMeta) -> bool {
        match self {
            Selector::Extension(want) => extension(&meta.path).is_some_and(|got| got == want),
            Selector::ExtensionAny(wants) => {
                extension(&meta.path).is_some_and(|got| wants.iter().any(|want| want == got))
            }
            Selector::UnderAny(prefixes) => under_any(&meta.path, prefixes),
            Selector::NotUnderAny(prefixes) => !under_any(&meta.path, prefixes),
            Selector::PackageAny(names) => in_package(meta, names),
            Selector::NotPackageAny(names) => !in_package(meta, names),
            Selector::FileNameAny(names) => {
                file_name(&meta.path).is_some_and(|got| names.iter().any(|want| want == got))
            }
            Selector::NotFileNameAny(names) => {
                !file_name(&meta.path).is_some_and(|got| names.iter().any(|want| want == got))
            }
            Selector::NotSegmentAny(segments) => !has_segment_any(&meta.path, segments),
            Selector::DomainAny(domains) => meta.domain.is_some_and(|got| domains.contains(&got)),
        }
    }
}

fn extension(path: &Path) -> Option<&str> {
    path.extension().and_then(|raw| raw.to_str())
}

fn file_name(path: &Path) -> Option<&str> {
    path.file_name().and_then(|raw| raw.to_str())
}

fn under_any(path: &Path, prefixes: &[String]) -> bool {
    let text = path.to_string_lossy();
    prefixes
        .iter()
        .any(|prefix| text.starts_with(prefix.as_str()))
}

fn has_segment_any(path: &Path, segments: &[String]) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|got| segments.iter().any(|want| want == got))
    })
}

fn in_package(meta: &FileMeta, names: &[String]) -> bool {
    meta.package
        .as_deref()
        .is_some_and(|got| names.iter().any(|want| want == got))
}
