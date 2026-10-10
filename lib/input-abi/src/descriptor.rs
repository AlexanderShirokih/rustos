//! Дескриптор устройства ввода и его проверка.

use ipc::{
    IntoWireValue, ReceivedResources, WireTyped, WireValue,
    schema::WireType,
    wire::{IpcError, MessageBuf},
};

use crate::{
    packed::Packed,
    record::MESSAGE_RECORDS_MAX,
    usage::{USAGE_SIZE, Usage},
};

/// Наибольшая длина имени устройства в байтах.
pub const DEVICE_NAME_MAX: usize = 128;

/// Наибольшее число клавиш устройства.
pub const KEYS_MAX: usize = 256;

/// Наибольшее число осей устройства и наибольшее число осей контакта.
pub const AXES_MAX: usize = 8;

/// Размер объявления оси в байтах.
pub const AXIS_DECL_SIZE: usize = 16;

/// Наибольшее значение `max_contacts` дескриптора.
pub const CONTACTS_MAX: u16 = 16;

const MODE_ABSOLUTE: u8 = 1;
const MODE_RELATIVE: u8 = 2;

ipc::wire_enum! {
    /// Привязка абсолютных осей `X` и `Y`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Binding: u8 {
        /// Абсолютные `X` и `Y` указывают точку экрана.
        Screen = 1,
        /// Абсолютные `X` и `Y` не связаны с экраном или отсутствуют.
        None = 2,
    }
}

/// Режим оси.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxisMode {
    /// Ось хранит значение в диапазоне `min..=max`.
    Absolute { min: i32, max: i32 },
    /// Ось передаёт приращение.
    Relative,
}

/// Объявление оси.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AxisDecl {
    pub usage: Usage,
    pub mode: AxisMode,
}

/// Список объявлений осей в раскладке поля дескриптора.
///
/// Объявление занимает [`AXIS_DECL_SIZE`] байт little-endian: страница usage `u16`, номер usage
/// `u16`, режим `u8`, три байта резерва, `min` `i32`, `max` `i32`. У относительной оси `min` и
/// `max` равны нулю. Индекс объявления в списке - код оси в записях `Axis`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AxisList {
    decls: Packed<AXIS_DECL_SIZE, AXES_MAX>,
}

/// Список usage клавиш в раскладке поля дескриптора.
///
/// Usage занимает 4 байта little-endian: страница `u16`, номер `u16`. Индекс usage в списке -
/// код клавиши в записях `Key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyList {
    usages: Packed<USAGE_SIZE, KEYS_MAX>,
}

ipc::wire_value! {
    /// Дескриптор устройства ввода.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DeviceDescriptor<'a> {
        pub name: &'a str,
        pub binding: Binding,
        /// Клавиши в раскладке [`KeyList`].
        pub keys: &'a [u8],
        /// Оси устройства в раскладке [`AxisList`].
        pub device_axes: &'a [u8],
        pub max_contacts: u16,
        /// Оси контакта в раскладке [`AxisList`].
        pub contact_axes: &'a [u8],
    }
}

/// Проверенный дескриптор устройства.
///
/// Отдаёт usage клавиш и объявления осей по индексу, которым на них ссылаются записи.
/// Передаётся полем сообщения в раскладке [`DeviceDescriptor`]. Поле, которое не читается
/// дескриптором либо не проходит [`DeviceDescriptor::layout`], читается как `InvalidValue`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceLayout {
    name: Packed<1, DEVICE_NAME_MAX>,
    binding: Binding,

    keys: [Usage; KEYS_MAX],
    key_count: usize,

    pub(crate) device_axes: AxisSet,
    pub(crate) max_contacts: u16,
    pub(crate) contact_axes: AxisSet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AxisSet {
    decls: [AxisDecl; AXES_MAX],
    len: usize,
}

