use component_startup::{Requirements as _, component_manifest};
use driver_pm8941_pwrkey::{RESIN, Stopped};
use input_abi::input_source;
use platform_abi::platform;
use spmi_abi::spmi;
use syscall::{Handle, kind};

component_manifest! {
    struct ResinStartup {
        requirements: {
            spmi: all(spmi::Protocol),
            platform: session(platform::Protocol),
        },
        features: ["qcom,pm8941-resin"],
        exports: [input_source::Protocol],
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Stopped> {
    let requirements = ResinStartup::from_parent(parent);
    driver_pm8941_pwrkey::run(&RESIN, requirements.spmi, &requirements.platform, parent)
}
