//! Кнопка питания `qcom,pm8941-pwrkey`.

#![no_std]
#![no_main]

use component_startup::component_manifest;
use driver_pm8941_pwrkey::{PWRKEY, Stopped};
use input_abi::input_source;
use platform_abi::platform;
use spmi_abi::spmi;

component_manifest! {
    struct PwrkeyStartup {
        requirements: {
            spmi: session(spmi::Protocol),
            platform: session(platform::Protocol),
        },
        features: ["qcom,pm8941-pwrkey"],
        exports: [input_source::Protocol],
    }
}

#[program_entry::component]
fn run(requirements: PwrkeyStartup) -> Result<(), Stopped> {
    driver_pm8941_pwrkey::run(
        &PWRKEY,
        &requirements.spmi,
        &requirements.platform,
        requirements.parent,
    )
}