/// Отказ проверки дескриптора.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DescriptorError {
    /// Список клавиш или объявление оси неверно:
    /// - список осей не разбирается на объявления либо режим оси неизвестен;
    /// - абсолютная ось с `min > max` либо относительная ось с непустым диапазоном;
    /// - список клавиш не разбирается на usage;
    /// - usage повторяется в списке клавиш, среди осей устройства или среди осей контакта;
    /// - ось контакта относительная.
    Malformed,
    /// Части дескриптора противоречат друг другу:
    /// - контакты без осей контакта либо оси контакта без контактов;
    /// - привязка `Screen` без абсолютных осей `X` и `Y` у устройства либо у контакта;
    /// - абсолютная ось `X` или `Y` объявлена и у устройства, и у контакта.
    Inconsistent,
    /// Дескриптор больше пределов модели:
    /// - имя длиннее [`DEVICE_NAME_MAX`] байт либо клавиш больше [`KEYS_MAX`];
    /// - контактов больше [`CONTACTS_MAX`] либо осей больше [`AXES_MAX`];
    /// - наибольший снимок устройства больше [`MESSAGE_RECORDS_MAX`] записей.
    TooLarge,
}

impl AxisDecl {
    /// Абсолютная ось с диапазоном `min..=max`.
    pub const fn absolute(usage: Usage, min: i32, max: i32) -> Self {
        Self {
            usage,
            mode: AxisMode::Absolute { min, max },
        }
    }

    /// Относительная ось.
    pub const fn relative(usage: Usage) -> Self {
        Self {
            usage,
            mode: AxisMode::Relative,
        }
    }

    fn to_bytes(self) -> [u8; AXIS_DECL_SIZE] {
        let (mode, min, max) = match self.mode {
            AxisMode::Absolute { min, max } => (MODE_ABSOLUTE, min, max),
            AxisMode::Relative => (MODE_RELATIVE, 0, 0),
        };
        let [u0, u1, u2, u3] = self.usage.to_bytes();
        let [n0, n1, n2, n3] = min.to_le_bytes();
        let [x0, x1, x2, x3] = max.to_le_bytes();
        [
            u0, u1, u2, u3, mode, 0, 0, 0, n0, n1, n2, n3, x0, x1, x2, x3,
        ]
    }

    fn from_bytes(bytes: [u8; AXIS_DECL_SIZE]) -> Result<Self, DescriptorError> {
        let [p0, p1, i0, i1, mode, _, _, _, range @ ..] = bytes;
        let [n0, n1, n2, n3, x0, x1, x2, x3] = range;
        let usage = Usage::from_bytes([p0, p1, i0, i1]);
        let min = i32::from_le_bytes([n0, n1, n2, n3]);
        let max = i32::from_le_bytes([x0, x1, x2, x3]);

        match mode {
            MODE_ABSOLUTE if min <= max => Ok(Self::absolute(usage, min, max)),
            MODE_RELATIVE if min == 0 && max == 0 => Ok(Self::relative(usage)),
            _ => Err(DescriptorError::Malformed),
        }
    }

    fn is_absolute(self) -> bool {
        matches!(self.mode, AxisMode::Absolute { .. })
    }
}

impl AxisList {
    /// Пустой список.
    pub const fn new() -> Self {
        Self {
            decls: Packed::new(),
        }
    }

    /// Дописывает объявление оси.
    ///
    /// Отказы:
    /// - `TooLarge` - в списке [`AXES_MAX`] объявлений.
    pub fn push(&mut self, decl: AxisDecl) -> Result<(), DescriptorError> {
        if self.decls.push(decl.to_bytes()) {
            Ok(())
        } else {
            Err(DescriptorError::TooLarge)
        }
    }

    /// Раскладка списка.
    pub fn as_bytes(&self) -> &[u8] {
        self.decls.as_bytes()
    }
}

impl Default for AxisList {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyList {
    /// Пустой список.
    pub const fn new() -> Self {
        Self {
            usages: Packed::new(),
        }
    }

    /// Дописывает клавишу.
    ///
    /// Отказы:
    /// - `TooLarge` - в списке [`KEYS_MAX`] клавиш.
    pub fn push(&mut self, usage: Usage) -> Result<(), DescriptorError> {
        if self.usages.push(usage.to_bytes()) {
            Ok(())
        } else {
            Err(DescriptorError::TooLarge)
        }
    }

