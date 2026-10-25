//! Контроллер QUP v2 в режиме FIFO: подготовка по `qup_i2c_probe` и передача по
//! `qup_i2c_xfer_v2` Linux 6.16.

use alloc::vec::Vec;

use i2c_abi::{TransferBuf, TransferError};
use mmio::Registers;

use crate::registers::{
    CONFIG, ERROR_FLAGS, I2C_CLK_CTL, I2C_MASTER_GEN, I2C_STATUS, INPUT_FIFO, IO_MODE,
    MX_INPUT_CNT, MX_OUTPUT_CNT, MX_READ_CNT, MX_WRITE_CNT, OPERATIONAL, OUTPUT_FIFO, STATE,
    SW_RESET, WINDOW_MIN,
};

pub(crate) const CORE_HZ: u32 = 19_200_000;

const STANDARD_MODE_HZ: u32 = 100_000;
const FAST_MODE_HZ: u32 = 400_000;
const HS_DIVIDER: u32 = 3;
const FS_DIVIDER_MAX: u32 = 0xff;

const STATE_RESET: u32 = 0;
const STATE_RUN: u32 = 1;
const STATE_PAUSE: u32 = 3;
const STATE_MASK: u32 = 3;
const STATE_VALID: u32 = 1 << 2;
const MASTER_GENERATOR: u32 = 1 << 4;

const NACK: u32 = 1 << 3;
const OUTPUT_SERVICE: u32 = 1 << 8;
const INPUT_SERVICE: u32 = 1 << 9;
const BUS_ACTIVE: u32 = 1 << 8;
const STATUS_ERRORS: u32 = 0x0380_00fc;
const QUP_ERRORS: u32 = 0x7c;

const NO_INPUT: u32 = 1 << 7;
const MINI_CORE: u32 = 2 << 8;
const N_VALUE: u32 = 7;
const REPACK: u32 = 1 << 14 | 1 << 15;
const TAGS_ENABLE: u32 = 1;
const CONFIG_DURING_RUN: u32 = 1 << 31;

const BLOCK_SIZES: [usize; 3] = [4, 16, 32];
const OUTPUT_BLOCK_SHIFT: u32 = 0;
const OUTPUT_FIFO_SHIFT: u32 = 2;
const INPUT_BLOCK_SHIFT: u32 = 5;
const INPUT_FIFO_SHIFT: u32 = 7;
const BLOCK_FIELD: u32 = 0x3;
const FIFO_FIELD: u32 = 0x7;

const TAG_START: u8 = 0x81;
const TAG_WRITE: u8 = 0x82;
const TAG_WRITE_STOP: u8 = 0x83;
const TAG_READ_NACK: u8 = 0x86;
const TAG_READ_STOP: u8 = 0x87;

const OUTPUT_TAGS: usize = 4;
const INPUT_TAGS: usize = 2;
const WRITE_MAX: usize = 256;
const READ_MAX: usize = 254;

const WORD_BYTES: usize = 4;
const BITS_PER_BYTE: usize = 8;

const STATE_POLLS: usize = 10_000;
const BUS_POLLS: usize = 100_000;

/// Подготовленный контроллер: регистры его окна, размеры FIFO и делитель шины.
pub struct Controller<W> {
    registers: W,
    output_fifo: usize,
    input_fifo: usize,
    clock_control: u32,
}

/// Отказ подготовки контроллера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Окно меньше регистров контроллера, с размером окна.
    Window(usize),
    /// После сброса `QUP_STATE` не сообщил о верном состоянии.
    State,
    /// `QUP_IO_MODE` называет размер блока вне таблицы.
    IoMode(u32),
}

