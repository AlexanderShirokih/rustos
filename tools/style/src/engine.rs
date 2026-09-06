use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
};

use anyhow::{Result, bail};

pub use crate::changeset::Selection;
use crate::{
    anchor::item_path,
    baseline::{self, Baseline},
    changeset::Changeset,
    config::Config,
    report::{Format, Report, sort},
    rule::{Finding, Rule, Severity},
    rules::catalog,
    scope::{Context, MacroBodies},
    settings::Settings,
    source::Source,
    span::Span,
    suppress::Suppressions,
    text::fingerprint,
    workspace::Workspace,
};

/// Параметры прогона.
pub struct Run {
    /// Директория, от которой ищется корень репозитория.
    pub from: PathBuf,
    /// Что считать изменением.
    pub selection: Selection,
    /// Перезаписать снапшот вместо сверки с ним.
    pub write_baseline: bool,
    /// Учитывать снапшот при выводе.
    pub use_baseline: bool,
    /// Формат вывода.
    pub format: Format,
    /// Ограничение прогона перечисленными правилами.
    pub only: Vec<String>,
}

/// Результат прогона.
pub struct Outcome {
    /// Находки, оставшиеся после всех фильтров.
    pub reports: Vec<Report>,
    /// Строки подвала: что пропущено и почему.
    pub notes: Vec<String>,
    /// Код возврата.
    pub code: i32,
}

/// Прогон каталога по дереву.
pub struct Engine;

impl Engine {
    /// Выполняет прогон целиком и возвращает готовый к печати результат.
    pub fn run(run: &Run) -> Result<Outcome> {
        let workspace = Workspace::discover(&run.from)?;
        let changeset = Changeset::collect(&workspace, run.selection.clone())?;

        let rustfmt = changeset.read(&workspace, Path::new("rustfmt.toml"));
        let settings = Arc::new(Settings::from_rustfmt(rustfmt.as_deref())?);

        let compiled = catalog::compiled();
        let declaration = changeset.read(&workspace, Path::new("style.toml"));
        let mut config = match declaration.as_deref() {
            Some(text) => Config::parse(text, &compiled)?,
            None => Config::empty(),
        };
        let catalog = config.catalog(compiled);

        let active: Vec<&Rule> = catalog
            .iter()
            .filter(|rule| run.only.is_empty() || run.only.contains(&rule.meta.id))
            .filter(|rule| config.level(&rule.meta).is_some())
            .collect();
        if !run.only.is_empty() {
            for id in &run.only {
                if !catalog.iter().any(|rule| rule.meta.id == *id) {
                    bail!("unknown rule id: {id}");
                }
            }
        }

        let files = if changeset.is_filtering() {
            changeset.paths()
        } else {
            workspace.files(false)?
        };

        let strict = matches!(run.selection, Selection::All);
        let known: BTreeSet<String> = catalog.iter().map(|rule| rule.meta.id.clone()).collect();
        let running: BTreeSet<String> = active.iter().map(|rule| rule.meta.id.clone()).collect();
        let scan = scan(Sweep {
            workspace: &workspace,
            changeset: &changeset,
            config: &config,
            active: &active,
            settings: &settings,
            files,
            strict,
            known: &known,
            running: &running,
        })?;
        let mut notes = scan.notes;
        let mut reports = scan.reports;
        sort(&mut reports);
        number(&mut reports);

        let seen: BTreeSet<String> = reports.iter().map(Report::key).collect();

        if run.write_baseline {
            Baseline::store(&workspace.root, &seen)?;
            return Ok(Outcome {
                reports: Vec::new(),
                notes: vec![format!("baseline written: {} entries", seen.len())],
                code: 0,
            });
        }

        let baseline = if run.use_baseline {
            changeset
                .read(&workspace, Path::new(baseline::FILE_NAME))
                .map_or_else(Baseline::empty, |text| Baseline::parse(&text))
        } else {
            Baseline::empty()
        };

        let stale_check =
            run.use_baseline && run.only.is_empty() && matches!(run.selection, Selection::All);
        if stale_check {
            if burn_out(&baseline, &seen, &catalog, &config, &mut notes) {
                return Ok(Outcome {
                    reports: Vec::new(),
                    notes,
                    code: 2,
                });
            }
        } else if run.use_baseline && !baseline.is_empty() {
            notes.push(format!(
                "baseline applied ({} entries), burn-out check runs only with --all",
                baseline.len()
            ));
        }

        let mut remaining: Vec<Report> = Vec::new();
        for report in reports {
            if baseline.contains(&report.key()) {
                continue;
            }
            if !changeset.touches(&report.path, report.position.line, report.position.line) {
                continue;
            }
            remaining.push(report);
        }

        explain(&catalog, &remaining, &mut notes);

        let code = i32::from(
            remaining
                .iter()
                .any(|report| report.severity == Severity::Deny),
        );

        Ok(Outcome {
            reports: remaining,
            notes,
            code,
        })
    }
}

