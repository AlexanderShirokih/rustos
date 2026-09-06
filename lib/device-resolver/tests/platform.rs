//! Обход описания и отказы резолвера на биндинге, объявленном самим тестом:
//! нейтральному крейту незачем знать контроллеры платформы. Первая ячейка
//! specifier - номер линии, вторая - триггер.

#![allow(clippy::cast_possible_truncation, clippy::expect_used)]

use device_resolver::{
    ControllerBinding, ControllerBindings, ControllerId, DeviceInterrupt, DeviceMmio,
    DeviceSelector, DeviceTreePlatform, InterruptLine, InterruptSpecifier, PlatformDescription,
    ResolveError,
};
use fdt::devicetree::DeviceTree;
use syscall::IrqTrigger;

const CONTROLLER_PHANDLE: u32 = 0x8001;

const TEST_CONTROLLER: &str = "test,controller";
const TEST_SPECIFIER_CELLS: usize = 2;
const TEST_TRIGGER_EDGE: u32 = 1;

const TEST_BINDINGS: ControllerBindings = ControllerBindings::new(&[ControllerBinding::new(
    &[TEST_CONTROLLER],
    decode_test_specifier,
)]);

const CLOCK: &str = "test,clock";
const SERIAL: &str = "test,serial";
const NESTED_CLOCK: &str = "test,soc-clock";
const BRIDGED_CLOCK: &str = "test,bridged-clock";

fn decode_test_specifier(specifier: InterruptSpecifier<'_>) -> Option<DeviceInterrupt> {
    if specifier.cell_count() != TEST_SPECIFIER_CELLS {
        return None;
    }
    let trigger = if specifier.cell(1)? == TEST_TRIGGER_EDGE {
        IrqTrigger::Edge
    } else {
        IrqTrigger::Level
    };
    Some(DeviceInterrupt {
        line: InterruptLine::new(
            ControllerId::PLATFORM,
            u16::try_from(specifier.cell(0)?).ok()?,
        ),
        trigger,
    })
}

struct DtbBuilder {
    structure: Vec<u8>,
    strings: Vec<u8>,
}

impl DtbBuilder {
    fn new() -> Self {
        Self {
            structure: Vec::new(),
            strings: Vec::new(),
        }
    }

    fn intern(&mut self, name: &str) -> u32 {
        let needle = format!("{name}\0");
        if let Some(at) = find(&self.strings, needle.as_bytes()) {
            return at as u32;
        }
        let at = self.strings.len() as u32;
        self.strings.extend_from_slice(needle.as_bytes());
        at
    }

    fn begin_node(&mut self, name: &str) -> &mut Self {
        push_u32(&mut self.structure, 0x1);
        self.structure.extend_from_slice(name.as_bytes());
        self.structure.push(0);
        align4(&mut self.structure);
        self
    }

    fn end_node(&mut self) -> &mut Self {
        push_u32(&mut self.structure, 0x2);
        self
    }

    fn prop(&mut self, name: &str, value: &[u8]) -> &mut Self {
        let at = self.intern(name);
        push_u32(&mut self.structure, 0x3);
        push_u32(&mut self.structure, value.len() as u32);
        push_u32(&mut self.structure, at);
        self.structure.extend_from_slice(value);
        align4(&mut self.structure);
        self
    }

    fn cells(&mut self, name: &str, cells: &[u32]) -> &mut Self {
        let mut value = Vec::new();
        for cell in cells {
            push_u32(&mut value, *cell);
        }
        self.prop(name, &value)
    }

    fn strings(&mut self, name: &str, values: &[&str]) -> &mut Self {
        let mut value = Vec::new();
        for text in values {
            value.extend_from_slice(text.as_bytes());
            value.push(0);
        }
        self.prop(name, &value)
    }

