//! Устройства PMIC за арбитром по описанию платформы.
//!
//! Под узлом арбитра лежат узлы PMIC, первая ячейка `reg` которых - номер ведомого (SID).
//! Устройство - доступный узел с непустым `compatible` ниже узла PMIC. Каждая запись `reg`
//! устройства - 16-битный адрес начала блока из 256 регистров. Узел без `reg` получает блоки
//! ближайшего предка ниже узла PMIC, у которого `reg` есть, и остаётся отдельным устройством.
//! Запись `interrupts` - четыре ячейки: SID, номер блока, бит 0..7 и тип `IRQ_TYPE_*`.

use alloc::{borrow::ToOwned, string::String, vec::Vec};

use platform_abi::{Description, DescriptionError, DeviceId};
use spmi_abi::DeviceError;

use crate::{arbiter::COMMAND_BYTES_MAX, registers::Window};

const CELL_BYTES: usize = 4;

const EE_MAX: u32 = 5;

const SID_MAX: u32 = 0xF;
const BLOCK_BYTES: u32 = 0x100;
const BLOCK_ADDRESS_MAX: u32 = 0xFFFF;

const INTERRUPT_CELLS: usize = 4;
const INTERRUPT_BIT_MAX: u32 = 7;

const ADDRESS_CELLS_DEFAULT: u32 = 2;
const SIZE_CELLS_DEFAULT: u32 = 1;

const SEPARATOR: char = '/';

/// Свойства узла арбитра.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArbiterNode {
    ee: u8,
    resources: [u16; Window::ALL.len()],
    phandle: Option<u32>,
}

/// Отказ в разборе узла арбитра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeError {
    /// По пути нет узла.
    Missing,
    /// `qcom,ee` нет либо оно вне 0..=5.
    Ee,
    /// В `reg-names` нет имени окна.
    Window(Window),
    /// Свойство узла повреждено.
    Description(DescriptionError),
}

/// Устройство за арбитром.
#[derive(Clone, PartialEq, Eq)]
pub struct Peripheral {
    id: DeviceId,
    compatible: Vec<String>,
    sid: u8,
    blocks: Vec<u8>,
    interrupts: Vec<Result<Interrupt, DeviceError>>,
}

/// Узел за арбитром, который не стал устройством.
#[derive(Clone, PartialEq, Eq)]
pub struct Skipped {
    /// Путь узла.
    pub id: DeviceId,
    /// Причина.
    pub reason: SkipReason,
}

/// Причина пропуска узла за арбитром.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// `reg` узла PMIC не задаёт SID 0..=15.
    Sid,
    /// Запись `reg` устройства не задаёт начало блока.
    Block,
    /// Свойство узла повреждено.
    Description(DescriptionError),
}

/// Регистры одного блока, к которым обращается одна команда.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// PPID блока.
    pub ppid: u16,
    /// Смещение первого регистра в блоке.
    pub offset: u8,
}

/// Прерывание блока устройства.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interrupt {
    /// PPID блока.
    pub ppid: u16,
    /// Бит прерывания в регистрах прерываний блока.
    pub bit: u8,
    /// Условие срабатывания.
    pub trigger: Trigger,
}

/// Условие срабатывания прерывания, тип `IRQ_TYPE_*` описания.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// Подъём.
    Rising,
    /// Спад.
    Falling,
    /// Подъём и спад.
    Both,
    /// Высокий уровень.
    High,
    /// Низкий уровень.
    Low,
}

impl ArbiterNode {
    /// Разбирает узел арбитра `id`: среду исполнения `qcom,ee`, номера окон в `reg-names` и
    /// `phandle`.
    ///
    /// Отказы:
    /// - `Missing` - по пути нет узла;
    /// - `Ee` - `qcom,ee` нет либо оно вне 0..=5;
    /// - `Window` - в `reg-names` нет имени окна;
    /// - `Description` - свойство повреждено.
    pub fn parse(description: &Description<'_>, id: &DeviceId) -> Result<Self, NodeError> {
        let node = description.node(id).ok_or(NodeError::Missing)?;

        let ee = node
            .u32("qcom,ee")
            .map_err(NodeError::Description)?
            .filter(|ee| *ee <= EE_MAX)
            .and_then(|ee| u8::try_from(ee).ok())
            .ok_or(NodeError::Ee)?;

        let names: Vec<&str> = node
            .strings("reg-names")
            .map_err(NodeError::Description)?
            .map(Iterator::collect)
            .unwrap_or_default();

        let mut resources = [0; Window::ALL.len()];
        for (resource, window) in resources.iter_mut().zip(Window::ALL) {
            *resource = names
                .iter()
                .position(|name| *name == window.name())
                .and_then(|index| u16::try_from(index).ok())
                .ok_or(NodeError::Window(window))?;
        }

        let phandle = node.u32("phandle").map_err(NodeError::Description)?;

        Ok(Self {
            ee,
            resources,
            phandle,
        })
    }

