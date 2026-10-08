fn main() -> Result<(), userland_build::Error> {
    userland_build::emit_linker_script()?;
    userland_build::compile_c("c-selftest", &["src/selftest.c"])?;
    Ok(())
}
