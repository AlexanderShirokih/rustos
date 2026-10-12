//! Включение тактов узла и частота такта у контроллера GCC.

use alloc::vec::Vec;

use clocks_abi::ClocksError;
use platform_abi::{Description, DeviceClaims, DeviceId};

use crate::{
    records::records,
    table::{Branch, Clock, Enable, Generator},
};

const REGISTER_BYTES: usize = 4;
const VOTE: usize = 0x52004;

const BRANCH_ENABLE: u32 = 1 << 0;
const CLK_OFF: u32 = 1 << 31;
const NOC_FSM_STATUS_SHIFT: u32 = 28;
const NOC_FSM_STATUS_MASK: u32 = 0x7;
const NOC_FSM_STATUS_ON: u32 = 2;

const CFG: usize = 0x4;
const UPDATE: u32 = 1 << 0;
const DIVIDER_MASK: u32 = 0x1F;
const SOURCE_SHIFT: u32 = 8;
const SOURCE_MASK: u32 = 0x7 << SOURCE_SHIFT;
const MODE_MASK: u32 = 0x3 << 12;
const HW_CLK_CTRL: u32 = 1 << 20;

const BRANCH_POLLS: usize = 20_000;
const UPDATE_POLLS: usize = 50_000;

/// Регистры окна контроллера. Смещение в [`load`](Registers::load) и
/// [`store`](Registers::store) выровнено по регистру и лежит внутри окна.
pub trait Registers {
    /// Размер окна в байтах.
    fn size(&self) -> usize;

    /// Читает регистр по смещению `offset`.
    fn load(&self, offset: usize) -> u32;

    /// Записывает регистр по смещению `offset`.
    fn store(&self, offset: usize, value: u32);
}

/// Контроллер GCC: регистры его окна и таблица тактов SoC.
pub struct Gcc<R> {
    registers: R,
    clocks: &'static [Clock],
    id: DeviceId,
}

/// Отказ в создании [`Gcc`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Окно меньше регистров тактов таблицы.
    Window,
}

impl<R: Registers> Gcc<R> {
    /// Контроллер узла `id` над регистрами `registers` с таблицей тактов `clocks`.
    ///
    /// Отказы:
    /// - [`Refusal::Window`] - окно меньше регистров тактов таблицы.
    pub fn new(registers: R, clocks: &'static [Clock], id: DeviceId) -> Result<Self, Refusal> {
        let last = clocks.iter().map(last_register).max().unwrap_or_default();
        if registers.size() < last + REGISTER_BYTES {
            return Err(Refusal::Window);
        }

        Ok(Self {
            registers,
            clocks,
            id,
        })
    }

    /// Включает такты узла `device` и занимает узел в `claims` сессией `badge`. Записи `clocks`
    /// узла проверяются до первой записи регистров. Узел сессии `badge` включён повторно без
    /// записей. Ветви, включённые до отказа, остаются включёнными.
    ///
    /// Отказы в порядке проверки:
    /// - [`ClocksError::NoDevice`] - узла нет в описании либо он недоступен;
    /// - [`ClocksError::Busy`] - узел занят другой сессией;
    /// - [`ClocksError::Unsupported`] - запись тактов узла повреждена;
    /// - [`ClocksError::NoDevice`] - узел не называет тактов контроллера;
    /// - [`ClocksError::Unsupported`] - такт контроллера вне таблицы;
    /// - [`ClocksError::Timeout`] - ветвь не сообщила о работе.
    pub fn enable(
        &self,
        description: &Description<'_>,
        claims: &mut DeviceClaims,
        badge: u64,
        device: &DeviceId,
    ) -> Result<(), ClocksError> {
        let node = description.node(device).ok_or(ClocksError::NoDevice)?;
        match claims.holder(device) {
            Some(holder) if holder == badge => return Ok(()),
            Some(_) => return Err(ClocksError::Busy),
            None => {}
        }

        let own: Vec<_> = records(&node)?
            .into_iter()
            .filter(|record| record.controller == self.id)
            .collect();
        if own.is_empty() {
            return Err(ClocksError::NoDevice);
        }
        let branches = own
            .iter()
            .map(|record| {
                self.clock(&record.cells)
                    .map(|clock| clock.branch)
                    .ok_or(ClocksError::Unsupported)
            })
            .collect::<Result<Vec<_>, _>>()?;

        for branch in &branches {
            self.start(branch)?;
        }
        claims.claim(badge, device.clone());
        Ok(())
    }

