/// Сообщает, содержит ли фрагмент кириллицу.
pub fn has_cyrillic(text: &str) -> bool {
    text.chars().any(is_cyrillic)
}

/// Сообщает, принадлежит ли символ кириллическому блоку.
pub fn is_cyrillic(ch: char) -> bool {
    matches!(ch, '\u{0400}'..='\u{04ff}')
}

/// Возвращает маску: для каждого байта — лежит ли он внутри обратных кавычек.
pub fn backtick_mask(text: &str) -> Vec<bool> {
    let mut mask = vec![false; text.len()];
    let mut open: Option<usize> = None;
    for (offset, ch) in text.char_indices() {
        if ch != '`' {
            continue;
        }
        match open {
            None => open = Some(offset),
            Some(start) => {
                for cell in mask.iter_mut().take(offset + 1).skip(start) {
                    *cell = true;
                }
                open = None;
            }
        }
    }
    mask
}

/// Заменяет содержимое обратных кавычек пробелами, сохраняя байтовые смещения.
pub fn mask_backticks(text: &str) -> String {
    let mask = backtick_mask(text);
    let mut out = String::with_capacity(text.len());
    for (offset, ch) in text.char_indices() {
        if mask.get(offset).copied().unwrap_or_default() {
            for _ in 0..ch.len_utf8() {
                out.push(' ');
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Ширина строки в символах.
pub fn width(line: &str) -> usize {
    line.chars().count()
}

/// Схлопывает пробельные последовательности, давая устойчивый к переносам ключ.
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(ch);
    }
    out
}

/// Короткий отпечаток текста, устойчивый к переносу и к соседним находкам.
pub fn fingerprint(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in normalize(text).bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    #[allow(clippy::cast_possible_truncation)]
    let fingerprint = hash as u32;
    format!("{fingerprint:08x}")
}