impl Outcome {
    /// Печатает результат в выбранном формате.
    pub fn render(&self, format: Format) -> String {
        match format {
            Format::Text => {
                let mut out = String::new();
                for report in &self.reports {
                    out.push_str(&report.line());
                    out.push('\n');
                }
                for note in &self.notes {
                    out.push_str("note: ");
                    out.push_str(note);
                    out.push('\n');
                }
                out
            }
            Format::Json => {
                let body = serde_json::json!({
                    "findings": self.reports.iter().map(Report::json).collect::<Vec<_>>(),
                    "notes": self.notes,
                    "code": self.code,
                });
                format!("{body:#}\n")
            }
        }
    }
}

fn explain(catalog: &[Rule], reports: &[Report], notes: &mut Vec<String>) {
    let fired: BTreeSet<&str> = reports.iter().map(|report| report.rule.as_str()).collect();
    for rule in catalog {
        if fired.contains(rule.meta.id.as_str()) {
            notes.push(format!("{}: {}", rule.meta.id, rule.meta.explain));
        }
    }
}

fn burn_out(
    baseline: &Baseline,
    seen: &BTreeSet<String>,
    catalog: &[Rule],
    config: &Config,
    notes: &mut Vec<String>,
) -> bool {
    let burned = baseline.burned_out(seen);
    let blocking: Vec<&String> = burned
        .iter()
        .filter(|key| level_of(catalog, config, key) == Some(Severity::Deny))
        .collect();
    if blocking.is_empty() {
        if !burned.is_empty() {
            notes.push(format!(
                "{} warn entries burned out of the baseline",
                burned.len()
            ));
        }
        return false;
    }
    for key in blocking.iter().take(10) {
        notes.push(format!("baseline entry no longer reproduces: {key}"));
    }
    notes.push(format!(
        "{} deny entries burned out, regenerate with --write-baseline",
        blocking.len()
    ));
    true
}

struct Sweep<'a> {
    workspace: &'a Workspace,
    changeset: &'a Changeset,
    config: &'a Config,
    active: &'a [&'a Rule],
    settings: &'a Arc<Settings>,
    files: Vec<PathBuf>,
    strict: bool,
    known: &'a BTreeSet<String>,
    running: &'a BTreeSet<String>,
}

struct Scan {
    reports: Vec<Report>,
    notes: Vec<String>,
}

#[derive(Clone, Copy)]
struct Pass<'a> {
    workspace: &'a Workspace,
    changeset: &'a Changeset,
    config: &'a Config,
    active: &'a [&'a Rule],
    settings: &'a Arc<Settings>,
    strict: bool,
    vocabulary: Vocabulary<'a>,
}

struct Part {
    reports: Vec<Report>,
    directives: Vec<Report>,
    notes: Vec<String>,
}