    /// Раскладка списка.
    pub fn as_bytes(&self) -> &[u8] {
        self.usages.as_bytes()
    }
}

impl Default for KeyList {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> DeviceDescriptor<'a> {
    /// Дескриптор в раскладке поля. `None`, если имя длиннее [`DEVICE_NAME_MAX`] байт.
    pub fn new(
        name: &'a str,
        binding: Binding,
        keys: &'a KeyList,
        device_axes: &'a AxisList,
        max_contacts: u16,
        contact_axes: &'a AxisList,
    ) -> Option<Self> {
        if name.len() > DEVICE_NAME_MAX {
            return None;
        }

        Some(Self {
            name,
            binding,
            keys: keys.as_bytes(),
            device_axes: device_axes.as_bytes(),
            max_contacts,
            contact_axes: contact_axes.as_bytes(),
        })
    }

    /// Проверяет дескриптор.
    ///
    /// Отказы - варианты [`DescriptorError`].
    pub fn layout(&self) -> Result<DeviceLayout, DescriptorError> {
        if self.name.len() > DEVICE_NAME_MAX {
            return Err(DescriptorError::TooLarge);
        }

        let (keys, key_count) = parse_keys(self.keys)?;
        let device_axes = AxisSet::parse(self.device_axes)?;
        let contact_axes = AxisSet::parse(self.contact_axes)?;

        if contact_axes.iter().any(|decl| !decl.is_absolute()) {
            return Err(DescriptorError::Malformed);
        }

        if self.max_contacts > CONTACTS_MAX {
            return Err(DescriptorError::TooLarge);
        }

        if (self.max_contacts == 0) != contact_axes.is_empty() {
            return Err(DescriptorError::Inconsistent);
        }

        let absolute_on =
            |set: &AxisSet, usage: Usage| set.find(usage).is_some_and(AxisDecl::is_absolute);
        let position = [Usage::X, Usage::Y];
        if position
            .iter()
            .any(|&axis| absolute_on(&device_axes, axis) && absolute_on(&contact_axes, axis))
        {
            return Err(DescriptorError::Inconsistent);
        }

        let point_on = |set: &AxisSet| position.iter().all(|&axis| absolute_on(set, axis));
        if self.binding == Binding::Screen && !point_on(&device_axes) && !point_on(&contact_axes) {
            return Err(DescriptorError::Inconsistent);
        }

        let layout = DeviceLayout {
            name: Packed::from_bytes(self.name.as_bytes()).ok_or(DescriptorError::TooLarge)?,
            binding: self.binding,
            keys,
            key_count,
            device_axes,
            max_contacts: self.max_contacts,
            contact_axes,
        };
        if layout.snapshot_records_max() > MESSAGE_RECORDS_MAX {
            return Err(DescriptorError::TooLarge);
        }
        Ok(layout)
    }
}

impl<'a, R: ReceivedResources> WireValue<'a, R> for DeviceLayout {
    type Owned = Self;

    fn from_field(data: &'a [u8], frame: &mut R) -> Result<Self, IpcError> {
        let descriptor = DeviceDescriptor::from_field(data, frame)?;
        descriptor.layout().map_err(|_| IpcError::InvalidValue)
    }
}

impl<R: ReceivedResources> IntoWireValue<R> for DeviceLayout {
    type Read = Self;

