use alloc::vec::Vec;

use spmi_abi::DeviceError;

use crate::{
    peripheral::{Interrupt, Trigger},
    registers::{Registers, Window},
};

const VERSION: usize = 0x0;
const VERSION_2: u32 = 0x2001_0000;
const VERSION_3: u32 = 0x3000_0000;
const VERSION_5: u32 = 0x5000_0000;

const REGISTER_BYTES: usize = 4;

const CHANNEL_TABLE: usize = 0x800;
const OWNER_TABLE: usize = 0x700;
const TABLE_ENTRY_BYTES: usize = 4;
const CHANNELS_MAX: usize = 512;

const PPID_SHIFT: u32 = 8;
const PPID_MASK: u32 = 0xFFF;
const OWNER_MASK: u32 = 0x7;

const CHANNEL_STRIDE: usize = 0x8000;
const EE_STRIDE: usize = 0x1000;

const COMMAND: usize = 0x0;
const STATUS: usize = 0x8;
const WRITE_DATA_LOW: usize = 0x10;
const WRITE_DATA_HIGH: usize = 0x14;
const READ_DATA_LOW: usize = 0x18;
const READ_DATA_HIGH: usize = 0x1C;

const STATUS_DONE: u32 = 1 << 0;
const STATUS_FAILURE: u32 = 1 << 1;
const STATUS_DENIED: u32 = 1 << 2;
const STATUS_DROPPED: u32 = 1 << 3;

const OPCODE_SHIFT: u32 = 27;
const OFFSET_SHIFT: u32 = 4;
const OPCODE_WRITE: u32 = 0;
const OPCODE_READ: u32 = 1;

pub(crate) const COMMAND_BYTES_MAX: u8 = 8;
const DATA_REGISTER_BYTES: u8 = 4;

const STATUS_POLLS: usize = 100_000;

const ACCUMULATOR_V2: usize = 0x10_0000;
const ACCUMULATOR_V3: usize = 0x20_0000;
const ACCUMULATOR_EE_STRIDE: usize = 0x1000;
const CHANNELS_PER_WORD: usize = 32;

const CHANNEL_INTERRUPT_STRIDE: usize = 0x1000;
const CHANNEL_ENABLE: usize = 0x0;
const CHANNEL_STATUS: usize = 0x4;
const CHANNEL_CLEAR: usize = 0x8;

const CHANNEL_ENABLED: u32 = 1 << 0;
const CHANNEL_STATUS_MASK: u32 = 0xFF;
const STATUS_CLEAR_ALL: u32 = 0xFF;

const SET_TYPE: u8 = 0x11;
const LATCHED_CLEAR: u8 = 0x14;
const ENABLE_CLEAR: u8 = 0x16;

const TYPE_LANES: u64 = 0x01_01_01;

/// Версия арбитра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    /// Версия 2.
    V2,
    /// Версия 3.
    V3,
}

/// Отказ в создании [`Arbiter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Регистр версии читается значением вне версий 2 и 3.
    Version(u32),
    /// Окно `core` либо `cnfg` меньше регистров версии и таблиц каналов.
    Window,
}

/// Арбитр над регистрами `R` в среде исполнения `ee`. Канал (APID) арбитра обслуживает один
/// блок PMIC - PPID с номером ведомого в битах 11:8 и номером блока в битах 7:0.
pub struct Arbiter<R> {
    registers: R,
    version: Version,
    ee: u8,
    channels: Vec<Option<Channel>>,
}

#[derive(Debug, Clone, Copy)]
struct Channel {
    ppid: u16,
    owner: u8,
}

impl<R: Registers> Arbiter<R> {
    /// Читает версию арбитра и таблицы каналов и владельцев. Число каналов задаёт размер окна
    /// `core`, не больше 512.
    ///
    /// Отказы:
    /// - `Version` - версия вне 2 и 3;
    /// - `Window` - окно `core` либо `cnfg` не вмещает регистр версии либо владельца канала.
    pub fn new(registers: R, ee: u8) -> Result<Self, Refusal> {
        let raw = checked_load(&registers, Window::Core, VERSION).ok_or(Refusal::Window)?;

        let version = match raw {
            VERSION_3..VERSION_5 => Version::V3,
            VERSION_2..VERSION_3 => Version::V2,
            _ => return Err(Refusal::Version(raw)),
        };

        let mut channels = Vec::with_capacity(CHANNELS_MAX);
        for apid in 0..CHANNELS_MAX {
            let entry_offset = apid * TABLE_ENTRY_BYTES;
            let Some(entry) = checked_load(&registers, Window::Core, CHANNEL_TABLE + entry_offset)
            else {
                break;
            };

            let owner = checked_load(&registers, Window::Config, OWNER_TABLE + entry_offset)
                .ok_or(Refusal::Window)?;

            channels.push((entry != 0).then(|| Channel {
                ppid: field(entry >> PPID_SHIFT, PPID_MASK),
                owner: field(owner, OWNER_MASK),
            }));
        }

        Ok(Self {
            registers,
            version,
            ee,
            channels,
        })
    }

