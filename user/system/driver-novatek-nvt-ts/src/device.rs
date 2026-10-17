//! Описание тачскрина для хаба ввода с диапазонами осей контактов.

use input_abi::{AxisDecl, AxisList, Binding, DescriptorError, DeviceDescriptor, KeyList, Usage};
use platform_abi::{DescriptionError, Node};

use crate::parameters::Parameters;

const DEVICE_NAME: &str = "nt36672a-ts";

const SIZE_X: &str = "touchscreen-size-x";
const SIZE_Y: &str = "touchscreen-size-y";

/// Тачскрин с привязкой `Screen`. Каждый контакт имеет абсолютные оси `X` и `Y` с минимумом
/// 0. Клавиши и оси устройства не объявляются.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    keys: KeyList,
    axes: AxisList,
    contact_axes: AxisList,
    max_contacts: u16,
    max_x: i32,
    max_y: i32,
}

/// Ошибка создания устройства тачскрина.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceError {
    /// Ошибка чтения свойства `touchscreen-size-x` либо `touchscreen-size-y`.
    Description(DescriptionError),
    /// Размер оси равен 0 либо её максимум `size - 1` не помещается в `i32`.
    Size(u32),
    /// Ошибка добавления оси контакта.
    Axes(DescriptorError),
}

impl From<DescriptionError> for DeviceError {
    fn from(error: DescriptionError) -> Self {
        Self::Description(error)
    }
}

impl Device {
    /// Создаёт устройство узла `node` с числом контактов из параметров чипа `parameters`.
    /// Максимум оси `X` равен `touchscreen-size-x - 1`, максимум оси `Y` -
    /// `touchscreen-size-y - 1`. При отсутствии свойства используется соответствующий
    /// размер из параметров чипа минус 1.
    ///
    /// Отказы:
    /// - `Description` - ошибка чтения свойства размера;
    /// - `Size` - размер равен 0 либо максимум `size - 1` не помещается в `i32`;
    /// - `Axes` - ошибка добавления оси контакта.
    pub fn new(node: &Node<'_>, parameters: Parameters) -> Result<Self, DeviceError> {
        let width = node.u32(SIZE_X)?.unwrap_or(u32::from(parameters.width()));
        let height = node.u32(SIZE_Y)?.unwrap_or(u32::from(parameters.height()));
        let max_x = axis_max(width)?;
        let max_y = axis_max(height)?;

        let mut contact_axes = AxisList::new();
        for decl in [
            AxisDecl::absolute(Usage::X, 0, max_x),
            AxisDecl::absolute(Usage::Y, 0, max_y),
        ] {
            contact_axes.push(decl).map_err(DeviceError::Axes)?;
        }

        Ok(Self {
            keys: KeyList::new(),
            axes: AxisList::new(),
            contact_axes,
            max_contacts: u16::from(parameters.max_touches()),
            max_x,
            max_y,
        })
    }

    /// Дескриптор устройства для объявления хабу.
    pub fn descriptor(&self) -> Option<DeviceDescriptor<'_>> {
        DeviceDescriptor::new(
            DEVICE_NAME,
            Binding::Screen,
            &self.keys,
            &self.axes,
            self.max_contacts,
            &self.contact_axes,
        )
    }

    /// Наибольшее число одновременных касаний устройства.
    pub const fn max_contacts(&self) -> u16 {
        self.max_contacts
    }

    /// Максимум оси `X` контакта.
    pub const fn max_x(&self) -> i32 {
        self.max_x
    }

    /// Максимум оси `Y` контакта.
    pub const fn max_y(&self) -> i32 {
        self.max_y
    }
}

fn axis_max(size: u32) -> Result<i32, DeviceError> {
    size.checked_sub(1)
        .and_then(|max| i32::try_from(max).ok())
        .ok_or(DeviceError::Size(size))
}
