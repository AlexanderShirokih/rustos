fn main() {
    userland_build::emit_linker_script();
    println!("cargo::rerun-if-env-changed=TEST_PROFILE");
    export_or_default("TEST_PROFILE", "regular");
}

fn export_or_default(name: &str, default: &str) {
    let value = std::env::var(name).unwrap_or_else(|_| default.to_owned());
    println!("cargo::rustc-env={name}={value}");
}