    fn build(&mut self) -> Vec<u8> {
        push_u32(&mut self.structure, 0x9);

        let header = 40u32;
        let rsvmap = 16u32;
        let off_struct = header + rsvmap;
        let off_strings = off_struct + self.structure.len() as u32;
        let total = off_strings + self.strings.len() as u32;

        let mut buf = Vec::new();
        push_u32(&mut buf, 0xD00D_FEED);
        push_u32(&mut buf, total);
        push_u32(&mut buf, off_struct);
        push_u32(&mut buf, off_strings);
        push_u32(&mut buf, header);
        push_u32(&mut buf, 17);
        push_u32(&mut buf, 16);
        push_u32(&mut buf, 0);
        push_u32(&mut buf, self.strings.len() as u32);
        push_u32(&mut buf, self.structure.len() as u32);
        buf.extend_from_slice(&[0u8; 16]);
        buf.extend_from_slice(&self.structure);
        buf.extend_from_slice(&self.strings);
        buf
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn push_u32(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_be_bytes());
}

fn align4(buf: &mut Vec<u8>) {
    while !buf.len().is_multiple_of(4) {
        buf.push(0);
    }
}

fn platform_dtb(controller: &str) -> Vec<u8> {
    let mut builder = DtbBuilder::new();
    builder
        .begin_node("")
        .cells("#address-cells", &[2])
        .cells("#size-cells", &[2])
        .cells("interrupt-parent", &[CONTROLLER_PHANDLE]);

    builder
        .begin_node("intc@8000000")
        .strings("compatible", &[controller])
        .cells("#interrupt-cells", &[2])
        .cells("phandle", &[CONTROLLER_PHANDLE])
        .end_node();

    builder
        .begin_node("clock@9010000")
        .strings("compatible", &[CLOCK, "test,primecell"])
        .cells("reg", &[0, 0x0901_0000, 0, 0x1000])
        .cells("interrupts", &[34, 0])
        .end_node();

    builder
        .begin_node("clock@9020000")
        .strings("compatible", &[CLOCK])
        .cells("reg", &[0, 0x0902_0000, 0, 0x1000])
        .cells("interrupts", &[35, 1])
        .end_node();

    builder
        .begin_node("serial@9000000")
        .strings("compatible", &[SERIAL])
        .cells("reg", &[0, 0x0900_0000, 0, 0x1000])
        .end_node();

    // Шина с тождественным отображением. Её потомок читается наравне с
    // устройствами корня, а `interrupt-parent` наследуется от неё.
    builder
        .begin_node("soc")
        .cells("#address-cells", &[2])
        .cells("#size-cells", &[2])
        .prop("ranges", &[])
        .cells("interrupt-parent", &[CONTROLLER_PHANDLE])
        .begin_node("clock@9030000")
        .strings("compatible", &[NESTED_CLOCK])
        .cells("reg", &[0, 0x0903_0000, 0, 0x1000])
        .cells("interrupts", &[36, 0])
        .end_node()
        .end_node();

    // Шина без `ranges` в родительское пространство окна не переносит.
    builder
        .begin_node("bridge")
        .cells("#address-cells", &[2])
        .cells("#size-cells", &[2])
        .begin_node("clock@40000")
        .strings("compatible", &[BRIDGED_CLOCK])
        .cells("reg", &[0, 0x0004_0000, 0, 0x1000])
        .cells("interrupts", &[37, 0])
        .end_node()
        .end_node();

    builder.end_node().build()
}

fn selector(compatible: &str, instance: u16) -> DeviceSelector<'_> {
    DeviceSelector {
        compatible,
        instance,
    }
}

