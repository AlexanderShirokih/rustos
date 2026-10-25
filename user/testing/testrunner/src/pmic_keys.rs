//! Библиотека кнопок PON `driver-pm8941-pwrkey` на синтетических входах и кнопки PMIC на
//! описанной плате.

use alloc::{string::String, vec::Vec};

use component_abi::RequirementDeclaration;
use driver_pm8941_pwrkey::{Generation, PWRKEY, RESIN, usage};
use input_abi::{
    AttachError, AxisDecl, AxisList, Binding, DeviceDescriptor, DeviceLayout, DeviceNumber,
    KeyList, Kind, Usage, input, input_inject, input_source,
};
use ipc::{CallError, Protocol};
use kernel_objects::Session;
use runtime::{PortTransport, Timeout};
use spmi_abi::DeviceError;

use crate::{
    composition::{Import, Importer},
    rootkeeper_system::{finish, platform_has_compatible, with_drivers},
    source_fixture::with_key_descriptor,
    spmi::{SPMI_IMPORTS, described_device, open, spmi_bus},
};

const INPUT: &str = "input";
const INJECT: &str = "inject";
const SOURCES: &str = "sources";

const IMPORTS: &[Import] = &[
    Import {
        declaration: RequirementDeclaration::session(INPUT, input::Protocol::ID),
        namespace_root: None,
    },
    Import {
        declaration: RequirementDeclaration::session(INJECT, input_inject::Protocol::ID),
        namespace_root: None,
    },
    Import {
        declaration: RequirementDeclaration::all(SOURCES, input_source::Protocol::ID),
        namespace_root: None,
    },
];

const PON: &str = "qcom,pm8998-pon";
const GPIO_KEYS: &str = "gpio-keys";
const TOUCHSCREEN: &str = "novatek,nt36672a-ts";
const TOUCHSCREEN_NAME: &str = "nt36672a-ts";
const TOUCHSCREEN_CONTACTS: u16 = 10;
const TOUCHSCREEN_SIZE: (i32, i32) = (1080, 2340);
const PON_PULL_CTL: u16 = 0x870;

const REPLY_TIMEOUT: Timeout = Timeout::from_secs(15);

type Consumer = input::Client<PortTransport>;

fn layout(name: &str, usage: Usage) -> DeviceLayout {
    with_key_descriptor(name, usage, |descriptor| descriptor.layout())
        .expect("name fits")
        .expect("descriptor is valid")
}

fn touchscreen_layout() -> DeviceLayout {
    let mut axes = AxisList::new();
    for decl in [
        AxisDecl::absolute(Usage::X, 0, TOUCHSCREEN_SIZE.0 - 1),
        AxisDecl::absolute(Usage::Y, 0, TOUCHSCREEN_SIZE.1 - 1),
    ] {
        axes.push(decl).expect("axis fits");
    }
    let (keys, device_axes) = (KeyList::new(), AxisList::new());
    DeviceDescriptor::new(
        TOUCHSCREEN_NAME,
        Binding::Screen,
        &keys,
        &device_axes,
        TOUCHSCREEN_CONTACTS,
        &axes,
    )
    .expect("name fits")
    .layout()
    .expect("descriptor is valid")
}

fn consumer(session: &Session<input::Protocol>) -> Consumer {
    input::Client::new(PortTransport::new(session.handle().as_raw())).with_wait(REPLY_TIMEOUT)
}

fn added(consumer: &Consumer, layouts: &[DeviceLayout]) -> Vec<Option<DeviceNumber>> {
    let mut numbers = alloc::vec![None; layouts.len()];

    while numbers.iter().any(Option::is_none) {
        let Ok(records) = consumer.next().wait() else {
            break;
        };

        for device in records
            .records()
            .filter(|record| record.kind == Kind::DeviceAdded)
            .filter_map(|record| record.device)
        {
            let described = consumer.describe(device).wait();
            let found = layouts
                .iter()
                .position(|layout| described.as_ref() == Ok(layout));
            if let Some(slot) = found.and_then(|index| numbers.get_mut(index)) {
                *slot = Some(device);
            }
        }
    }

    numbers
}

