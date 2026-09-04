//! Резолюция устройства платформы через реестр биндингов aarch64: specifier
//! PL031 в описании qemu virt - `GIC_SPI 2 IRQ_TYPE_LEVEL_HIGH`.

#![allow(clippy::cast_possible_truncation, clippy::expect_used)]

use device_resolver::{
    DeviceMmio, DeviceSelector, DeviceTreePlatform, PlatformDescription, ResolveError,
};
use device_resolver_aarch64::PLATFORM_BINDINGS;
use fdt::devicetree::DeviceTree;
use syscall::IrqTrigger;

const GIC_PHANDLE: u32 = 0x8001;

const GIC: &str = "arm,gic-v3";
const PL031: &str = "arm,pl031";

const PL031_BASE: u64 = 0x0901_0000;
const PL031_SIZE: u64 = 0x1000;

const PL031_SPECIFIER: &[u32] = &[0, 2, 4];
const PL031_LINE: u16 = 34;

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
        .cells("interrupt-parent", &[GIC_PHANDLE]);

    builder
        .begin_node("intc@8000000")
        .strings("compatible", &[controller])
        .cells("#interrupt-cells", &[3])
        .cells("phandle", &[GIC_PHANDLE])
        .end_node();

    let reg = [
        0,
        PL031_BASE as u32,
        0,
        u32::try_from(PL031_SIZE).expect("window size fits a cell"),
    ];
    builder
        .begin_node("pl031@9010000")
        .strings("compatible", &[PL031, "arm,primecell"])
        .cells("reg", &reg)
        .cells("interrupts", PL031_SPECIFIER)
        .end_node();

    builder.end_node().build()
}

fn with_platform<R>(controller: &str, body: impl FnOnce(&DeviceTreePlatform<'_>) -> R) -> R {
    let bytes = platform_dtb(controller);
    let tree = DeviceTree::from_bytes(&bytes).expect("device tree parses");
    let platform = DeviceTreePlatform::new(&tree, PLATFORM_BINDINGS);
    body(&platform)
}

fn selector(compatible: &str) -> DeviceSelector<'_> {
    DeviceSelector {
        compatible,
        instance: 0,
    }
}

#[test]
fn a_platform_device_resolves_through_the_gic_binding() {
    with_platform(GIC, |platform| {
        let interrupt = platform
            .interrupt(selector(PL031), 0)
            .expect("pl031 line is described");

        assert_eq!(interrupt.line.number(), PL031_LINE);
        assert_eq!(interrupt.trigger, IrqTrigger::Level);
        assert_eq!(
            platform.mmio(selector(PL031), 0),
            Ok(DeviceMmio {
                base: PL031_BASE,
                size: PL031_SIZE,
            })
        );
    });
}

#[test]
fn a_controller_outside_the_platform_registry_is_an_error() {
    with_platform("riscv,plic0", |platform| {
        assert_eq!(
            platform.interrupt(selector(PL031), 0),
            Err(ResolveError::UnknownController)
        );
    });
}