fn with_platform<R>(controller: &str, body: impl FnOnce(&DeviceTreePlatform<'_>) -> R) -> R {
    let bytes = platform_dtb(controller);
    let tree = DeviceTree::from_bytes(&bytes).expect("device tree parses");
    let platform = DeviceTreePlatform::new(&tree, TEST_BINDINGS);
    body(&platform)
}

#[test]
fn a_described_device_yields_its_line_and_window() {
    with_platform(TEST_CONTROLLER, |platform| {
        let interrupt = platform
            .interrupt(selector(CLOCK, 0), 0)
            .expect("clock line is described");
        assert_eq!(interrupt.line.number(), 34);
        assert_eq!(interrupt.line.controller(), ControllerId::PLATFORM);
        assert_eq!(interrupt.trigger, IrqTrigger::Level);

        assert_eq!(
            platform.mmio(selector(CLOCK, 0), 0),
            Ok(DeviceMmio {
                base: 0x0901_0000,
                size: 0x1000,
            })
        );
    });
}

#[test]
fn the_instance_index_picks_among_devices_of_one_compatible() {
    with_platform(TEST_CONTROLLER, |platform| {
        let second = platform
            .interrupt(selector(CLOCK, 1), 0)
            .expect("second clock is described");

        assert_eq!(second.line.number(), 35);
        assert_eq!(second.trigger, IrqTrigger::Edge);
        assert_eq!(
            platform
                .mmio(selector(CLOCK, 1), 0)
                .map(|window| window.base),
            Ok(0x0902_0000)
        );
    });
}

#[test]
fn a_device_outside_the_description_is_named_apart() {
    with_platform(TEST_CONTROLLER, |platform| {
        assert_eq!(
            platform.interrupt(selector("test,absent-device", 0), 0),
            Err(ResolveError::DeviceNotFound)
        );
        assert_eq!(
            platform.mmio(selector("test,absent-device", 0), 0),
            Err(ResolveError::DeviceNotFound)
        );
        // Экземпляр за концом набора совместимых - тот же отказ поиска.
        assert_eq!(
            platform.interrupt(selector(CLOCK, 2), 0),
            Err(ResolveError::DeviceNotFound)
        );
    });
}

#[test]
fn a_device_without_interrupts_is_named_apart() {
    with_platform(TEST_CONTROLLER, |platform| {
        assert_eq!(
            platform.interrupt(selector(SERIAL, 0), 0),
            Err(ResolveError::NoInterrupts)
        );
    });
}

#[test]
fn a_resource_index_past_the_set_is_named_apart() {
    with_platform(TEST_CONTROLLER, |platform| {
        assert_eq!(
            platform.interrupt(selector(CLOCK, 0), 1),
            Err(ResolveError::ResourceOutOfRange)
        );
        assert_eq!(
            platform.mmio(selector(CLOCK, 0), 1),
            Err(ResolveError::ResourceOutOfRange)
        );
    });
}

#[test]
fn an_unknown_controller_is_an_error_and_not_a_default() {
    with_platform("test,absent-controller", |platform| {
        assert_eq!(
            platform.interrupt(selector(CLOCK, 0), 0),
            Err(ResolveError::UnknownController)
        );
        // Окна устройства толкования прерываний не требуют.
        assert!(platform.mmio(selector(CLOCK, 0), 0).is_ok());
    });
}

#[test]
fn a_device_behind_an_identity_bus_is_found_with_its_inherited_controller() {
    with_platform(TEST_CONTROLLER, |platform| {
        let interrupt = platform
            .interrupt(selector(NESTED_CLOCK, 0), 0)
            .expect("nested device is described");
        assert_eq!(interrupt.line.number(), 36);
        assert_eq!(interrupt.trigger, IrqTrigger::Level);

        assert_eq!(
            platform.mmio(selector(NESTED_CLOCK, 0), 0),
            Ok(DeviceMmio {
                base: 0x0903_0000,
                size: 0x1000,
            })
        );
    });
}

#[test]
fn a_window_behind_a_translating_bus_is_refused() {
    with_platform(TEST_CONTROLLER, |platform| {
        // Линия шиной не переносится и читается наравне с остальными.
        assert_eq!(
            platform
                .interrupt(selector(BRIDGED_CLOCK, 0), 0)
                .map(|interrupt| interrupt.line.number()),
            Ok(37)
        );
        assert_eq!(
            platform.mmio(selector(BRIDGED_CLOCK, 0), 0),
            Err(ResolveError::BusTranslation)
        );
    });
}
