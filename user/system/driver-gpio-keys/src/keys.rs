//! Узел `gpio-keys`: имя устройства ввода и клавиши доступных потомков.

use alloc::vec::Vec;

use input_abi::Usage;
use platform_abi::{Description, DescriptionError, DeviceId, Node};

const NAME_DEFAULT: &str = "gpio-keys";

const EV_KEY: u32 = 1;

const DEBOUNCE_DEFAULT_MS: u32 = 5;
const NS_PER_MS: u64 = 1_000_000;

/// Устройство ввода узла `gpio-keys`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keys<'d> {
    /// Имя устройства: `label` узла, без него - `gpio-keys`.
    pub name: &'d str,
    /// Клавиши устройства в порядке первого появления у потомков.
    pub usages: Vec<Usage>,
    /// Клавиши доступных потомков в порядке описания.
    pub keys: Vec<Key>,
}

/// Клавиша потомка узла `gpio-keys`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    /// Путь потомка, свойство `gpios` которого описывает линию клавиши.
    pub node: DeviceId,
    /// Индекс клавиши кода `linux,code` в [`Keys::usages`].
    pub usage: usize,
    /// Длина окна антидребезга в наносекундах.
    pub debounce: u64,
}

/// Отказ разбора узла `gpio-keys`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeysError {
    /// Значение свойства либо путь потомка не читаются.
    Description(DescriptionError),
    /// У узла нет доступных потомков.
    NoKeys,
    /// У потомка нет свойства `gpios`.
    NoLine,
    /// У потомка нет свойства `linux,code`.
    NoCode,
    /// Код `linux,code` не переводится в клавишу.
    UnknownCode(u32),
    /// `linux,input-type` потомка отличен от `EV_KEY`.
    InputType(u32),
}

impl From<DescriptionError> for KeysError {
    fn from(error: DescriptionError) -> Self {
        Self::Description(error)
    }
}

/// Возвращает устройство ввода узла `node` с клавишей на каждого доступного потомка. Потомки с
/// одним кодом `linux,code` делят клавишу устройства.
///
/// Отказы:
/// - `Description` - значение свойства либо путь потомка не читаются;
/// - `NoKeys` - узел недоступен либо у него нет доступных потомков;
/// - `NoLine`, `NoCode` - у потомка нет `gpios` либо `linux,code`;
/// - `UnknownCode` - код потомка не переводится в клавишу;
/// - `InputType` - `linux,input-type` потомка отличен от `EV_KEY`.
pub fn keys<'d>(description: &'d Description<'_>, node: &DeviceId) -> Result<Keys<'d>, KeysError> {
    let name = description
        .node(node)
        .map(|node| node.string("label"))
        .transpose()?
        .flatten()
        .unwrap_or(NAME_DEFAULT);

    let mut usages = Vec::new();
    let keys = description
        .children(node)
        .map(|child| {
            let child = child?;
            key(child.id, &child.node, &mut usages)
        })
        .collect::<Result<Vec<_>, _>>()?;

    if keys.is_empty() {
        return Err(KeysError::NoKeys);
    }

    Ok(Keys { name, usages, keys })
}

fn key(id: DeviceId, node: &Node<'_>, usages: &mut Vec<Usage>) -> Result<Key, KeysError> {
    if node.bytes("gpios").is_none() {
        return Err(KeysError::NoLine);
    }

    let code = node.u32("linux,code")?.ok_or(KeysError::NoCode)?;
    let usage = Usage::from_linux_key(code).ok_or(KeysError::UnknownCode(code))?;

    let input_type = node.u32("linux,input-type")?.unwrap_or(EV_KEY);
    if input_type != EV_KEY {
        return Err(KeysError::InputType(input_type));
    }

    let debounce = node
        .u32("debounce-interval")?
        .unwrap_or(DEBOUNCE_DEFAULT_MS);

    let usage = usages
        .iter()
        .position(|known| *known == usage)
        .unwrap_or_else(|| {
            let index = usages.len();
            usages.push(usage);
            index
        });

    Ok(Key {
        node: id,
        usage,
        debounce: u64::from(debounce).saturating_mul(NS_PER_MS),
    })
}