/// Передача устройству: запись, чтение либо запись и повторным стартом чтение.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation<'a> {
    /// Запись байт одним сообщением с остановкой.
    Write(&'a [u8]),
    /// Чтение числа байт одним сообщением с остановкой.
    Read(u16),
    /// Запись байт без остановки и чтение числа байт с остановкой.
    WriteRead(&'a [u8], u16),
}

/// Уровневая линия прерывания контроллера.
#[allow(async_fn_in_trait)]
pub trait Line {
    /// Ждёт срабатывания линии. Возвращает `false`, если срок истёк.
    async fn wait(&self) -> bool;

    /// Снимает защёлку линии, когда контроллер погасил источник прерывания.
    fn ack(&self);

    /// Снимает защёлку, поднятую до сброса контроллера.
    fn clear(&self);
}

#[derive(Clone, Copy)]
enum Message<'a> {
    Write(&'a [u8]),
    Read(usize),
}

#[derive(Default)]
struct Input {
    tags_fetched: bool,
    word: u32,
    position: usize,
}

struct CancelTransfer<'a, W: Registers<()>, L: Line> {
    controller: &'a Controller<W>,
    line: &'a L,
    armed: bool,
}

impl<W: Registers<()>, L: Line> Drop for CancelTransfer<'_, W, L> {
    fn drop(&mut self) {
        if self.armed {
            self.controller.store(SW_RESET, 1);
            self.line.clear();
        }
    }
}

impl<W: Registers<()>> Controller<W> {
    pub(crate) fn new(registers: W, clock_control: u32) -> Result<Self, Refusal> {
        let size = registers.size(());
        if size < WINDOW_MIN {
            return Err(Refusal::Window(size));
        }

        // Загрузчик мог оставить прерывание контроллера, и сброс гасит его до взятия линии.
        registers.store((), SW_RESET, 1);
        let controller = Self {
            registers,
            output_fifo: 0,
            input_fifo: 0,
            clock_control,
        };
        if !controller.poll_state(0, 0) {
            return Err(Refusal::State);
        }

        let io_mode = controller.load(IO_MODE);
        let fifo = |block_shift, fifo_shift| {
            let block = BLOCK_SIZES.get(field(io_mode, block_shift, BLOCK_FIELD))?;
            Some(block << (field(io_mode, fifo_shift, FIFO_FIELD) + 1))
        };
        let output_fifo =
            fifo(OUTPUT_BLOCK_SHIFT, OUTPUT_FIFO_SHIFT).ok_or(Refusal::IoMode(io_mode))?;
        let input_fifo =
            fifo(INPUT_BLOCK_SHIFT, INPUT_FIFO_SHIFT).ok_or(Refusal::IoMode(io_mode))?;

        Ok(Self {
            output_fifo,
            input_fifo,
            ..controller
        })
    }

    /// Выполняет передачу `operation` устройству с адресом `address` и ждёт её прерывания на
    /// линии `line`.
    ///
    /// Отказы:
    /// - `Length` - данные записи пусты либо не помещаются в FIFO вывода, число байт чтения
    ///   равно 0 либо не помещается в FIFO ввода, и регистры не записываются;
    /// - `NoAck`, `Bus` - контроллер сообщил об отказе, и контроллер переведён в `RESET`;
    /// - `Timeout` - прерывание не пришло за срок либо контроллер не перешёл в нужное
    ///   состояние, и контроллер сброшен.
    ///
    /// Отмена future сбрасывает контроллер и снимает оставшуюся защёлку линии. Вызывающий
    /// обеспечивает исключительное владение контроллером до разрушения future.
    pub async fn transfer(
        &self,
        line: &impl Line,
        address: u8,
        operation: Operation<'_>,
    ) -> Result<TransferBuf, TransferError> {
        let (pair, len) = match operation {
            Operation::Write(data) => ([Message::Write(data), Message::Read(0)], 1),
            Operation::Read(count) => ([Message::Read(usize::from(count)), Message::Read(0)], 1),
            Operation::WriteRead(data, count) => {
                ([Message::Write(data), Message::Read(usize::from(count))], 2)
            }
        };
        let messages = pair.get(..len).unwrap_or_default();
        if !messages.iter().all(|message| self.fits(*message)) {
            return Err(TransferError::Length);
        }

        let mut cancelled = CancelTransfer {
            controller: self,
            line,
            armed: true,
        };

        let outcome = self.perform(line, address, messages).await;
        cancelled.armed = false;
        outcome
    }

