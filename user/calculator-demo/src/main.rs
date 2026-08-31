//! Демо композиции: компонент-клиент двух сервисов и своей файловой области.
//! Читает запись собственного каталога сессией файловой системы, а дальше раз в
//! секунду спрашивает произведение у калькулятора, время у часов реального
//! времени и кладёт обе величины строкой в журнал.

#![feature(molecular_ext)]

use std::{os::molecular, process};

use calculator_demo::calculator;
use component_abi::{component_manifest, supervisor_gone};
use filesystem_abi::{AccessMode, OpenFlags};
use filesystem_client::FileSystem;
use ipc::{Transport as _, wire::IpcError};
use kernel_objects::{OwnedHandle, PortTransport};
use rtc_abi::rtc;
use syscall::Timeout;

const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const TURN: Timeout = Timeout::from_secs(1);

const MULTIPLIER: u64 = 7;

const BANNER_RECORD: &str = "/banner";
const BANNER_MAX: usize = 64;

const NO_STARTUP_EXIT_CODE: i32 = 1;
const CALCULATOR_LOST_EXIT_CODE: i32 = 3;
const CLOCK_LOST_EXIT_CODE: i32 = 4;
const SUPERVISOR_LOST_EXIT_CODE: i32 = 7;

component_manifest! {
    slots: {
        calculator: service(protocol = "Calculator"),
        clock: service(protocol = "Rtc"),
        area: directory(instance = "filesystem-imagefs"),
    },
}

enum Supervisor {
    Alive,
    Gone,
}

fn main() {
    let Some(startup) = molecular::startup() else {
        process::exit(NO_STARTUP_EXIT_CODE);
    };
    let mut slots = startup.slots();
    let (Some(calculator_session), Some(clock_session), Some(area_session), Some(control)) = (
        slots
            .next()
            .and_then(|slot| OwnedHandle::from_raw(slot.get())),
        slots
            .next()
            .and_then(|slot| OwnedHandle::from_raw(slot.get())),
        slots
            .next()
            .and_then(|slot| OwnedHandle::from_raw(slot.get())),
        OwnedHandle::from_raw(startup.control().get()),
    ) else {
        process::exit(NO_STARTUP_EXIT_CODE);
    };

    let calculator =
        calculator::Client::new(PortTransport::client(&calculator_session)).with_wait(CALL_TIMEOUT);
    let clock = rtc::Client::new(PortTransport::client(&clock_session)).with_wait(CALL_TIMEOUT);

    println!("started");
    announce(area_session);
    process::exit(run(&calculator, &clock, &PortTransport::server(&control)))
}

fn announce(area: OwnedHandle) {
    // Запись лежит в собственном каталоге компонента, и его область - корень
    // сессии: имя отсчитывается от неё.
    let mut bytes = [0; BANNER_MAX];
    match banner(area, &mut bytes) {
        Some(text) => println!("banner: {text}"),
        None => println!("banner unread"),
    }
}

fn banner(area: OwnedHandle, bytes: &mut [u8; BANNER_MAX]) -> Option<&str> {
    let mut area = FileSystem::windowed_session(area).with_wait(CALL_TIMEOUT);
    let mut record = area
        .open(BANNER_RECORD, AccessMode::Read, OpenFlags::NONE)
        .ok()?;
    let count = record.read(bytes).ok()?;

    Some(str::from_utf8(bytes.get(..count)?).ok()?.trim_end())
}

fn run(
    calculator: &calculator::Client<PortTransport>,
    clock: &rtc::Client<PortTransport>,
    control: &PortTransport,
) -> i32 {
    let mut step: u64 = 1;
    loop {
        // Уход провайдера виден отказом транспорта на его сессии.
        let Ok(product) = calculator.multiply(step, MULTIPLIER) else {
            println!("calculator gone");
            return CALCULATOR_LOST_EXIT_CODE;
        };
        let Ok(now) = clock.now() else {
            println!("clock gone");
            return CLOCK_LOST_EXIT_CODE;
        };
        println!("{step} * {MULTIPLIER} = {product}, rtc {now}s");
        if matches!(turn(control), Supervisor::Gone) {
            println!("supervisor gone");
            return SUPERVISOR_LOST_EXIT_CODE;
        }
        step = step.wrapping_add(1);
    }
}

fn turn(control: &PortTransport) -> Supervisor {
    // Срок приёма на control-порту задаёт такт цикла, а уход значка supervisor
    // виден отказом того же приёма.
    match control.wait_readable(TURN) {
        // Протокола на control-порту нет: пришедший кадр уходит без ответа, а
        // закрытие reply-объекта будит вызывателя.
        Ok(()) => {
            control.cancel_reply();
            Supervisor::Alive
        }
        // Покадровое исчерпание такт не рвёт, как и уход чужого значка.
        Err(IpcError::Timeout | IpcError::ResourceExhausted) => Supervisor::Alive,
        Err(IpcError::SenderGone) if !supervisor_gone(control.peer_badge()) => Supervisor::Alive,
        Err(_) => Supervisor::Gone,
    }
}
