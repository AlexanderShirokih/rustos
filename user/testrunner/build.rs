fn main() {
    userland_build::attach_linker_script("link.ld");
    println!("cargo::rerun-if-env-changed=RUSTOS_TEST_PROFILE");
    println!("cargo::rerun-if-env-changed=RUSTOS_STRESS_SEED");
    println!("cargo::rerun-if-env-changed=RUSTOS_STRESS_SEEDS");
    println!("cargo::rerun-if-env-changed=RUSTOS_STRESS_STEPS");
    export_or_default("RUSTOS_TEST_PROFILE", "regular");
    export_or_default("RUSTOS_STRESS_SEED", "1");
    export_or_default("RUSTOS_STRESS_SEEDS", "4");
    export_or_default("RUSTOS_STRESS_STEPS", "64");
}

fn export_or_default(name: &str, default: &str) {
    let value = std::env::var(name).unwrap_or_else(|_| default.to_owned());
    println!("cargo::rustc-env={name}={value}");
}
