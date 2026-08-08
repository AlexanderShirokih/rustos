use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use anyhow::Result;

use crate::workspace::{Workspace, git};

/// Откуда берётся множество изменённых строк и содержимое файлов.
#[derive(Clone, Debug)]
pub enum Selection {
    /// Всё дерево, фильтра по строкам нет.
    All,
    /// Индекс: содержимое читается из индекса.
    Staged,
    /// Ветка против ревизии, содержимое читается из рабочего дерева.
    Since(String),
}

/// Изменённые строки, переименования и источник содержимого файлов.
pub struct Changeset {
    selection: Selection,
    lines: BTreeMap<PathBuf, BTreeSet<u32>>,
}

impl Changeset {
    /// Собирает множество изменений для выбранного режима.
    pub fn collect(workspace: &Workspace, selection: Selection) -> Result<Changeset> {
        let mut lines: BTreeMap<PathBuf, BTreeSet<u32>> = BTreeMap::new();
        let mut renames: Vec<(PathBuf, PathBuf)> = Vec::new();

        match &selection {
            Selection::All => {}
            Selection::Staged => {
                collect_diff(workspace, &["diff", "--cached"], &mut lines, &mut renames)?;
            }
            Selection::Since(base) => {
                collect_diff(
                    workspace,
                    &["diff", "--merge-base", base],
                    &mut lines,
                    &mut renames,
                )?;
                for path in untracked(workspace)? {
                    lines.entry(path).or_default().insert(u32::MAX);
                }
            }
        }

        Ok(Changeset { selection, lines })
    }

    /// Сообщает, ограничивает ли изменение множество проверяемых файлов.
    pub fn is_filtering(&self) -> bool {
        !matches!(self.selection, Selection::All)
    }

    /// Пути, затронутые изменением.
    pub fn paths(&self) -> Vec<PathBuf> {
        self.lines.keys().cloned().collect()
    }

    /// Сообщает, пересекается ли диапазон строк с изменёнными строками файла.
    pub fn touches(&self, path: &Path, first: u32, last: u32) -> bool {
        if !self.is_filtering() {
            return true;
        }
        let Some(changed) = self.lines.get(path) else {
            return false;
        };
        if changed.contains(&u32::MAX) {
            return true;
        }
        changed.range(first..=last).next().is_some()
    }

    /// Читает содержимое файла из источника, отвечающего режиму.
    pub fn read(&self, workspace: &Workspace, path: &Path) -> Option<String> {
        if matches!(self.selection, Selection::Staged) {
            let spec = format!(":{}", path.to_string_lossy());
            return git(&workspace.root, &["show", &spec]).ok();
        }
        std::fs::read_to_string(workspace.root.join(path)).ok()
    }
}

fn collect_diff(
    workspace: &Workspace,
    head: &[&str],
    lines: &mut BTreeMap<PathBuf, BTreeSet<u32>>,
    renames: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<()> {
    let mut status_args: Vec<&str> = head.to_vec();
    status_args.extend_from_slice(&["--name-status", "-M", "-z", "--ignore-submodules=all"]);
    let status = git(&workspace.root, &status_args)?;
    let fields: Vec<&str> = status.split('\0').filter(|item| !item.is_empty()).collect();

    let mut index = 0;
    let mut touched: Vec<PathBuf> = Vec::new();
    while index < fields.len() {
        let code = fields[index];
        index += 1;
        if code.starts_with('R') || code.starts_with('C') {
            if index + 1 >= fields.len() {
                break;
            }
            let from = PathBuf::from(fields[index]);
            let to = PathBuf::from(fields[index + 1]);
            index += 2;
            renames.push((from, to.clone()));
            touched.push(to);
            continue;
        }
        if index >= fields.len() {
            break;
        }
        let path = PathBuf::from(fields[index]);
        index += 1;
        if code.starts_with('D') {
            lines.entry(path).or_default();
            continue;
        }
        touched.push(path);
    }

    for path in touched {
        let name = path.to_string_lossy().into_owned();
        let mut hunk_args: Vec<&str> = head.to_vec();
        hunk_args.extend_from_slice(&["-U0", "--ignore-submodules=all", "--", &name]);
        let body = git(&workspace.root, &hunk_args)?;
        let entry = lines.entry(path).or_default();
        for number in added_lines(&body) {
            entry.insert(number);
        }
    }

    Ok(())
}

fn added_lines(diff: &str) -> Vec<u32> {
    let mut numbers = Vec::new();
    for line in diff.lines() {
        let Some(rest) = line.strip_prefix("@@ ") else {
            continue;
        };
        let Some(target) = rest.split_whitespace().find(|part| part.starts_with('+')) else {
            continue;
        };
        let target = target.trim_start_matches('+');
        let mut parts = target.split(',');
        let Some(start) = parts.next().and_then(|value| value.parse::<u32>().ok()) else {
            continue;
        };
        let count = parts
            .next()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(1);
        for offset in 0..count {
            numbers.push(start + offset);
        }
    }
    numbers
}

fn untracked(workspace: &Workspace) -> Result<Vec<PathBuf>> {
    let listing = git(
        &workspace.root,
        &["ls-files", "-z", "-o", "--exclude-standard"],
    )?;
    Ok(listing
        .split('\0')
        .filter(|entry| !entry.is_empty())
        .map(PathBuf::from)
        .collect())
}
