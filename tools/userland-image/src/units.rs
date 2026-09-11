use anyhow::{Context, Result, bail, ensure};

const MEMORY_SUFFIXES: [(&str, u64); 4] = [
    ("K", 1024),
    ("M", 1024 * 1024),
    ("G", 1024 * 1024 * 1024),
    ("", 1),
];

pub(crate) fn parse_memory_limit(text: &str) -> Result<u64> {
    let (digits, scale) = split_suffix(text).with_context(|| {
        format!(
            "memory limit '{text}' must be a decimal number \
             with an optional 'K', 'M' or 'G' suffix"
        )
    })?;
    let bytes = digits
        .checked_mul(scale)
        .with_context(|| format!("memory limit '{text}' overflows"))?;
    ensure!(bytes != 0, "memory limit '{text}' must be non-zero");
    Ok(bytes)
}

fn split_suffix(text: &str) -> Result<(u64, u64)> {
    let text = text.trim();
    // Пустой суффикс стоит в наборе последним, потому что совпадает с концом любой строки.
    for (suffix, scale) in MEMORY_SUFFIXES {
        let Some(digits) = text.strip_suffix(suffix) else {
            continue;
        };
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            break;
        }
        return Ok((digits.parse()?, scale));
    }
    bail!("'{text}' is not a number with a known suffix")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_suffixes_scale_by_the_binary_multiplier() {
        assert_eq!(parse_memory_limit("4096").expect("bytes"), 4096);
        assert_eq!(parse_memory_limit("4K").expect("kibibytes"), 4096);
        assert_eq!(parse_memory_limit("1M").expect("mebibytes"), 1024 * 1024);
        assert_eq!(
            parse_memory_limit("8M").expect("mebibytes"),
            8 * 1024 * 1024
        );
        assert_eq!(
            parse_memory_limit("1G").expect("gibibytes"),
            1024 * 1024 * 1024
        );
        assert_eq!(parse_memory_limit(" 8M ").expect("padded"), 8 * 1024 * 1024);
    }

    #[test]
    fn a_memory_limit_off_the_page_boundary_is_kept_in_bytes() {
        assert_eq!(parse_memory_limit("1").expect("one byte"), 1);
        assert_eq!(parse_memory_limit("4095").expect("below a page"), 4095);
        assert_eq!(parse_memory_limit("5000").expect("above a page"), 5000);
    }

    #[test]
    fn a_zero_memory_limit_is_refused() {
        for text in ["0", "0M"] {
            assert!(
                parse_memory_limit(text).is_err(),
                "'{text}' must not compile"
            );
        }
    }

    #[test]
    fn a_memory_limit_outside_the_grammar_is_refused() {
        for text in ["", "M", "8MB", "8m", "-4K", "8 M", "eight"] {
            assert!(
                parse_memory_limit(text).is_err(),
                "'{text}' must not compile"
            );
        }
    }
}