fn pull_up_bits(
    importer: &Importer,
    (pon, controller): &(String, Vec<String>),
) -> Result<u8, CallError<DeviceError>> {
    let bus = spmi_bus(importer, controller);
    let pon = open(&bus, pon).expect("pon session");
    let value = pon.read(PON_PULL_CTL, 1).wait()?;
    Ok(u8::try_from(value).unwrap_or_default() & (PWRKEY.pull_up_bit | RESIN.pull_up_bit))
}

#[test_harness::test]
fn a_key_comes_from_the_linux_code_or_is_power_without_it() {
    test_harness::check_eq!(usage(Some(114)), Ok(Usage::VOLUME_DOWN));
    test_harness::check_eq!(usage(None), Ok(Usage::POWER));
}

#[test_harness::test]
fn a_debounce_time_is_encoded_for_the_generation_of_the_block() {
    let generations = [1, 4].map(Generation::of);
    let default = generations.map(|generation| generation.debounce_field(15_625));
    let refused =
        generations.map(|generation| [0, 2_000_001].map(|us| generation.debounce_field(us)));

    test_harness::check_eq!(generations, [Generation::First, Generation::Second]);
    test_harness::check_eq!(default, [Some(0), Some(8)]);
    test_harness::check_eq!(refused, [[None, None], [None, None]]);
    test_harness::check_eq!(Generation::Second.debounce_field(61), Some(0));
    test_harness::check_eq!(Generation::Second.debounce_field(2_000_000), Some(0xF));
    test_harness::check_eq!(Generation::Second.debounce_window(8), 15_625_000);
    test_harness::check_eq!(Generation::First.debounce_window(0), 15_625_000);
    test_harness::check_eq!(Generation::Second.debounce_window(0xF), 2_000_000_000);
}

#[test_harness::test]
fn the_board_buttons_are_attached_input_sources_with_their_keys() {
    if !platform_has_compatible(PWRKEY.compatible) {
        test_harness::skip!("{} is not described", PWRKEY.compatible);
    }

    let pon = described_device(PON).expect("pon described");
    let mut layouts = alloc::vec![
        layout(PWRKEY.name, Usage::POWER),
        layout(RESIN.name, Usage::VOLUME_DOWN),
    ];
    // Повторный `attach` ниже требует `Busy`, и он не должен опередить подключение хабом.
    if platform_has_compatible(GPIO_KEYS) {
        layouts.push(layout(GPIO_KEYS, Usage::VOLUME_UP));
    }
    if platform_has_compatible(TOUCHSCREEN) {
        layouts.push(touchscreen_layout());
    }

    let composition = with_drivers();
    let importer = composition.importer(IMPORTS);
    let input = importer.session::<input::Protocol>(INPUT);

    let numbers = added(&consumer(&input), &layouts);

    let sources = importer.found::<input_source::Protocol>(SOURCES);
    let reattached: Vec<_> = sources
        .iter()
        .map(|found| {
            let source = {
                let executor = crate::executor::executor();
                executor.block_on(component_startup::open::<input_source::Protocol>(
                    found,
                    &executor.reactor(),
                    REPLY_TIMEOUT,
                ))
            }
            .expect("source opens")
            .client()
            .with_wait(REPLY_TIMEOUT);
            source
                .attach(importer.session::<input_inject::Protocol>(INJECT))
                .wait()
        })
        .collect();

    let pull_up = pull_up_bits(&composition.importer(SPMI_IMPORTS), &pon);

    test_harness::check!(numbers.iter().all(Option::is_some));
    test_harness::check!(!reattached.is_empty());
    test_harness::check!(
        reattached
            .iter()
            .all(|attach| *attach == Err(CallError::Remote(AttachError::Busy)))
    );
    test_harness::check_eq!(pull_up, Ok(PWRKEY.pull_up_bit | RESIN.pull_up_bit));

    drop(input);
    finish(composition);
}