    /// Версия арбитра.
    pub fn version(&self) -> Version {
        self.version
    }

    /// Число каналов таблицы, включая пустые записи.
    pub fn channels(&self) -> usize {
        self.channels.len()
    }

    /// Среда исполнения - владелец канала блока `ppid`. Блок без канала даёт `None`.
    pub fn owner(&self, ppid: u16) -> Option<u8> {
        self.channel(ppid).map(|(_, channel)| channel.owner)
    }

    /// Читает `count` байт регистров блока `ppid` с `offset`. Байт `offset` - младший байт
    /// значения.
    ///
    /// Отказы:
    /// - `OutOfRange` - `count` вне 1..=8;
    /// - `Denied` - арбитр отказал среде в доступе;
    /// - `Failed` - у блока нет канала, арбитр не выполнил команду либо не ответил за отведённое
    ///   число опросов.
    pub fn read(&self, ppid: u16, offset: u8, count: u8) -> Result<u64, DeviceError> {
        let (apid, _) = self.channel(ppid).ok_or(DeviceError::Failed)?;

        self.read_channel(apid, offset, count)
    }

    /// Записывает `count` младших байт `value` в регистры блока `ppid` с `offset`.
    ///
    /// Отказы:
    /// - `OutOfRange` - `count` вне 1..=8;
    /// - `Denied` - канал блока принадлежит другой среде либо арбитр отказал в доступе;
    /// - `Failed` - у блока нет канала, арбитр не выполнил команду либо не ответил за отведённое
    ///   число опросов.
    pub fn write(&self, ppid: u16, offset: u8, count: u8, value: u64) -> Result<(), DeviceError> {
        let (apid, channel) = self.channel(ppid).ok_or(DeviceError::Failed)?;

        self.write_channel(apid, channel, offset, count, value)
    }

    /// Заменяет биты `mask` регистра `offset` блока `ppid` битами `value`. Регистр
    /// записывается, только если его значение меняется.
    ///
    /// Отказы - как у [`read`](Self::read) и [`write`](Self::write).
    pub fn update(&self, ppid: u16, offset: u8, mask: u8, value: u8) -> Result<(), DeviceError> {
        let (apid, channel) = self.channel(ppid).ok_or(DeviceError::Failed)?;
        let current = self.read_channel(apid, offset, 1)?;
        let updated = (current & !u64::from(mask)) | u64::from(value & mask);

        if updated == current {
            return Ok(());
        }

        self.write_channel(apid, channel, offset, 1, updated)
    }

    /// Настраивает срабатывание прерывания `interrupt`, разрешает его в блоке и разрешает
    /// канал блока в арбитре. При отказе канал остаётся в прежнем состоянии.
    ///
    /// Отказы:
    /// - `Denied` - канал блока принадлежит другой среде;
    /// - `Failed` - у блока нет канала либо команда не выполнена.
    pub fn arm(&self, interrupt: &Interrupt) -> Result<(), DeviceError> {
        let (apid, channel) = self.channel(interrupt.ppid).ok_or(DeviceError::Failed)?;
        let mask = 1u64 << interrupt.bit;

        self.write_channel(apid, channel, ENABLE_CLEAR, 1, mask)?;
        self.write_channel(apid, channel, LATCHED_CLEAR, 1, mask)?;

        let current = self.read_channel(apid, SET_TYPE, 3)?;
        let configured = (current & !(TYPE_LANES * mask)) | (polarity(interrupt.trigger) * mask);
        self.write_channel(apid, channel, SET_TYPE, 3, configured)?;

        let enabled = self.enabled(apid);
        self.store(
            Window::Interrupts,
            interrupt_base(apid) + CHANNEL_ENABLE,
            CHANNEL_ENABLED,
        )
        .and_then(|()| self.unmask(apid, channel, mask))
        .inspect_err(|_| {
            if !enabled {
                self.disable_channel(apid);
            }
        })
    }