    async fn perform(
        &self,
        line: &impl Line,
        address: u8,
        messages: &[Message<'_>],
    ) -> Result<TransferBuf, TransferError> {
        self.store(SW_RESET, 1);
        self.expect_state(STATE_RESET, STATE_MASK)?;
        line.clear();

        self.store(CONFIG, MINI_CORE | N_VALUE);
        self.store(I2C_MASTER_GEN, TAGS_ENABLE);
        self.expect_state(MASTER_GENERATOR, MASTER_GENERATOR)?;
        self.store(MX_OUTPUT_CNT, 0);
        self.store(MX_INPUT_CNT, 0);
        self.store(IO_MODE, REPACK);

        let mut received = Vec::new();
        for (index, message) in messages.iter().enumerate() {
            let last = index + 1 == messages.len();
            self.start(address, *message, index == 0, last)?;

            let mut input = Input::default();
            loop {
                if !line.wait().await {
                    return Err(self.timeout());
                }
                // Шаг гасит источник уровневой линии до снятия защёлки.
                let serviced = self.service(*message, &mut input, &mut received);
                line.ack();
                if serviced? {
                    break;
                }
            }

            if !last {
                self.expect_change(STATE_PAUSE)?;
            }
        }

        if !self.bus_idle() {
            return Err(self.timeout());
        }
        self.change_state(STATE_RESET);
        Ok(TransferBuf(received))
    }

    fn fits(&self, message: Message<'_>) -> bool {
        match message {
            Message::Write(data) => {
                !data.is_empty()
                    && data.len() <= WRITE_MAX
                    && data.len() + OUTPUT_TAGS <= self.output_fifo
            }
            Message::Read(count) => {
                count != 0 && count <= READ_MAX && count + INPUT_TAGS <= self.input_fifo
            }
        }
    }

    fn start(
        &self,
        address: u8,
        message: Message<'_>,
        first: bool,
        last: bool,
    ) -> Result<(), TransferError> {
        let (read, data, length, tag) = match message {
            Message::Write(data) => (
                0,
                data,
                data.len(),
                if last { TAG_WRITE_STOP } else { TAG_WRITE },
            ),
            Message::Read(count) => (
                1,
                &[][..],
                count,
                if last { TAG_READ_STOP } else { TAG_READ_NACK },
            ),
        };
        // Длину 256 тег передаёт нулём.
        let length_tag = u8::try_from(length).unwrap_or(0);
        let tags = [TAG_START, address << 1 | read, tag, length_tag];

        let during_run = if first { 0 } else { CONFIG_DURING_RUN };
        let mut config = MINI_CORE | N_VALUE;
        self.store(MX_WRITE_CNT, during_run | count(OUTPUT_TAGS + data.len()));
        if read == 1 {
            self.store(MX_READ_CNT, during_run | count(INPUT_TAGS + length));
        } else {
            config |= NO_INPUT;
        }
        self.store(CONFIG, config);

        if first {
            self.expect_change(STATE_RUN)?;
            self.store(I2C_CLK_CTL, self.clock_control);
            self.expect_change(STATE_PAUSE)?;
        }

        let mut word = 0;
        let mut position = 0;
        for byte in tags.iter().chain(data) {
            word |= u32::from(*byte) << (position * BITS_PER_BYTE);
            position += 1;
            if position == WORD_BYTES {
                self.store(OUTPUT_FIFO, word);
                word = 0;
                position = 0;
            }
        }
        if position != 0 {
            self.store(OUTPUT_FIFO, word);
        }

        self.expect_change(STATE_RUN)
    }

    fn service(
        &self,
        message: Message<'_>,
        input: &mut Input,
        received: &mut Vec<u8>,
    ) -> Result<bool, TransferError> {
        let bus_errors = self.load(I2C_STATUS) & STATUS_ERRORS;
        let qup_errors = self.load(ERROR_FLAGS) & QUP_ERRORS;
        let flags = self.load(OPERATIONAL);

        if qup_errors != 0 {
            self.store(ERROR_FLAGS, qup_errors);
        }
        if bus_errors != 0 {
            self.store(I2C_STATUS, bus_errors);
        }
        if qup_errors != 0 || bus_errors != 0 {
            self.store(STATE, STATE_RESET);
            return Err(if bus_errors & NACK != 0 {
                TransferError::NoAck
            } else {
                TransferError::Bus
            });
        }

        if flags & OUTPUT_SERVICE != 0 {
            self.store(OPERATIONAL, OUTPUT_SERVICE);
        }
        let Message::Read(count) = message else {
            // Запись в режиме FIFO завершает первое прерывание без отказа.
            return Ok(true);
        };
        if flags & INPUT_SERVICE != 0 {
            self.store(OPERATIONAL, INPUT_SERVICE);
            self.receive(count, input, received);
        }
        Ok(received.len() >= count)
    }

    fn receive(&self, count: usize, input: &mut Input, received: &mut Vec<u8>) {
        let mut available = self.input_fifo;
        if !input.tags_fetched {
            input.word = self.load(INPUT_FIFO) >> (INPUT_TAGS * BITS_PER_BYTE);
            input.position = INPUT_TAGS;
            input.tags_fetched = true;
            available -= INPUT_TAGS;
        }

        while received.len() < count && available > 0 {
            if input.position == 0 {
                input.word = self.load(INPUT_FIFO);
            }
            let [byte, ..] = input.word.to_le_bytes();
            received.push(byte);
            input.word >>= BITS_PER_BYTE;
            input.position = (input.position + 1) % WORD_BYTES;
            available -= 1;
        }
    }

    fn load(&self, offset: usize) -> u32 {
        self.registers.load((), offset)
    }

    fn store(&self, offset: usize, value: u32) {
        self.registers.store((), offset, value);
    }

    fn timeout(&self) -> TransferError {
        self.store(SW_RESET, 1);
        TransferError::Timeout
    }

    fn poll_state(&self, state: u32, mask: u32) -> bool {
        (0..STATE_POLLS).any(|_| {
            let value = self.load(STATE);
            value & STATE_VALID != 0 && value & mask == state
        })
    }

    fn expect_state(&self, state: u32, mask: u32) -> Result<(), TransferError> {
        if self.poll_state(state, mask) {
            Ok(())
        } else {
            Err(self.timeout())
        }
    }

    fn change_state(&self, state: u32) -> bool {
        if !self.poll_state(0, 0) {
            return false;
        }
        self.store(STATE, state);
        self.poll_state(state, STATE_MASK)
    }

    fn expect_change(&self, state: u32) -> Result<(), TransferError> {
        if self.change_state(state) {
            Ok(())
        } else {
            Err(self.timeout())
        }
    }

    fn bus_idle(&self) -> bool {
        (0..BUS_POLLS).any(|_| self.load(I2C_STATUS) & BUS_ACTIVE == 0)
    }
}

pub(crate) fn clock_control(bus_hz: u32) -> Option<u32> {
    if bus_hz == 0 || bus_hz > FAST_MODE_HZ {
        return None;
    }

    let ratio = CORE_HZ / bus_hz;
    if bus_hz <= STANDARD_MODE_HZ {
        let fs_divider = (ratio / 2).saturating_sub(3);
        (fs_divider <= FS_DIVIDER_MAX).then_some(HS_DIVIDER << 8 | fs_divider)
    } else {
        // Скважность 33/66 %.
        let fs_divider = ratio.saturating_sub(6) * 2 / 3;
        Some((fs_divider / 2) << 16 | HS_DIVIDER << 8 | fs_divider)
    }
}

fn field(value: u32, shift: u32, mask: u32) -> usize {
    usize::try_from(value >> shift & mask).unwrap_or(usize::MAX)
}

fn count(bytes: usize) -> u32 {
    u32::try_from(bytes).unwrap_or(u32::MAX)
}
