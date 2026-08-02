use std::path::PathBuf;

use anyhow::{Result, bail};
use style_tool::{Engine, Format, Run, Selection};

pub enum BaselineMode {
    Apply,
    Ignore,
    Write,
}

pub struct Options {
    pub all: bool,
    pub staged: bool,
    pub since: Option<String>,
    pub baseline: BaselineMode,
    pub format: String,
    pub rules: Vec<String>,
}

const DEFAULT_BASE: &str = "origin/main";

pub fn run(options: &Options) -> Result<()> {
    let selection = match (options.all, options.staged, &options.since) {
        (true, false, None) => Selection::All,
        (false, true, None) => Selection::Staged,
        (false, false, Some(base)) => Selection::Since(base.clone()),
        (false, false, None) => Selection::Since(DEFAULT_BASE.to_owned()),
        _ => bail!("choose one of --all, --staged, --since"),
    };

    if matches!(options.baseline, BaselineMode::Write) && !matches!(selection, Selection::All) {
        bail!("--write-baseline requires --all");
    }

    let format = match options.format.as_str() {
        "text" => Format::Text,
        "json" => Format::Json,
        other => bail!("unknown format: {other}"),
    };

    let outcome = Engine::run(&Run {
        from: PathBuf::from("."),
        selection,
        write_baseline: matches!(options.baseline, BaselineMode::Write),
        use_baseline: !matches!(options.baseline, BaselineMode::Ignore),
        format,
        only: options.rules.clone(),
    })?;

    print!("{}", outcome.render(format));

    if outcome.code != 0 {
        std::process::exit(outcome.code);
    }
    Ok(())
}