    /// Снимает защёлку прерывания `interrupt` и разрешает его в блоке.
    ///
    /// Отказы - как у [`write`](Self::write).
    pub fn rearm(&self, interrupt: &Interrupt) -> Result<(), DeviceError> {
        let (apid, channel) = self.channel(interrupt.ppid).ok_or(DeviceError::Failed)?;

        self.unmask(apid, channel, 1u64 << interrupt.bit)
    }

    /// Запрещает прерывания `bits` блока `ppid` в блоке.
    ///
    /// Отказы - как у [`write`](Self::write).
    pub fn mask(&self, ppid: u16, bits: u8) -> Result<(), DeviceError> {
        let (apid, channel) = self.channel(ppid).ok_or(DeviceError::Failed)?;

        self.write_channel(apid, channel, ENABLE_CLEAR, 1, u64::from(bits))
    }

    /// Запрещает канал блока `ppid` в арбитре и сбрасывает его статус. Блок без канала
    /// пропускается.
    pub fn disable(&self, ppid: u16) {
        if let Some((apid, _)) = self.channel(ppid) {
            self.disable_channel(apid);
        }
    }

    /// Запрещает в блоке сработавшие прерывания каналов своей среды, сбрасывает статус каналов
    /// и передаёт `deliver` PPID блока и биты статуса.
    pub fn drain(&self, mut deliver: impl FnMut(u16, u8)) {
        let accumulator = match self.version {
            Version::V2 => ACCUMULATOR_V2,
            Version::V3 => ACCUMULATOR_V3,
        } + usize::from(self.ee) * ACCUMULATOR_EE_STRIDE;

        let mut accumulated = false;
        for word in 0..self.channels.len().div_ceil(CHANNELS_PER_WORD) {
            let Ok(bits) = self.load(Window::Interrupts, accumulator + word * TABLE_ENTRY_BYTES)
            else {
                continue;
            };

            for bit in (0..CHANNELS_PER_WORD).filter(|bit| bits & (1 << bit) != 0) {
                let apid = word * CHANNELS_PER_WORD + bit;

                if self.enabled(apid) {
                    accumulated = true;
                    self.service(apid, &mut deliver);
                }
            }
        }

        if !accumulated {
            for apid in 0..self.channels.len() {
                if self.owned(apid) && self.enabled(apid) {
                    self.service(apid, &mut deliver);
                }
            }
        }
    }

    fn service(&self, apid: usize, deliver: &mut impl FnMut(u16, u8)) {
        let base = interrupt_base(apid);
        let Ok(status) = self.load(Window::Interrupts, base + CHANNEL_STATUS) else {
            return;
        };

        let bits = field(status, CHANNEL_STATUS_MASK);
        if bits == 0 {
            return;
        }

        let channel = self.channels.get(apid).copied().flatten();
        if let Some(channel) = channel {
            let _ = self.write_channel(apid, channel, ENABLE_CLEAR, 1, u64::from(bits));
        }

        let _ = self.store(Window::Interrupts, base + CHANNEL_CLEAR, u32::from(bits));

        if let Some(channel) = channel {
            deliver(channel.ppid, bits);
        }
    }

    fn unmask(&self, apid: usize, channel: Channel, mask: u64) -> Result<(), DeviceError> {
        self.write_channel(apid, channel, LATCHED_CLEAR, 2, mask | mask << 8)
    }

    fn disable_channel(&self, apid: usize) {
        let base = interrupt_base(apid);
        let _ = self.store(Window::Interrupts, base + CHANNEL_ENABLE, 0);
        let _ = self.store(Window::Interrupts, base + CHANNEL_CLEAR, STATUS_CLEAR_ALL);
    }

    fn read_channel(&self, apid: usize, offset: u8, count: u8) -> Result<u64, DeviceError> {
        let base = self.command_base(apid);
        self.command(Window::Observer, base, OPCODE_READ, offset, count)?;

        let low = self.load(Window::Observer, base + READ_DATA_LOW)?;
        let high = if count > DATA_REGISTER_BYTES {
            self.load(Window::Observer, base + READ_DATA_HIGH)?
        } else {
            0
        };

        let value = u64::from(high) << 32 | u64::from(low);
        Ok(value & (u64::MAX >> (64 - 8 * u32::from(count))))
    }