    /// Среда исполнения процессора.
    pub fn ee(&self) -> u8 {
        self.ee
    }

    /// Номер записи `reg` узла арбитра для окна `window`.
    pub fn resource(&self, window: Window) -> u16 {
        Window::ALL
            .iter()
            .zip(self.resources)
            .find_map(|(each, resource)| (*each == window).then_some(resource))
            .unwrap_or_default()
    }

    /// Устройства за арбитром `id` в порядке описания и узлы, которые устройствами не стали.
    pub fn peripherals(
        &self,
        description: &Description<'_>,
        id: &DeviceId,
    ) -> Vec<Result<Peripheral, Skipped>> {
        let mut found = Vec::new();
        for device in description.devices().filter_map(Result::ok) {
            let Some(below) = device
                .id()
                .as_str()
                .strip_prefix(id.as_str())
                .and_then(|below| below.strip_prefix(SEPARATOR))
            else {
                continue;
            };

            let Some((pmic, _)) = below.split_once(SEPARATOR) else {
                continue;
            };

            let compatible = device.compatible().map(ToOwned::to_owned).collect();
            found.push(
                self.peripheral(description, id, pmic, device.id(), compatible)
                    .map_err(|reason| Skipped {
                        id: device.id().clone(),
                        reason,
                    }),
            );
        }

        found
    }

    fn peripheral(
        &self,
        description: &Description<'_>,
        arbiter: &DeviceId,
        pmic: &str,
        id: &DeviceId,
        compatible: Vec<String>,
    ) -> Result<Peripheral, SkipReason> {
        let mut pmic_path = String::from(arbiter.as_str());
        pmic_path.push(SEPARATOR);
        pmic_path.push_str(pmic);

        let pmic = description.find(&pmic_path).ok_or(SkipReason::Sid)?;
        let sid = cells(pmic.bytes("reg").unwrap_or_default())
            .and_then(|cells| cells.first().copied())
            .filter(|sid| *sid <= SID_MAX)
            .and_then(|sid| u8::try_from(sid).ok())
            .ok_or(SkipReason::Sid)?;

        let holder = blocks_holder(description, &pmic_path, id.as_str());
        let parent = holder
            .rsplit_once(SEPARATOR)
            .and_then(|(parent, _)| description.find(parent))
            .ok_or(SkipReason::Block)?;

        let address_cells = cell_count(&parent, "#address-cells", ADDRESS_CELLS_DEFAULT)?;
        let size_cells = cell_count(&parent, "#size-cells", SIZE_CELLS_DEFAULT)?;

        let reg = description
            .find(holder)
            .and_then(|holder| holder.bytes("reg"))
            .unwrap_or_default();
        let reg = cells(reg).ok_or(SkipReason::Block)?;
        let node = description.find(id.as_str()).ok_or(SkipReason::Block)?;

        let entry = address_cells + size_cells;
        if address_cells != 1 || !reg.len().is_multiple_of(entry) {
            return Err(SkipReason::Block);
        }

        let blocks = reg
            .chunks(entry)
            .map(|cells| {
                cells
                    .first()
                    .filter(|address| {
                        address.is_multiple_of(BLOCK_BYTES) && **address <= BLOCK_ADDRESS_MAX
                    })
                    .and_then(|address| u8::try_from(address / BLOCK_BYTES).ok())
            })
            .collect::<Option<Vec<_>>>()
            .ok_or(SkipReason::Block)?;

        let interrupt_cells = cells(node.bytes("interrupts").unwrap_or_default())
            .filter(|cells| cells.len().is_multiple_of(INTERRUPT_CELLS))
            .filter(|_| self.delivers(description, arbiter, id))
            .unwrap_or_default();

        let interrupts = interrupt_cells
            .as_chunks::<INTERRUPT_CELLS>()
            .0
            .iter()
            .map(|entry| interrupt(sid, &blocks, *entry))
            .collect();

        Ok(Peripheral {
            id: id.clone(),
            compatible,
            sid,
            blocks,
            interrupts,
        })
    }

