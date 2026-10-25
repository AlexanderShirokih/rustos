fn main() -> Result<(), Box<dyn core::error::Error>> {
    userland_build::emit_linker_script()?;
    slint_build::compile("ui/test.slint")?;
    Ok(())
}
