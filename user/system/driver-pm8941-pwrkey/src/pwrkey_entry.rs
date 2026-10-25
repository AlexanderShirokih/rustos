use component_startup::{Requirements as _, component_manifest};
use driver_pm8941_pwrkey::{PWRKEY, Stopped};
use input_abi::input_source;
use platform_abi::platform;
use spmi_abi::spmi;
use syscall::{Handle, kind};

component_manifest! {
    struct PwrkeyStartup {
        requirements: {
            spmi: all(spmi::Protocol),
            platform: session(platform::Protocol),
        },
        features: ["qcom,pm8941-pwrkey"],
        exports: [input_source::Protocol],
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Stopped> {
    let requirements = PwrkeyStartup::from_parent(parent);
    driver_pm8941_pwrkey::run(&PWRKEY, requirements.spmi, &requirements.platform, parent)
}
