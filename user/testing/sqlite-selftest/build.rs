use std::env;

use userland_build::{CSources, Error};

const SQLITE_INCLUDE: &str = "DEP_SQLITE3_INCLUDE";

fn main() -> Result<(), Error> {
    userland_build::emit_linker_script()?;
    let include = env::var(SQLITE_INCLUDE).map_err(|_| Error::MissingVariable(SQLITE_INCLUDE))?;
    CSources::new("sqlite-selftest", &["src/selftest.c"])
        .include_dir(include)
        .compile()
}
