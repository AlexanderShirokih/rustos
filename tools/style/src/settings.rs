use anyhow::{Context as _, Result};

/// Числа и словари, которыми правила параметризуются вместо литералов в коде.
pub struct Settings {
    /// Предельная ширина строки, источник - `rustfmt.toml`.
    pub max_width: usize,
    /// Предельный объём комментария на тестовой функции.
    pub test_comment_max_lines: usize,
    /// Атрибуты, помечающие функцию как тест.
    pub test_attributes: Vec<String>,
    /// Атрибуты деривации, чей doc принадлежит порождённой поверхности.
    pub doc_bearing_derives: Vec<String>,
}

const DEFAULT_MAX_WIDTH: usize = 100;

impl Settings {
    /// Собирает настройки, беря ширину строки из текста `rustfmt.toml`.
    pub fn from_rustfmt(rustfmt: Option<&str>) -> Result<Settings> {
        let max_width = match rustfmt {
            Some(text) => {
                let table: toml::Table = text.parse().context("parse rustfmt.toml")?;
                table
                    .get("max_width")
                    .and_then(toml::Value::as_integer)
                    .and_then(|value| usize::try_from(value).ok())
                    .unwrap_or(DEFAULT_MAX_WIDTH)
            }
            None => DEFAULT_MAX_WIDTH,
        };

        Ok(Settings {
            max_width,
            test_comment_max_lines: 2,
            test_attributes: vec!["test".to_owned(), "test_harness::test".to_owned()],
            doc_bearing_derives: vec![
                "Parser".to_owned(),
                "Subcommand".to_owned(),
                "Args".to_owned(),
                "ValueEnum".to_owned(),
            ],
        })
    }
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            max_width: DEFAULT_MAX_WIDTH,
            test_comment_max_lines: 2,
            test_attributes: vec!["test".to_owned(), "test_harness::test".to_owned()],
            doc_bearing_derives: vec![
                "Parser".to_owned(),
                "Subcommand".to_owned(),
                "Args".to_owned(),
                "ValueEnum".to_owned(),
            ],
        }
    }
}
