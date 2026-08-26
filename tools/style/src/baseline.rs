use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use anyhow::{Context as _, Result};

/// Имя снапшота в корне репозитория.
pub const FILE_NAME: &str = "style-baseline.txt";

/// Снапшот принятых нарушений.
pub struct Baseline {
    keys: BTreeSet<String>,
}

impl Baseline {
    /// Пустой снапшот.
    pub fn empty() -> Baseline {
        Baseline {
            keys: BTreeSet::new(),
        }
    }

    /// Разбирает снапшот из текста, одна запись на строку.
    pub fn parse(text: &str) -> Baseline {
        Baseline {
            keys: text
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .map(str::to_owned)
                .collect(),
        }
    }

    /// Записывает снапшот в корень репозитория.
    pub fn store(root: &Path, keys: &BTreeSet<String>) -> Result<()> {
        let path = root.join(FILE_NAME);
        let mut text = String::new();
        for key in keys {
            text.push_str(key);
            text.push('\n');
        }
        std::fs::write(&path, text).with_context(|| format!("write {}", path.display()))
    }

    /// Сообщает, принят ли ключ снапшотом.
    pub fn contains(&self, key: &str) -> bool {
        self.keys.contains(key)
    }

    /// Возвращает записи снапшота, отсутствующие в переданном множестве ключей.
    pub fn burned_out(&self, seen: &BTreeSet<String>) -> Vec<String> {
        self.keys
            .iter()
            .filter(|key| !seen.contains(*key))
            .cloned()
            .collect()
    }

    /// Число принятых записей по каждому правилу.
    pub fn counts_by_rule(&self) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for key in &self.keys {
            let rule = key.split('|').next().unwrap_or(key.as_str());
            *counts.entry(rule.to_owned()).or_insert(0) += 1;
        }
        counts
    }

    /// Число записей.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Снапшот пуст.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}