fn scan(sweep: Sweep) -> Result<Scan> {
    let Sweep {
        workspace,
        changeset,
        config,
        active,
        settings,
        files,
        strict,
        known,
        running,
    } = sweep;
    let pass = Pass {
        workspace,
        changeset,
        config,
        active,
        settings,
        strict,
        vocabulary: Vocabulary { known, running },
    };

    // Файлы разбираются независимо друг от друга, а разбор занимает большую часть прогона.
    // Дерево делится на доли по числу ядер. Доли складываются в порядке деления, и порядок
    // файлов внутри доли сохраняется, поэтому отчёт не зависит от числа потоков.
    let size = files.len().div_ceil(shares(files.len())).max(1);
    let parts: Vec<Result<Part>> = thread::scope(|scope| {
        let handles: Vec<_> = files
            .chunks(size)
            .map(|share| scope.spawn(move || sweep_share(pass, share)))
            .collect();
        handles.into_iter().map(join).collect()
    });

    let mut reports: Vec<Report> = Vec::new();
    let mut directives: Vec<Report> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    for part in parts {
        let mut part = part?;
        reports.append(&mut part.reports);
        directives.append(&mut part.directives);
        notes.append(&mut part.notes);
    }

    reports.append(&mut directives);
    Ok(Scan { reports, notes })
}

fn shares(files: usize) -> usize {
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    cores.min(files).max(1)
}

