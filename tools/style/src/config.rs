use std::{collections::BTreeMap, path::Path};

use anyhow::{Context as _, Result, bail};
use regex::Regex;
use serde::Deserialize;

use crate::{
    rule::{Category, Check, Rule, RuleMeta, Severity, Strip, TextRule, TextTarget},
    scope::{Applies, Context, Domain, MacroBodies, Scope, Selector},
};

/// Разобранная и проверенная конфигурация прогона.
pub struct Config {
    levels: BTreeMap<String, Option<Severity>>,
    exempt: Vec<Exempt>,
    declared: Vec<Rule>,
}

struct Exempt {
    rule: Option<String>,
    prefixes: Vec<String>,
}

impl Config {
    /// Пустая конфигурация: уровни правил берутся из их паспортов.
    pub fn empty() -> Config {
        Config {
            levels: BTreeMap::new(),
            exempt: Vec::new(),
            declared: Vec::new(),
        }
    }

    /// Разбирает и проверяет конфигурацию против каталога.
    pub fn parse(text: &str, compiled: &[Rule]) -> Result<Config> {
        Config::build(
            toml::from_str(text).context("parse style configuration")?,
            compiled,
        )
    }

    fn build(raw: RawConfig, compiled: &[Rule]) -> Result<Config> {
        let declared = raw
            .text
            .into_iter()
            .map(declare)
            .collect::<Result<Vec<Rule>>>()?;

        let mut known: BTreeMap<String, Category> = BTreeMap::new();
        for rule in compiled.iter().chain(declared.iter()) {
            if known
                .insert(rule.meta.id.clone(), rule.meta.category)
                .is_some()
            {
                bail!("duplicate rule id: {}", rule.meta.id);
            }
        }

        let mut levels = BTreeMap::new();
        for (id, level) in raw.rules {
            let Some(category) = known.get(&id) else {
                bail!("unknown rule id in [rules]: {id}");
            };
            let level = match level.as_str() {
                "off" => None,
                "warn" => Some(Severity::Warn),
                "deny" => Some(Severity::Deny),
                other => bail!("unknown level for rule {id}: {other}"),
            };
            if level.is_some_and(|value| value > category.ceiling()) {
                bail!("rule {id} cannot exceed the ceiling of its category");
            }
            levels.insert(id, level);
        }

        let mut exempt = Vec::new();
        for entry in raw.exempt {
            if let Some(id) = &entry.rule
                && !known.contains_key(id)
            {
                bail!("unknown rule id in [[exempt]]: {id}");
            }
            if entry.paths.is_empty() {
                bail!("[[exempt]] without paths");
            }
            exempt.push(Exempt {
                rule: entry.rule,
                prefixes: entry.paths,
            });
        }

        for rule in compiled.iter().chain(declared.iter()) {
            if rule.meta.severity > rule.meta.category.ceiling() {
                bail!(
                    "rule {} declares a level above the ceiling of its category",
                    rule.meta.id
                );
            }
        }

        Ok(Config {
            levels,
            exempt,
            declared,
        })
    }

    /// Возвращает полный каталог: скомпилированные правила плюс объявленные данными.
    pub fn catalog(&mut self, compiled: Vec<Rule>) -> Vec<Rule> {
        let mut catalog = compiled;
        catalog.append(&mut self.declared);
        catalog.sort_by(|left, right| left.meta.id.cmp(&right.meta.id));
        catalog
    }

    /// Возвращает действующий уровень правила: `None` означает выключено.
    pub fn level(&self, rule: &RuleMeta) -> Option<Severity> {
        match self.levels.get(&rule.id) {
            Some(level) => *level,
            None => Some(rule.severity),
        }
    }