    fn delivers(&self, description: &Description<'_>, arbiter: &DeviceId, id: &DeviceId) -> bool {
        let mut path = id.as_str();

        loop {
            let Some(node) = description.find(path) else {
                return false;
            };

            if let Ok(Some(parent)) = node.u32("interrupt-parent") {
                return self.phandle == Some(parent);
            }

            path = path.rsplit_once(SEPARATOR).map_or("", |(parent, _)| parent);
            if path == arbiter.as_str() {
                return true;
            }

            let controller = description
                .find(path)
                .is_some_and(|parent| parent.bytes("#interrupt-cells").is_some());
            if controller || !path.starts_with(arbiter.as_str()) {
                return false;
            }
        }
    }
}

impl Peripheral {
    /// Путь узла устройства.
    pub fn id(&self) -> &DeviceId {
        &self.id
    }

    /// Есть ли `compatible` в списке совместимостей устройства.
    pub fn is_compatible(&self, compatible: &str) -> bool {
        self.compatible.iter().any(|each| each == compatible)
    }

    /// PPID блоков устройства в порядке `reg`.
    pub fn ppids(&self) -> impl Iterator<Item = u16> + '_ {
        self.blocks.iter().map(|block| ppid(self.sid, *block))
    }

    /// Блок и смещение `count` регистров подряд с адреса `address`.
    ///
    /// Отказы:
    /// - `OutOfRange` - `count` вне 1..=8 либо регистры не лежат в одном блоке устройства.
    pub fn span(&self, address: u16, count: u8) -> Result<Span, DeviceError> {
        let [block, offset] = address.to_be_bytes();
        let inside = (1..=COMMAND_BYTES_MAX).contains(&count)
            && u32::from(offset) + u32::from(count) <= BLOCK_BYTES;

        if inside && self.blocks.contains(&block) {
            Ok(Span {
                ppid: ppid(self.sid, block),
                offset,
            })
        } else {
            Err(DeviceError::OutOfRange)
        }
    }

    /// Прерывание из записи `resource` в `interrupts` устройства.
    ///
    /// Отказы:
    /// - `NotDescribed` - в `interrupts` нет записи `resource`, её тип неверен либо прерывания
    ///   узла доставляет не арбитр;
    /// - `OutOfRange` - SID, блок либо бит записи не принадлежат устройству.
    pub fn interrupt(&self, resource: u16) -> Result<Interrupt, DeviceError> {
        self.interrupts
            .get(usize::from(resource))
            .copied()
            .unwrap_or(Err(DeviceError::NotDescribed))
    }
}

fn interrupt(
    sid: u8,
    blocks: &[u8],
    [entry_sid, block, bit, kind]: [u32; INTERRUPT_CELLS],
) -> Result<Interrupt, DeviceError> {
    let trigger = trigger(kind).ok_or(DeviceError::NotDescribed)?;

    let block = u8::try_from(block)
        .ok()
        .filter(|block| u32::from(sid) == entry_sid && blocks.contains(block))
        .ok_or(DeviceError::OutOfRange)?;

    let bit = u8::try_from(bit)
        .ok()
        .filter(|bit| u32::from(*bit) <= INTERRUPT_BIT_MAX)
        .ok_or(DeviceError::OutOfRange)?;

    Ok(Interrupt {
        ppid: ppid(sid, block),
        bit,
        trigger,
    })
}

fn blocks_holder<'a>(description: &Description<'_>, pmic: &str, id: &'a str) -> &'a str {
    let mut path = id;

    while path.len() > pmic.len() {
        if description
            .find(path)
            .is_some_and(|node| node.bytes("reg").is_some())
        {
            return path;
        }

        path = path.rsplit_once(SEPARATOR).map_or("", |(parent, _)| parent);
    }

    id
}

const fn trigger(kind: u32) -> Option<Trigger> {
    match kind {
        1 => Some(Trigger::Rising),
        2 => Some(Trigger::Falling),
        3 => Some(Trigger::Both),
        4 => Some(Trigger::High),
        8 => Some(Trigger::Low),
        _ => None,
    }
}

fn ppid(sid: u8, block: u8) -> u16 {
    u16::from_be_bytes([sid, block])
}

fn cells(bytes: &[u8]) -> Option<Vec<u32>> {
    let (cells, rest) = bytes.as_chunks::<CELL_BYTES>();

    rest.is_empty()
        .then(|| cells.iter().map(|cell| u32::from_be_bytes(*cell)).collect())
}

fn cell_count(
    node: &platform_abi::Node<'_>,
    name: &str,
    default: u32,
) -> Result<usize, SkipReason> {
    let count = node
        .u32(name)
        .map_err(SkipReason::Description)?
        .unwrap_or(default);

    usize::try_from(count).map_err(|_| SkipReason::Block)
}
