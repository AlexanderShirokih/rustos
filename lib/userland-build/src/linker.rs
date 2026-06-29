use std::{env, path::PathBuf};

/// Передаёт компоновщику относительный путь `name` к linker script.
pub fn attach_linker_script(name: &str) {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let linker_script = manifest_dir.join(name);
    println!("cargo:rerun-if-changed={name}");
    println!("cargo:rustc-link-arg=-T{}", linker_script.display());
}
