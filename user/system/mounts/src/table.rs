//! Таблица начальных монтирований `/mounts/table` userland-образа.
//!
//! Строка - `<путь> <ключ>=<значение>[,<ключ>=<значение>...]`: путь монтирования и атрибуты
//! запроса `FileSystem`. Пустые строки и строки с `#` пропускаются, некорректная пишется в журнал.
//!
//! Строки монтируются одновременно, вложенная ждёт монтирования строки-родителя. Строки одной
//! файловой системы сверх `managed_component::IMPORTER_REQUESTS_PER_EXPORT_MAX` получают `NoSpace`.

use alloc::{string::String, vec::Vec};

use bootstrap_abi::image;
use component_abi::{
    ATTRIBUTE_COUNT_MAX, ATTRIBUTE_KEY_MAX_BYTES, ATTRIBUTE_VALUE_MAX_BYTES, Attribute,
    AttributeSet, Attributes, attributes_len, encode_attributes_into,
};
use component_startup::{Import, session};
use filesystem_abi::{
    FileSystemError, MountEnded, Path, file_system, file_system_mount, namespace,
};
use ipc::CallError;
use kernel_objects::{MemoryRegion, Port, ReadOnlyMapping, Session};
use runtime::{log_fmt, spawn_owned};
use syscall::{Handle, Timeout, kind};

use crate::router::{open, open_mount};

const TABLE_PATH: &str = "/mounts/table";

const FILESYSTEM_REQUIREMENT: &str = "filesystem";

#[derive(Debug)]
enum LineError {
    InvalidPath,
    MissingAttributes,
    InvalidAttribute,
    AttributeLimits,
}

pub(crate) struct MountLine {
    number: usize,
    path: String,
    attributes: AttributeSet,
}

pub(crate) fn read(image: &Import<image::Protocol>) -> Vec<MountLine> {
    let image = match image.open() {
        Ok(image) => image,
        Err(error) => {
            log_fmt(format_args!("mount table skipped: {error}"));
            return Vec::new();
        }
    };
    let client = image.client();
    let Ok(region) = client.region().map(MemoryRegion::from) else {
        log_fmt(format_args!("mount table skipped: image region refused"));
        return Vec::new();
    };
    let Ok(mapping) = ReadOnlyMapping::new(region) else {
        log_fmt(format_args!("mount table skipped: image not mapped"));
        return Vec::new();
    };
    let Ok(decoded) = userland_image::decode(mapping.as_bytes()) else {
        log_fmt(format_args!("mount table skipped: image not decoded"));
        return Vec::new();
    };
    let Some(entry) = decoded.entry(TABLE_PATH) else {
        return Vec::new();
    };
    let Ok(text) = core::str::from_utf8(entry.data()) else {
        log_fmt(format_args!("mount table {TABLE_PATH} is not UTF-8"));
        return Vec::new();
    };

    text.lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let number = index.saturating_add(1);
            match parsed(number, line) {
                Ok(line) => Some(line),
                Err(error) => {
                    log_fmt(format_args!("mount table line {number}: {error:?}"));
                    None
                }
            }
        })
        .collect()
}

pub(crate) fn start(
    lines: Vec<MountLine>,
    parent: Handle<kind::Port>,
    views: &'static Port,
    export: &'static Port,
) {
    let enclosing: Vec<Option<String>> = lines.iter().map(|line| enclosing(&lines, line)).collect();
    for (line, enclosing) in lines.into_iter().zip(enclosing) {
        let number = line.number;
        let started = spawn_owned(move || {
            mount(&line, enclosing.as_deref(), parent, views, export);
            0
        });
        if started.is_err() {
            log_fmt(format_args!("mount table line {number} not started"));
        }
    }
}

