//! Такты BLSP GCC SDM660 по `drivers/clk/qcom/gcc-sdm660.c` и
//! `include/dt-bindings/clock/qcom,gcc-sdm660.h` Linux 6.12.

use crate::table::{Branch, Clock, Enable, Frequency, Generator};

const XO: u32 = 0;
const QUP_GENERATOR_OFFSET: usize = 0x18;

const QUP_I2C_FREQUENCIES: &[Frequency] = &[Frequency {
    hz: 19_200_000,
    source: XO,
    divider: 1,
}];

/// Такты BLSP SDM660: ветви AHB BLSP1 и BLSP2 и ветви QUP I2C с генераторами.
pub const SDM660: &[Clock] = &[
    ahb(25, 1 << 17, 0x17004),
    qup_i2c(26, 0x19008),
    qup_i2c(28, 0x1b008),
    qup_i2c(30, 0x1d008),
    qup_i2c(32, 0x1f008),
    ahb(36, 1 << 15, 0x25004),
    qup_i2c(37, 0x26008),
    qup_i2c(39, 0x28008),
    qup_i2c(41, 0x2a008),
    qup_i2c(43, 0x2c008),
];

const fn ahb(index: u32, vote: u32, halt: usize) -> Clock {
    Clock {
        index,
        branch: Branch {
            enable: Enable::Vote(vote),
            halt,
            generator: None,
        },
    }
}

const fn qup_i2c(index: u32, branch: usize) -> Clock {
    Clock {
        index,
        branch: Branch {
            enable: Enable::Branch(branch),
            halt: branch,
            generator: Some(Generator {
                command: branch + QUP_GENERATOR_OFFSET,
                frequencies: QUP_I2C_FREQUENCIES,
            }),
        },
    }
}
