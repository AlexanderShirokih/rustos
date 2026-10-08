fn main() -> Result<(), userland_build::Error> {
    userland_build::emit_linker_script()?;
    Ok(())
}
