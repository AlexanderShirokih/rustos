use anyhow::{Context, Result, bail, ensure};
use syscall::Timeout;
use userland::USERLAND_PAGE_SIZE;

const MEMORY_SUFFIXES: [(&str, u64); 4] = [
    ("K", 1024),
    ("M", 1024 * 1024),
    ("G", 1024 * 1024 * 1024),
    ("", 1),
];

const DURATION_SUFFIXES: [(&str, u64); 2] = [("ms", 1), ("s", 1_000)];

pub(crate) fn parse_memory_pages(text: &str) -> Result<u64> {
    let (digits, scale) = split_suffix(text, &MEMORY_SUFFIXES).with_context(|| {
        format!(
            "memory limit '{text}' must be a decimal number \
             with an optional 'K', 'M' or 'G' suffix"
        )
    })?;
    let bytes = digits
        .checked_mul(scale)
        .with_context(|| format!("memory limit '{text}' overflows"))?;
    ensure!(bytes != 0, "memory limit '{text}' must be non-zero");
    ensure!(
        bytes.is_multiple_of(USERLAND_PAGE_SIZE),
        "memory limit '{text}' is not a multiple of the {USERLAND_PAGE_SIZE}-byte page"
    );
    Ok(bytes / USERLAND_PAGE_SIZE)
}

pub(crate) fn parse_duration(text: &str) -> Result<Timeout> {
    let (digits, scale) = split_suffix(text, &DURATION_SUFFIXES).with_context(|| {
        format!("duration '{text}' must be a decimal number with an 'ms' or 's' suffix")
    })?;
    let millis = digits
        .checked_mul(scale)
        .with_context(|| format!("duration '{text}' overflows"))?;
    ensure!(millis != 0, "duration '{text}' must be non-zero");
    let timeout = Timeout::from_millis(millis);
    ensure!(
        timeout != Timeout::INFINITE,
        "duration '{text}' is not a finite deadline"
    );
    Ok(timeout)
}

fn split_suffix(text: &str, suffixes: &[(&str, u64)]) -> Result<(u64, u64)> {
    let text = text.trim();
    // Порядок набора значим: `ms` стоит в нём раньше `s` и забирает своё
    // совпадение первым.
    for (suffix, scale) in suffixes {
        let Some(digits) = text.strip_suffix(suffix) else {
            continue;
        };
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            break;
        }
        return Ok((digits.parse()?, *scale));
    }
    bail!("'{text}' is not a number with a known suffix")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_suffixes_scale_by_the_binary_multiplier() {
        assert_eq!(parse_memory_pages("4096").expect("bytes"), 1);
        assert_eq!(parse_memory_pages("4K").expect("kibibytes"), 1);
        assert_eq!(parse_memory_pages("1M").expect("mebibytes"), 256);
        assert_eq!(parse_memory_pages("8M").expect("mebibytes"), 2048);
        assert_eq!(parse_memory_pages("1G").expect("gibibytes"), 262_144);
        assert_eq!(parse_memory_pages(" 8M ").expect("padded"), 2048);
    }

    #[test]
    fn a_memory_limit_below_the_page_is_refused() {
        // Бюджет метрится страницами: доля страницы неотличима от опечатки.
        for text in ["0", "0M", "1", "4095", "5000"] {
            assert!(
                parse_memory_pages(text).is_err(),
                "'{text}' must not compile"
            );
        }
    }

    #[test]
    fn a_memory_limit_outside_the_grammar_is_refused() {
        for text in ["", "M", "8MB", "8m", "-4K", "8 M", "eight"] {
            assert!(
                parse_memory_pages(text).is_err(),
                "'{text}' must not compile"
            );
        }
    }

    #[test]
    fn durations_carry_milliseconds_and_seconds() {
        assert_eq!(
            parse_duration("500ms").expect("millis"),
            Timeout::from_millis(500)
        );
        assert_eq!(
            parse_duration("2s").expect("seconds"),
            Timeout::from_secs(2)
        );
    }

    #[test]
    fn a_duration_outside_the_grammar_is_refused() {
        for text in ["", "2", "0s", "0ms", "2sec", "s", "2 s", "two"] {
            assert!(parse_duration(text).is_err(), "'{text}' must not compile");
        }
    }
}
