//! Проверка кодстайла: разбор режима выборки и формата отчёта отдан clap, сам прогон — движку
//! `style-tool`.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use clap::{Args, ValueEnum};
use style_tool::{Engine, Format, Run, Selection, grown_against};

const DEFAULT_BASE: &str = "origin/main";

#[derive(Args)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is a flag of the command line surface"
)]
pub(crate) struct StyleArgs {
    /// Check the whole tree instead of the changed lines
    #[arg(long, group = "selection")]
    all: bool,
    /// Check the staged contents, for the pre-commit hook
    #[arg(long, group = "selection")]
    staged: bool,
    /// Compare against a revision instead of the default base
    #[arg(long, value_name = "REV", group = "selection")]
    since: Option<String>,
    /// Regenerate the baseline snapshot from the current tree
    #[arg(long, requires = "all")]
    write_baseline: bool,
    /// Report findings accepted by the baseline as well
    #[arg(long, conflicts_with = "write_baseline")]
    no_baseline: bool,
    /// Fail when the baseline accepts more findings than it does at this revision
    #[arg(long, value_name = "REV", conflicts_with = "write_baseline")]
    base: Option<String>,
    /// Output format
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
    /// Restrict the run to the listed rules
    #[arg(long = "rule", value_name = "ID")]
    rules: Vec<String>,
}

#[derive(Clone, Copy, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

impl From<OutputFormat> for Format {
    fn from(format: OutputFormat) -> Self {
        match format {
            OutputFormat::Text => Self::Text,
            OutputFormat::Json => Self::Json,
        }
    }
}

pub(crate) fn run(args: StyleArgs) -> Result<i32> {
    let selection = match (args.all, args.staged, args.since) {
        (true, _, _) => Selection::All,
        (_, true, _) => Selection::Staged,
        (_, _, Some(base)) => Selection::Since(base),
        _ => Selection::Since(DEFAULT_BASE.to_owned()),
    };

    let format = Format::from(args.format);
    let outcome = Engine::run(&Run {
        from: PathBuf::from("."),
        selection,
        write_baseline: args.write_baseline,
        use_baseline: !args.no_baseline,
        format,
        only: args.rules,
    })?;

    print!("{}", outcome.render(format));
    if outcome.code != 0 {
        return Ok(outcome.code);
    }

    if let Some(base) = &args.base {
        compare_baseline(base)?;
    }
    Ok(0)
}

fn compare_baseline(base: &str) -> Result<()> {
    let Some(comparison) = grown_against(Path::new("."), base)? else {
        eprintln!("style: {base} carries no baseline snapshot, nothing to compare");
        return Ok(());
    };

    for rule in &comparison.new {
        eprintln!(
            "{}: {} accepted findings, rule is new against {base}",
            rule.rule, rule.accepted
        );
    }
    for rule in &comparison.grown {
        eprintln!(
            "{}: {} accepted findings, was {}",
            rule.rule, rule.accepted, rule.was
        );
    }
    if !comparison.grown.is_empty() {
        bail!("the baseline snapshot grew against {base}, fix the code instead");
    }
    Ok(())
}
