fn main() {
    userland_build::emit_linker_script();
    println!("cargo::rerun-if-env-changed=TEST_PROFILE");
    println!("cargo::rerun-if-env-changed=STRESS_SEED");
    println!("cargo::rerun-if-env-changed=STRESS_SEEDS");
    println!("cargo::rerun-if-env-changed=STRESS_STEPS");
    export_or_default("TEST_PROFILE", "regular");
    export_or_default("STRESS_SEED", "1");
    export_or_default("STRESS_SEEDS", "4");
    export_or_default("STRESS_STEPS", "64");
}

fn export_or_default(name: &str, default: &str) {
    let value = std::env::var(name).unwrap_or_else(|_| default.to_owned());
    println!("cargo::rustc-env={name}={value}");
}