    fn write_channel(
        &self,
        apid: usize,
        channel: Channel,
        offset: u8,
        count: u8,
        value: u64,
    ) -> Result<(), DeviceError> {
        if channel.owner != self.ee {
            return Err(DeviceError::Denied);
        }

        let base = self.command_base(apid);
        let [low, high] = [value, value >> 32].map(low_word);

        self.store(Window::Channels, base + WRITE_DATA_LOW, low)?;
        if count > DATA_REGISTER_BYTES {
            self.store(Window::Channels, base + WRITE_DATA_HIGH, high)?;
        }

        self.command(Window::Channels, base, OPCODE_WRITE, offset, count)
    }

    fn channel(&self, ppid: u16) -> Option<(usize, Channel)> {
        self.channels
            .iter()
            .enumerate()
            .find_map(|(apid, channel)| {
                channel
                    .filter(|channel| channel.ppid == ppid)
                    .map(|channel| (apid, channel))
            })
    }

    fn owned(&self, apid: usize) -> bool {
        matches!(self.channels.get(apid), Some(Some(channel)) if channel.owner == self.ee)
    }

    fn enabled(&self, apid: usize) -> bool {
        self.load(Window::Interrupts, interrupt_base(apid) + CHANNEL_ENABLE)
            .is_ok_and(|enable| enable & CHANNEL_ENABLED != 0)
    }

    fn command(
        &self,
        window: Window,
        base: usize,
        opcode: u32,
        offset: u8,
        count: u8,
    ) -> Result<(), DeviceError> {
        let Some(bytes) = count.checked_sub(1).filter(|_| count <= COMMAND_BYTES_MAX) else {
            return Err(DeviceError::OutOfRange);
        };

        let command = opcode << OPCODE_SHIFT | u32::from(offset) << OFFSET_SHIFT | u32::from(bytes);
        self.store(window, base + COMMAND, command)?;

        // Опросы покрывают 1000 мкс ожидания DONE из Linux, если чтение регистра длится не
        // меньше 10 нс.
        for _ in 0..STATUS_POLLS {
            let status = self.load(window, base + STATUS)?;
            if status & STATUS_DONE == 0 {
                continue;
            }

            if status & STATUS_DENIED != 0 {
                return Err(DeviceError::Denied);
            }

            if status & (STATUS_FAILURE | STATUS_DROPPED) != 0 {
                return Err(DeviceError::Failed);
            }

            return Ok(());
        }

        Err(DeviceError::Failed)
    }

    fn command_base(&self, apid: usize) -> usize {
        apid * CHANNEL_STRIDE + usize::from(self.ee) * EE_STRIDE
    }

    fn load(&self, window: Window, offset: usize) -> Result<u32, DeviceError> {
        checked_load(&self.registers, window, offset).ok_or(DeviceError::Failed)
    }

    fn store(&self, window: Window, offset: usize, value: u32) -> Result<(), DeviceError> {
        if !inside(self.registers.size(window), offset) {
            return Err(DeviceError::Failed);
        }

        self.registers.store(window, offset, value);
        Ok(())
    }
}

const fn polarity(trigger: Trigger) -> u64 {
    match trigger {
        Trigger::Rising => 0x00_01_01,
        Trigger::Falling => 0x01_00_01,
        Trigger::Both => 0x01_01_01,
        Trigger::High => 0x00_01_00,
        Trigger::Low => 0x01_00_00,
    }
}

fn checked_load<R: Registers>(registers: &R, window: Window, offset: usize) -> Option<u32> {
    inside(registers.size(window), offset).then(|| registers.load(window, offset))
}

fn inside(size: usize, offset: usize) -> bool {
    offset.is_multiple_of(REGISTER_BYTES)
        && offset
            .checked_add(REGISTER_BYTES)
            .is_some_and(|end| end <= size)
}

fn interrupt_base(apid: usize) -> usize {
    apid * CHANNEL_INTERRUPT_STRIDE
}

fn field<T: TryFrom<u32> + Default>(value: u32, mask: u32) -> T {
    T::try_from(value & mask).unwrap_or_default()
}

fn low_word(value: u64) -> u32 {
    u32::try_from(value & u64::from(u32::MAX)).unwrap_or_default()
}
