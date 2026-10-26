//! Кнопки устройства и клавиши возврата.
//!
//! Устройство только из кнопок питания и громкости лаунчер резервирует от сессий фокуса. Громкость
//! вниз и вверх он переводит в стрелки виртуальной клавиатуры, нажатие питания передаёт главному
//! циклу, который отличает короткое нажатие от удержания. Клавиши `Home` и `F1` любого
//! устройства возвращают плитки.

use std::{collections::BTreeMap, sync::mpsc::Sender};

use executor::PortChannel;
use input_abi::{DeviceLayout, DeviceNumber, Kind, Record, Usage, input};
use kernel_objects::SessionTransport;

use crate::{Command, send_waking_loop};

const BUTTONS: [Usage; 3] = [Usage::POWER, Usage::VOLUME_UP, Usage::VOLUME_DOWN];

#[derive(Debug, Clone, Copy)]
pub(crate) enum Arrow {
    Up,
    Down,
}

struct Device {
    layout: DeviceLayout,
    reserved: bool,
}

pub(crate) async fn watch(
    session: &input::Client<PortChannel<SessionTransport>>,
    commands: &Sender<Command>,
) {
    let mut devices = BTreeMap::new();
    loop {
        let records: Vec<Record> = match session.next().await {
            Ok(records) => records.records().collect(),
            Err(error) => {
                println!("buttons stopped: {error:?}");
                return;
            }
        };
        for (index, record) in records.iter().enumerate() {
            let removed_later = |device| {
                records
                    .iter()
                    .skip(index)
                    .any(|later| *later == Record::device_removed(device))
            };
            let command = match (record.kind, record.device) {
                (Kind::Resync, _) => {
                    devices.clear();
                    None
                }
                (Kind::DeviceAdded, Some(number)) => added(session, &mut devices, number).await,
                (Kind::DeviceRemoved, Some(number)) => {
                    devices.remove(&number);
                    None
                }
                (Kind::Key, Some(number)) => devices.get(&number).and_then(|device| {
                    let usage = device.layout.key(record.code)?;
                    let pressed = record.value != 0;
                    // Отпускание из отмены ушедшего источника не считается коротким нажатием.
                    let cancelled = !pressed && removed_later(number);
                    key(usage, pressed, device.reserved, cancelled)
                }),
                _ => None,
            };
            if let Some(command) = command
                && send_waking_loop(commands, command).is_err()
            {
                return;
            }
        }
    }
}

async fn added(
    session: &input::Client<PortChannel<SessionTransport>>,
    devices: &mut BTreeMap<DeviceNumber, Device>,
    number: DeviceNumber,
) -> Option<Command> {
    let layout = session.describe(number).await.ok()?;
    let reserved = only_buttons(&layout);
    devices.insert(number, Device { layout, reserved });
    reserved.then_some(Command::Reserve(number))
}

fn key(usage: Usage, pressed: bool, reserved: bool, cancelled: bool) -> Option<Command> {
    match usage {
        Usage::HOME | Usage::F1 if pressed => Some(Command::Home),
        Usage::VOLUME_DOWN if reserved => Some(Command::Arrow(Arrow::Down, pressed)),
        Usage::VOLUME_UP if reserved => Some(Command::Arrow(Arrow::Up, pressed)),
        Usage::POWER if reserved && cancelled => Some(Command::PowerLost),
        Usage::POWER if reserved => Some(Command::Power(pressed)),
        _ => None,
    }
}

fn only_buttons(layout: &DeviceLayout) -> bool {
    let mut keys = (0..).map_while(|index| layout.key(index)).peekable();
    keys.peek().is_some()
        && keys.all(|usage| BUTTONS.contains(&usage))
        && layout.device_axis(0).is_none()
        && layout.contact_axis(0).is_none()
}
