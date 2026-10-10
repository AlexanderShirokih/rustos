use core::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    userland_build::emit_linker_script()?;

    let config = slint_build::CompilerConfiguration::new()
        .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer);
    slint_build::compile_with_config("ui/demo.slint", config)?;
    Ok(())
}
