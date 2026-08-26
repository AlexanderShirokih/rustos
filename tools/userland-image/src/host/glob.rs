pub(super) fn glob_matches(glob: &str, path: &str) -> bool {
    matches_bytes(glob.as_bytes(), path.as_bytes())
}

fn matches_bytes(glob: &[u8], path: &[u8]) -> bool {
    match glob.first() {
        None => path.is_empty(),
        Some(b'*') if glob.get(1) == Some(&b'*') => {
            let tail = &glob[2..];
            if tail.first() == Some(&b'/') && matches_bytes(&tail[1..], path) {
                return true;
            }
            (0..=path.len()).any(|split| matches_bytes(tail, &path[split..]))
        }
        Some(b'*') => {
            let component = path
                .iter()
                .position(|byte| *byte == b'/')
                .unwrap_or(path.len());
            (0..=component).any(|split| matches_bytes(&glob[1..], &path[split..]))
        }
        Some(b'?') => {
            matches!(path.first(), Some(byte) if *byte != b'/')
                && matches_bytes(&glob[1..], &path[1..])
        }
        Some(expected) => path.first() == Some(expected) && matches_bytes(&glob[1..], &path[1..]),
    }
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