fn join<T>(handle: thread::ScopedJoinHandle<'_, T>) -> T {
    match handle.join() {
        Ok(value) => value,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

fn sweep_share(pass: Pass, files: &[PathBuf]) -> Result<Part> {
    let Pass {
        workspace,
        changeset,
        config,
        active,
        settings,
        strict,
        vocabulary,
    } = pass;
    let mut reports: Vec<Report> = Vec::new();
    let mut directives: Vec<Report> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    for path in files {
        let meta = workspace.meta(path);
        let matching: Vec<&&Rule> = active
            .iter()
            .filter(|rule| rule.applies.scope.matches(&meta))
            .filter(|rule| !config.is_exempt(&rule.meta.id, path))
            .collect();
        if matching.is_empty() {
            continue;
        }
        let Some(text) = changeset.read(workspace, path) else {
            continue;
        };
        let source = Source::new(meta, &text, Arc::clone(settings));
        let is_rust_source = path.extension().and_then(|ext| ext.to_str()) == Some("rs");
        if is_rust_source && source.parsed().errors > 0 {
            if strict {
                bail!(
                    "{} does not parse as Rust, style rules cannot be applied",
                    path.display()
                );
            }
            notes.push(format!(
                "{} does not parse as Rust, skipped",
                path.display()
            ));
            continue;
        }

        let mut suppressions = Suppressions::collect(&source);
        let mut file_reports: Vec<Report> = Vec::new();

        for rule in matching {
            let Some(level) = config.level(&rule.meta) else {
                continue;
            };
            let mut findings: Vec<Finding> = Vec::new();
            rule.run(&source, &mut findings);
            for finding in findings {
                if !passes(rule, &source, finding.at) || suppressions.is_directive_text(finding.at)
                {
                    continue;
                }
                file_reports.push(make(&source, path, &rule.meta.id, level, &finding));
            }
        }

        directives.extend(directive_findings(
            &source,
            path,
            &mut suppressions,
            vocabulary,
            &mut file_reports,
        ));
        reports.append(&mut file_reports);
    }

    Ok(Part {
        reports,
        directives,
        notes,
    })
}

pub(crate) fn passes(rule: &Rule, source: &Source, at: Span) -> bool {
    if rule.applies.macro_bodies == MacroBodies::Exclude && source.in_macro_body(at) {
        return false;
    }
    match rule.applies.context {
        Context::Any => true,
        Context::Prod => !source.is_test(at),
        Context::Test => source.is_test(at),
    }
}

fn make(source: &Source, path: &Path, rule: &str, level: Severity, finding: &Finding) -> Report {
    let text = source
        .text
        .get(finding.at.start as usize..finding.at.end as usize)
        .unwrap_or_default();
    Report {
        rule: rule.to_owned(),
        severity: level,
        path: path.to_path_buf(),
        at: finding.at,
        position: source.lines().position(&source.text, finding.at.start),
        item: item_path(source, finding.at),
        fingerprint: fingerprint(text),
        occurrence: 1,
        message: finding.message.clone(),
    }
}

fn number(reports: &mut [Report]) {
    let mut seen: BTreeMap<String, u32> = BTreeMap::new();
    for report in reports {
        let base = format!(
            "{}|{}|{}|{}",
            report.rule,
            report.path.to_string_lossy(),
            report.item,
            report.fingerprint
        );
        let counter = seen.entry(base).or_insert(0);
        *counter += 1;
        report.occurrence = *counter;
    }
}

#[derive(Clone, Copy)]
struct Vocabulary<'a> {
    known: &'a BTreeSet<String>,
    running: &'a BTreeSet<String>,
}

fn directive_findings(
    source: &Source,
    path: &Path,
    suppressions: &mut Suppressions,
    vocabulary: Vocabulary,
    file_reports: &mut Vec<Report>,
) -> Vec<Report> {
    file_reports.retain(|report| !suppressions.covers(&report.rule, report.at));

    let mut out = Vec::new();
    for zone in &suppressions.zones {
        let position = source.lines().position(&source.text, zone.itself.start);
        let base = Report {
            rule: String::new(),
            severity: Severity::Deny,
            path: path.to_path_buf(),
            at: zone.itself,
            position,
            item: item_path(source, zone.itself),
            fingerprint: fingerprint(zone.rules.join(",").as_str()),
            occurrence: 1,
            message: String::new(),
        };
        if !zone.has_reason {
            out.push(Report {
                rule: "missing_style_reason".to_owned(),
                message: "suppression directive without a written reason".to_owned(),
                ..base.clone()
            });
        }
        for id in &zone.rules {
            if !vocabulary.known.contains(id) {
                out.push(Report {
                    rule: "unknown_style_rule".to_owned(),
                    message: format!("suppression names an unknown rule: {id}"),
                    ..base.clone()
                });
            }
        }
        let judged = zone.rules.iter().all(|id| vocabulary.running.contains(id));
        if !zone.used && judged {
            out.push(Report {
                rule: "unused_style_allow".to_owned(),
                message: "suppression directive covers no finding".to_owned(),
                ..base.clone()
            });
        }
    }
    out
}

/// Правила самого движка: диагностика директив подавления, вне каталога и конфигурации.
pub const ENGINE_RULES: [&str; 3] = [
    "missing_style_reason",
    "unknown_style_rule",
    "unused_style_allow",
];

fn level_of(catalog: &[Rule], config: &Config, key: &str) -> Option<Severity> {
    let id = key.split('|').next()?;
    if ENGINE_RULES.contains(&id) {
        return Some(Severity::Deny);
    }
    let rule = catalog.iter().find(|rule| rule.meta.id == id)?;
    config.level(&rule.meta)
}

#[cfg(test)]
mod tests {
    use crate::{
        engine::explain,
        report::Report,
        rule::Severity,
        rules::catalog,
        span::{Position, Span},
    };

    fn report(rule: &str) -> Report {
        Report {
            rule: rule.to_owned(),
            severity: Severity::Deny,
            path: std::path::PathBuf::from("kernel/demo/src/demo.rs"),
            at: Span::new(0, 1),
            position: Position { line: 1, column: 1 },
            item: "<file-root>".to_owned(),
            fingerprint: "00000000".to_owned(),
            occurrence: 1,
            message: "finding".to_owned(),
        }
    }

    #[test]
    fn fired_rule_appends_its_explanation_once() {
        let catalog = catalog::compiled();
        let reports = vec![report("doc_on_declaration"), report("doc_on_declaration")];
        let mut notes = Vec::new();
        explain(&catalog, &reports, &mut notes);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].starts_with("doc_on_declaration: "), "{notes:?}");
    }

    #[test]
    fn silent_rule_leaves_the_footer_alone() {
        let catalog = catalog::compiled();
        let mut notes = Vec::new();
        explain(&catalog, &[], &mut notes);
        assert!(notes.is_empty(), "{notes:?}");
    }
}
