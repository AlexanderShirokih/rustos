use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=link.ld");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("none") {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let linker_script = manifest_dir.join("link.ld");
    println!("cargo:rustc-link-arg=-T{}", linker_script.display());
}