fn parsed(number: usize, line: &str) -> Result<MountLine, LineError> {
    let mut words = line.split_whitespace();
    let path = words
        .next()
        .and_then(|path| Path::new(path).ok())
        .ok_or(LineError::InvalidPath)?;
    let text = words.next().ok_or(LineError::MissingAttributes)?;
    if words.next().is_some() {
        return Err(LineError::InvalidAttribute);
    }

    let mut pairs = [Attribute {
        key: &[],
        value: &[],
    }; ATTRIBUTE_COUNT_MAX];
    let mut count = 0;
    for pair in text.split(',') {
        let (key, value) = pair
            .split_once('=')
            .filter(|(key, _)| !key.is_empty())
            .ok_or(LineError::InvalidAttribute)?;
        if key.len() > ATTRIBUTE_KEY_MAX_BYTES || value.len() > ATTRIBUTE_VALUE_MAX_BYTES {
            return Err(LineError::AttributeLimits);
        }
        *pairs.get_mut(count).ok_or(LineError::AttributeLimits)? = Attribute {
            key: key.as_bytes(),
            value: value.as_bytes(),
        };
        count = count.saturating_add(1);
    }
    let pairs = pairs.get(..count).ok_or(LineError::AttributeLimits)?;

    // Пределы проверены выше, и кодирование не нарушает своих утверждений.
    let mut bytes: Vec<u8> = alloc::vec![0; attributes_len(pairs)];
    encode_attributes_into(&mut bytes, pairs);
    // Повтор ключа отвергает разбор набора.
    let attributes = Attributes::decode(&bytes).ok_or(LineError::AttributeLimits)?;

    Ok(MountLine {
        number,
        path: String::from(path.as_str()),
        attributes: AttributeSet::new(attributes),
    })
}

fn mount(
    line: &MountLine,
    enclosing: Option<&str>,
    parent: Handle<kind::Port>,
    views: &Port,
    export: &Port,
) {
    let number = line.number;
    loop {
        if let Some(enclosing) = enclosing
            && let Err(error) = await_mount(enclosing, views)
        {
            log_fmt(format_args!(
                "mount table line {number} {}: {enclosing} not awaited: {error:?}",
                line.path
            ));
        }

        let filesystem = match session::<file_system::Protocol>(
            parent,
            FILESYSTEM_REQUIREMENT,
            line.attributes.as_attributes(),
            Timeout::INFINITE,
        ) {
            Ok(filesystem) => filesystem,
            Err(error) => {
                log_fmt(format_args!(
                    "mount table line {number} {}: filesystem refused: {error:?}",
                    line.path
                ));
                return;
            }
        };
        let session = match open_mount(export) {
            Ok(session) => Session::<file_system_mount::Protocol>::from_handle(session),
            Err(error) => {
                log_fmt(format_args!(
                    "mount table line {number} {}: mount session refused: {error:?}",
                    line.path
                ));
                return;
            }
        };
        match session
            .client()
            .with_wait(Timeout::INFINITE)
            .mount(&line.path, filesystem)
        {
            Ok(MountEnded::FileSystemGone) => {}
            ended => {
                log_fmt(format_args!(
                    "mount table line {number} {} ended: {ended:?}",
                    line.path
                ));
                return;
            }
        }
    }
}

fn enclosing(lines: &[MountLine], line: &MountLine) -> Option<String> {
    let path = Path::new(&line.path).ok()?;
    lines
        .iter()
        .filter(|other| other.path != line.path)
        .filter(|other| {
            Path::new(&other.path).is_ok_and(|other| path.strip_prefix(other).is_some())
        })
        .max_by_key(|other| other.path.len())
        .map(|other| other.path.clone())
}

fn await_mount(path: &str, views: &Port) -> Result<(), CallError<FileSystemError>> {
    let view = open(views, path).map_err(CallError::Remote)?;
    // Разрешение в виде на путь родителя использует монтирования только на этом пути и под ним.
    // Под ним лежат лишь строки, ждущие того же родителя.
    Session::<namespace::Protocol>::from_handle(view)
        .client()
        .with_wait(Timeout::INFINITE)
        .resolve("/")
        .map(drop)
}