    fn write_into(self, frame: &mut MessageBuf<R>, field_id: u8) -> Result<(), IpcError> {
        let mut keys = KeyList::new();
        for &usage in self.keys.get(..self.key_count).unwrap_or_default() {
            keys.usages.push(usage.to_bytes());
        }
        let device_axes = self.device_axes.to_list();
        let contact_axes = self.contact_axes.to_list();

        // Имя и списки взяты из проверенного дескриптора, и сборка отказывает только на
        // испорченной раскладке.
        let name =
            core::str::from_utf8(self.name.as_bytes()).map_err(|_| IpcError::InvalidValue)?;
        let descriptor = DeviceDescriptor::new(
            name,
            self.binding,
            &keys,
            &device_axes,
            self.max_contacts,
            &contact_axes,
        )
        .ok_or(IpcError::InvalidValue)?;
        descriptor.write_into(frame, field_id)
    }
}

/// Поля раскладки [`DeviceDescriptor`] с ёмкостями пределов модели.
impl WireTyped for DeviceLayout {
    const WIRE_TYPE: WireType = WireType::Aggregate(&[
        WireType::BoundedStr(DEVICE_NAME_MAX),
        <Binding as WireTyped>::WIRE_TYPE,
        WireType::BoundedBytes(KEYS_MAX * USAGE_SIZE),
        WireType::BoundedBytes(AXES_MAX * AXIS_DECL_SIZE),
        <u16 as WireTyped>::WIRE_TYPE,
        WireType::BoundedBytes(AXES_MAX * AXIS_DECL_SIZE),
    ]);
}

impl DeviceLayout {
    /// Привязка абсолютных осей `X` и `Y`.
    pub fn binding(&self) -> Binding {
        self.binding
    }

    /// Usage клавиши с индексом `index`.
    pub fn key(&self, index: u16) -> Option<Usage> {
        self.keys
            .get(..self.key_count)?
            .get(usize::from(index))
            .copied()
    }

    /// Объявление оси устройства с индексом `index`.
    pub fn device_axis(&self, index: u16) -> Option<AxisDecl> {
        self.device_axes.get(index)
    }

    /// Объявление оси контакта с индексом `index`.
    pub fn contact_axis(&self, index: u16) -> Option<AxisDecl> {
        self.contact_axes.get(index)
    }

    /// Наибольшее число записей снимка устройства вместе с записью `Frame`.
    pub fn snapshot_records_max(&self) -> usize {
        let absolute_axes = self
            .device_axes
            .iter()
            .filter(|decl| decl.is_absolute())
            .count();
        let per_contact = 1 + self.contact_axes.len;
        self.key_count + absolute_axes + usize::from(self.max_contacts) * per_contact + 1
    }
}

impl AxisSet {
    fn parse(bytes: &[u8]) -> Result<Self, DescriptorError> {
        let (decls, rest) = bytes.as_chunks();
        if !rest.is_empty() {
            return Err(DescriptorError::Malformed);
        }

        let mut set = Self {
            decls: [AxisDecl::relative(Usage::new(0, 0)); AXES_MAX],
            len: 0,
        };
        for &raw in decls {
            let decl = AxisDecl::from_bytes(raw)?;
            if set.find(decl.usage).is_some() {
                return Err(DescriptorError::Malformed);
            }
            let slot = set
                .decls
                .get_mut(set.len)
                .ok_or(DescriptorError::TooLarge)?;
            *slot = decl;
            set.len += 1;
        }
        Ok(set)
    }

    fn to_list(self) -> AxisList {
        let mut list = AxisList::new();
        for decl in self.iter() {
            list.decls.push(decl.to_bytes());
        }
        list
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = AxisDecl> + '_ {
        self.decls.iter().take(self.len).copied()
    }

    fn get(&self, index: u16) -> Option<AxisDecl> {
        self.iter().nth(usize::from(index))
    }

    fn find(&self, usage: Usage) -> Option<AxisDecl> {
        self.iter().find(|decl| decl.usage == usage)
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }
}

fn parse_keys(bytes: &[u8]) -> Result<([Usage; KEYS_MAX], usize), DescriptorError> {
    let (usages, rest): (&[[u8; USAGE_SIZE]], _) = bytes.as_chunks();
    if !rest.is_empty() {
        return Err(DescriptorError::Malformed);
    }

    if usages.len() > KEYS_MAX {
        return Err(DescriptorError::TooLarge);
    }

    let mut keys = [Usage::new(0, 0); KEYS_MAX];
    for (index, (slot, &raw)) in keys.iter_mut().zip(usages).enumerate() {
        if usages
            .get(index + 1..)
            .is_some_and(|tail| tail.contains(&raw))
        {
            return Err(DescriptorError::Malformed);
        }
        *slot = Usage::from_bytes(raw);
    }
    Ok((keys, usages.len()))
}
