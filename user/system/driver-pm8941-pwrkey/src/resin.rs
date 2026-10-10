//! Кнопка сброса `qcom,pm8941-resin`.

#![no_std]
#![no_main]

use component_startup::component_manifest;
use driver_pm8941_pwrkey::{RESIN, Stopped};
use input_abi::input_source;
use platform_abi::platform;
use spmi_abi::spmi;

component_manifest! {
    struct ResinStartup {
        requirements: {
            spmi: session(spmi::Protocol),
            platform: session(platform::Protocol),
        },
        features: ["qcom,pm8941-resin"],
        exports: [input_source::Protocol],
    }
}

#[program_entry::component]
fn run(requirements: ResinStartup) -> Result<(), Stopped> {
    driver_pm8941_pwrkey::run(
        &RESIN,
        &requirements.spmi,
        &requirements.platform,
        requirements.parent,
    )
}
