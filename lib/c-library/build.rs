//! Компонует библиотеку C и компилирует с её заголовками C-исходники крейта.

fn main() -> Result<(), userland_build::Error> {
    userland_build::CSources::new(
        "c-library",
        &["src/errno.c", "src/files.c", "src/pthread.c", "src/time.c"],
    )
    .compile()?;

    userland_build::link_libc()?;
    Ok(())
}