    /// Задаёт частоту `hz` такта с именем `name` узла `device` сессии `badge` по таблице его
    /// генератора.
    ///
    /// Отказы:
    /// - [`ClocksError::NoDevice`] - узел не занят сессией `badge` в `claims`;
    /// - [`ClocksError::Unsupported`] - имени, генератора либо частоты нет в таблице, регистры
    ///   не пишутся;
    /// - [`ClocksError::Timeout`] - генератор не сбросил `UPDATE`.
    pub fn set_rate(
        &self,
        description: &Description<'_>,
        claims: &DeviceClaims,
        badge: u64,
        device: &DeviceId,
        name: &str,
        hz: u64,
    ) -> Result<(), ClocksError> {
        if claims.holder(device) != Some(badge) {
            return Err(ClocksError::NoDevice);
        }
        let node = description.node(device).ok_or(ClocksError::NoDevice)?;
        let record = records(&node)?
            .into_iter()
            .find(|record| record.name == Some(name) && record.controller == self.id)
            .ok_or(ClocksError::Unsupported)?;
        let generator = self
            .clock(&record.cells)
            .and_then(|clock| clock.branch.generator)
            .ok_or(ClocksError::Unsupported)?;
        let frequency = generator
            .frequencies
            .iter()
            .find(|frequency| frequency.hz == hz)
            .ok_or(ClocksError::Unsupported)?;

        let fields =
            frequency.divider & DIVIDER_MASK | frequency.source << SOURCE_SHIFT & SOURCE_MASK;
        let mask = DIVIDER_MASK | SOURCE_MASK | MODE_MASK | HW_CLK_CTRL;
        self.update(generator.command + CFG, mask, fields);
        self.update(generator.command, UPDATE, UPDATE);

        if !self.wait(generator.command, UPDATE_POLLS, is_updated) {
            return Err(ClocksError::Timeout);
        }
        Ok(())
    }

    fn clock(&self, cells: &[u32]) -> Option<&'static Clock> {
        let [index] = cells else {
            return None;
        };
        self.clocks.iter().find(|clock| clock.index == *index)
    }

    fn start(&self, branch: &Branch) -> Result<(), ClocksError> {
        match branch.enable {
            Enable::Branch(offset) => self.update(offset, BRANCH_ENABLE, BRANCH_ENABLE),
            Enable::Vote(mask) => self.update(VOTE, mask, mask),
        }
        if self.wait(branch.halt, BRANCH_POLLS, is_running) {
            Ok(())
        } else {
            Err(ClocksError::Timeout)
        }
    }

    fn update(&self, offset: usize, mask: u32, value: u32) {
        let old = self.registers.load(offset);
        let new = old & !mask | value & mask;
        if new != old {
            self.registers.store(offset, new);
        }
    }

    fn wait(&self, offset: usize, polls: usize, done: impl Fn(u32) -> bool) -> bool {
        // Опросы покрывают ожидание Linux - 200 мкс ветви (`clk_branch_wait`) и 500 мкс
        // `UPDATE` (`update_config`), если чтение регистра длится не меньше 10 нс.
        (0..polls).any(|_| done(self.registers.load(offset)))
    }
}

fn is_running(status: u32) -> bool {
    status & CLK_OFF == 0
        || status >> NOC_FSM_STATUS_SHIFT & NOC_FSM_STATUS_MASK == NOC_FSM_STATUS_ON
}

fn is_updated(command: u32) -> bool {
    command & UPDATE == 0
}

fn last_register(clock: &Clock) -> usize {
    let branch = &clock.branch;
    let enable = match branch.enable {
        Enable::Branch(offset) => offset,
        Enable::Vote(_) => VOTE,
    };
    let configuration = branch
        .generator
        .map(|Generator { command, .. }| command + CFG)
        .unwrap_or_default();
    enable.max(branch.halt).max(configuration)
}
