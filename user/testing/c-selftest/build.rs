fn main() -> Result<(), userland_build::Error> {
    userland_build::emit_linker_script()?;
    userland_build::CSources::new("c-selftest", &["src/selftest.c"]).compile()?;
    Ok(())
}