    /// Сообщает, исключён ли файл из проверки этим правилом.
    pub fn is_exempt(&self, rule: &str, path: &Path) -> bool {
        let text = path.to_string_lossy();
        self.exempt.iter().any(|entry| {
            entry.rule.as_deref().is_none_or(|want| want == rule)
                && entry
                    .prefixes
                    .iter()
                    .any(|prefix| text.starts_with(prefix.as_str()))
        })
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    rules: BTreeMap<String, String>,
    #[serde(default)]
    exempt: Vec<RawExempt>,
    #[serde(default)]
    text: Vec<RawText>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExempt {
    #[serde(default)]
    rule: Option<String>,
    #[serde(default)]
    paths: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawText {
    id: String,
    title: String,
    message: String,
    pattern: String,
    #[serde(default = "default_target")]
    target: String,
    #[serde(default)]
    require: Vec<String>,
    #[serde(default)]
    unless: Vec<String>,
    #[serde(default)]
    strip: Vec<String>,
    #[serde(default)]
    extension: Option<String>,
    #[serde(default)]
    under: Vec<String>,
    #[serde(default)]
    not_under: Vec<String>,
    #[serde(default)]
    package: Vec<String>,
    #[serde(default)]
    not_package: Vec<String>,
    #[serde(default)]
    file_name: Vec<String>,
    #[serde(default)]
    domain: Vec<String>,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    macro_bodies: Option<String>,
}

fn default_target() -> String {
    "comment".to_owned()
}

fn declare(raw: RawText) -> Result<Rule> {
    let target = match raw.target.as_str() {
        "comment" => TextTarget::Comment,
        "doc" => TextTarget::Doc,
        "line" => TextTarget::Line,
        other => bail!("unknown target for rule {}: {other}", raw.id),
    };
    let strip = raw
        .strip
        .iter()
        .map(|step| match step.as_str() {
            "backticks" => Ok(Strip::Backticks),
            other => bail!("unknown strip step for rule {}: {other}", raw.id),
        })
        .collect::<Result<Vec<Strip>>>()?;

    let context = match raw.context.as_deref() {
        None | Some("any") => Context::Any,
        Some("prod") => Context::Prod,
        Some("test") => Context::Test,
        Some(other) => bail!("unknown context for rule {}: {other}", raw.id),
    };
    let macro_bodies = match raw.macro_bodies.as_deref() {
        None | Some("include") => MacroBodies::Include,
        Some("exclude") => MacroBodies::Exclude,
        Some(other) => bail!("unknown macro_bodies for rule {}: {other}", raw.id),
    };

    let mut selectors = vec![Selector::Extension(
        raw.extension.unwrap_or_else(|| "rs".to_owned()),
    )];
    if !raw.under.is_empty() {
        selectors.push(Selector::UnderAny(raw.under));
    }
    if !raw.not_under.is_empty() {
        selectors.push(Selector::NotUnderAny(raw.not_under));
    }
    if !raw.package.is_empty() {
        selectors.push(Selector::PackageAny(raw.package));
    }
    if !raw.not_package.is_empty() {
        selectors.push(Selector::NotPackageAny(raw.not_package));
    }
    if !raw.file_name.is_empty() {
        selectors.push(Selector::FileNameAny(raw.file_name));
    }
    if !raw.domain.is_empty() {
        let domains = raw
            .domain
            .iter()
            .map(|name| match name.as_str() {
                "kernel" => Ok(Domain::Kernel),
                "library" => Ok(Domain::Library),
                "user" => Ok(Domain::User),
                "tool" => Ok(Domain::Tool),
                other => bail!("unknown domain for rule {}: {other}", raw.id),
            })
            .collect::<Result<Vec<Domain>>>()?;
        selectors.push(Selector::DomainAny(domains));
    }

    Ok(Rule {
        meta: RuleMeta {
            id: raw.id.clone(),
            title: raw.title,
            category: Category::Prose,
            severity: Severity::Warn,
        },
        applies: Applies {
            scope: Scope(selectors),
            context,
            macro_bodies,
        },
        check: Check::Text(TextRule {
            target,
            pattern: compile(&raw.pattern, &raw.id)?,
            require: raw
                .require
                .iter()
                .map(|item| compile(item, &raw.id))
                .collect::<Result<Vec<Regex>>>()?,
            unless: raw
                .unless
                .iter()
                .map(|item| compile(item, &raw.id))
                .collect::<Result<Vec<Regex>>>()?,
            strip,
            message: raw.message,
        }),
    })
}

fn compile(pattern: &str, id: &str) -> Result<Regex> {
    Regex::new(pattern).with_context(|| format!("compile pattern of rule {id}"))
}
