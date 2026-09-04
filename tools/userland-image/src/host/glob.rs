pub(super) fn glob_matches(glob: &str, path: &str) -> bool {
    matches_bytes(glob.as_bytes(), path.as_bytes())
}

fn matches_bytes(glob: &[u8], path: &[u8]) -> bool {
    if let Some(tail) = glob.strip_prefix(b"**") {
        if let Some(rest) = tail.strip_prefix(b"/")
            && matches_bytes(rest, path)
        {
            return true;
        }
        return suffixes(path, path.len()).any(|rest| matches_bytes(tail, rest));
    }

    match glob.split_first() {
        None => path.is_empty(),
        Some((b'*', tail)) => {
            let component = path
                .iter()
                .position(|byte| *byte == b'/')
                .unwrap_or(path.len());
            suffixes(path, component).any(|rest| matches_bytes(tail, rest))
        }
        Some((b'?', tail)) => match path.split_first() {
            Some((byte, path_tail)) => *byte != b'/' && matches_bytes(tail, path_tail),
            None => false,
        },
        Some((expected, tail)) => match path.split_first() {
            Some((byte, path_tail)) => byte == expected && matches_bytes(tail, path_tail),
            None => false,
        },
    }
}

fn suffixes(path: &[u8], last: usize) -> impl Iterator<Item = &[u8]> {
    (0..=last).filter_map(move |split| path.get(split..))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn star_stops_at_the_separator() {
        assert!(glob_matches("*.txt", "notes.txt"));
        assert!(!glob_matches("*.txt", "nested/notes.txt"));
        assert!(glob_matches("nested/*.txt", "nested/notes.txt"));
    }

    #[test]
    fn double_star_crosses_directories() {
        assert!(glob_matches("**", "nested/deeper/notes.txt"));
        assert!(glob_matches("**/*.txt", "nested/deeper/notes.txt"));
        // Ведущий `**/` принимает и путь без каталогов.
        assert!(glob_matches("**/*.txt", "notes.txt"));
        assert!(!glob_matches("**/*.txt", "notes.md"));
    }

    #[test]
    fn question_matches_one_character_inside_a_component() {
        assert!(glob_matches("seed-?.bin", "seed-3.bin"));
        assert!(!glob_matches("seed-?.bin", "seed-42.bin"));
        assert!(!glob_matches("seed?bin", "seed/bin"));
    }
}
