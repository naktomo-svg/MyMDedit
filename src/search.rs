//! Match finding for the find/replace dialog, over the editor's UTF-16 text.

fn fold(unit: u16, case_sensitive: bool) -> u16 {
    if case_sensitive {
        return unit;
    }
    // Ordinal ignore-case: simple one-to-one uppercase mapping per code unit.
    match char::from_u32(unit as u32) {
        Some(c) => {
            let mut up = c.to_uppercase();
            match (up.next(), up.next()) {
                (Some(u), None) if (u as u32) <= 0xFFFF => u as u32 as u16,
                _ => unit,
            }
        }
        None => unit,
    }
}

fn is_word_char(unit: u16) -> bool {
    char::from_u32(unit as u32).is_some_and(char::is_alphanumeric)
}

pub fn units_equal(a: &[u16], b: &[u16], case_sensitive: bool) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(&x, &y)| fold(x, case_sensitive) == fold(y, case_sensitive))
}

/// Start offsets of every non-overlapping match of `query` in `text`.
pub fn find_all(text: &[u16], query: &[u16], case_sensitive: bool, whole_word: bool) -> Vec<usize> {
    let mut matches = Vec::new();
    if query.is_empty() || query.len() > text.len() {
        return matches;
    }
    let mut pos = 0;
    while pos + query.len() <= text.len() {
        if units_equal(&text[pos..pos + query.len()], query, case_sensitive) {
            let end = pos + query.len();
            let whole = (pos == 0 || !is_word_char(text[pos - 1])) && (end >= text.len() || !is_word_char(text[end]));
            if !whole_word || whole {
                matches.push(pos);
                pos = end;
                continue;
            }
        }
        pos += 1;
    }
    matches
}

/// The first match at or after `from`, wrapping to the first match.
pub fn next(matches: &[usize], from: usize) -> Option<usize> {
    matches.iter().copied().find(|&m| m >= from).or(matches.first().copied())
}

/// The last match before `from`, wrapping to the last match.
pub fn previous(matches: &[usize], from: usize) -> Option<usize> {
    matches.iter().rev().copied().find(|&m| m < from).or(matches.last().copied())
}

/// `text` with every match replaced, plus the replaced span `(start, old_end, new_text)`
/// covering first to last match — so the whole operation is one edit, one undo step.
pub fn replace_all(text: &[u16], matches: &[usize], query_len: usize, replacement: &[u16]) -> Option<(usize, usize, Vec<u16>)> {
    let (&first, &last) = (matches.first()?, matches.last()?);
    let end = last + query_len;
    let mut out = Vec::new();
    let mut cursor = first;
    for &m in matches {
        out.extend_from_slice(&text[cursor..m]);
        out.extend_from_slice(replacement);
        cursor = m + query_len;
    }
    out.extend_from_slice(&text[cursor..end]);
    Some((first, end, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn finds_with_options() {
        let t = w("Cat cat concat CAT");
        assert_eq!(find_all(&t, &w("cat"), false, false), vec![0, 4, 11, 15]);
        assert_eq!(find_all(&t, &w("cat"), true, false), vec![4, 11]);
        assert_eq!(find_all(&t, &w("cat"), false, true), vec![0, 4, 15]);
        assert_eq!(find_all(&w("日本語の日本"), &w("日本"), false, true), Vec::<usize>::new());
    }

    #[test]
    fn wraps_around() {
        let m = [2, 8];
        assert_eq!(next(&m, 9), Some(2));
        assert_eq!(previous(&m, 2), Some(8));
        assert_eq!(previous(&m, 5), Some(2));
    }

    #[test]
    fn replaces_all_in_one_span() {
        let t = w("a-b-c");
        let m = find_all(&t, &w("-"), true, false);
        let (s, e, new) = replace_all(&t, &m, 1, &w("+=")).unwrap();
        assert_eq!((s, e), (1, 4));
        assert_eq!(String::from_utf16_lossy(&new), "+=b+=");
    }
}
