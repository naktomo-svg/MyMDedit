//! Match finding for the find/replace dialog, over the editor's UTF-16 text.

use std::borrow::Cow;

/// `units` with every character mapped to its simple uppercase, for ordinal ignore-case
/// comparison. Characters, not code units: a surrogate pair is folded as the one letter
/// it encodes. A mapping that would change the UTF-16 length (or a lone surrogate) is
/// left alone, so offsets into the folded text are offsets into `units`.
fn folded(units: &[u16], case_sensitive: bool) -> Cow<'_, [u16]> {
    if case_sensitive {
        return Cow::Borrowed(units);
    }
    let mut out = Vec::with_capacity(units.len());
    for c in char::decode_utf16(units.iter().copied()) {
        match c {
            Ok(c) => {
                let mut up = c.to_uppercase();
                let u = match (up.next(), up.next()) {
                    (Some(u), None) if u.len_utf16() == c.len_utf16() => u,
                    _ => c,
                };
                out.extend_from_slice(u.encode_utf16(&mut [0; 2]));
            }
            Err(e) => out.push(e.unpaired_surrogate()),
        }
    }
    Cow::Owned(out)
}

fn is_word_char(c: Option<char>) -> bool {
    c.is_some_and(char::is_alphanumeric)
}

/// The character ending just before `pos` / starting at `pos`, reading a surrogate pair
/// as one character (a lone surrogate reads as none).
fn char_before(text: &[u16], pos: usize) -> Option<char> {
    let from = if pos >= 2 && (0xDC00..0xE000).contains(&text[pos - 1]) { pos - 2 } else { pos.checked_sub(1)? };
    char::decode_utf16(text[from..pos].iter().copied()).last()?.ok()
}

fn char_at(text: &[u16], pos: usize) -> Option<char> {
    char::decode_utf16(text.get(pos..)?.iter().copied().take(2)).next()?.ok()
}

fn is_whole_word(text: &[u16], start: usize, end: usize) -> bool {
    !is_word_char(char_before(text, start)) && !is_word_char(char_at(text, end))
}

/// Start offsets of every non-overlapping match of `query` in `text`.
pub fn find_all(text: &[u16], query: &[u16], case_sensitive: bool, whole_word: bool) -> Vec<usize> {
    let mut matches = Vec::new();
    if query.is_empty() || query.len() > text.len() {
        return matches;
    }
    let (haystack, query) = (folded(text, case_sensitive), folded(query, case_sensitive));
    // Knuth–Morris–Pratt: linear in the text whatever the query. Comparing at every start
    // offset is O(text × query), which a run of one letter and a long query that almost
    // matches it turns into a visible freeze on the UI thread.
    let fail = failure_table(&query);
    let m = query.len();
    let mut k = 0; // How much of the query matches the text just before `i`.
    for (i, &c) in haystack.iter().enumerate() {
        while k > 0 && query[k] != c {
            k = fail[k - 1];
        }
        if query[k] == c {
            k += 1;
        }
        if k == m {
            let (start, end) = (i + 1 - m, i + 1);
            if !whole_word || is_whole_word(text, start, end) {
                matches.push(start);
                k = 0; // Matches do not overlap.
            } else {
                k = fail[m - 1]; // Not a match after all: a later, overlapping one may be.
            }
        }
    }
    matches
}

/// For each prefix of `query`, the length of its longest proper prefix that is also its
/// suffix: where the match resumes after a mismatch.
fn failure_table(query: &[u16]) -> Vec<usize> {
    let mut fail = vec![0; query.len()];
    let mut k = 0;
    for i in 1..query.len() {
        while k > 0 && query[i] != query[k] {
            k = fail[k - 1];
        }
        if query[i] == query[k] {
            k += 1;
        }
        fail[i] = k;
    }
    fail
}

/// Whether `text[start..start + len]` is exactly a match `find_all` would report with
/// these options — what the Replace button replaces, so it honours "whole word" too.
pub fn is_match_at(
    text: &[u16],
    start: usize,
    len: usize,
    query: &[u16],
    case_sensitive: bool,
    whole_word: bool,
) -> bool {
    let end = start + len;
    len == query.len()
        && end <= text.len()
        && !query.is_empty()
        && *folded(&text[start..end], case_sensitive) == *folded(query, case_sensitive)
        && (!whole_word || is_whole_word(text, start, end))
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
pub fn replace_all(
    text: &[u16],
    matches: &[usize],
    query_len: usize,
    replacement: &[u16],
) -> Option<(usize, usize, Vec<u16>)> {
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

    /// The scan that `find_all` replaced, kept as the definition of what it must return.
    fn every_offset(text: &[u16], query: &[u16], case: bool, word: bool) -> Vec<usize> {
        let (h, q) = (folded(text, case), folded(query, case));
        let (mut out, mut pos) = (Vec::new(), 0);
        while !q.is_empty() && pos + q.len() <= h.len() {
            let end = pos + q.len();
            if h[pos..end] == *q && (!word || is_whole_word(text, pos, end)) {
                out.push(pos);
                pos = end;
            } else {
                pos += 1;
            }
        }
        out
    }

    #[test]
    fn agrees_with_scanning_every_offset() {
        // Self-overlapping queries, and whole-word candidates that fail and leave room for
        // an overlapping one: where a skip-ahead search goes wrong.
        let texts = ["aaaa", "aaab aab ab", "abab abab", "ab ab-abab", "aAbBaAb a", "xaxaxax xax"];
        let queries = ["a", "aa", "aab", "ab", "abab", "bab", "xax", "aAb"];
        for t in texts {
            for q in queries {
                for (case, word) in [(false, false), (true, false), (false, true), (true, true)] {
                    let (t, q) = (w(t), w(q));
                    assert_eq!(find_all(&t, &q, case, word), every_offset(&t, &q, case, word), "{t:?} {q:?}");
                }
            }
        }
        assert_eq!(find_all(&w("aaaa"), &w("aa"), true, false), vec![0, 2]);
    }

    #[test]
    fn a_long_near_miss_query_stays_linear() {
        // O(text × query) would be 10^10 comparisons here; linear finishes at once.
        let text = vec![b'a' as u16; 1_000_000];
        let mut query = vec![b'a' as u16; 9_999];
        query.push(b'b' as u16);
        assert!(find_all(&text, &query, true, false).is_empty());
    }

    #[test]
    fn folds_and_bounds_by_character_not_code_unit() {
        // Deseret capital/small letter: a case pair outside the BMP.
        assert_eq!(find_all(&w("x𐐨y"), &w("𐐀"), false, false), vec![1]);
        assert_eq!(find_all(&w("x𐐨y"), &w("𐐀"), true, false), Vec::<usize>::new());
        // A letter outside the BMP is part of the word around it.
        assert_eq!(find_all(&w("𐐨cat"), &w("cat"), false, true), Vec::<usize>::new());
        assert_eq!(find_all(&w("cat𐐨"), &w("cat"), false, true), Vec::<usize>::new());
        assert_eq!(find_all(&w("😀cat😀"), &w("cat"), false, true), vec![2]);
        // Lone surrogates neither panic nor match a letter.
        assert_eq!(find_all(&[0xD800, b'a' as u16, 0xDC00], &w("A"), false, true), vec![1]);
    }

    #[test]
    fn replace_checks_the_selection_like_find_all() {
        let t = w("category cat");
        assert!(!is_match_at(&t, 0, 3, &w("cat"), false, true), "part of a word");
        assert!(is_match_at(&t, 0, 3, &w("cat"), false, false));
        assert!(is_match_at(&t, 9, 3, &w("CAT"), false, true));
        assert!(!is_match_at(&t, 9, 3, &w("CAT"), true, true));
        assert!(!is_match_at(&t, 9, 2, &w("cat"), false, false), "length differs");
        assert!(!is_match_at(&t, 11, 3, &w("cat"), false, false), "past the end");
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
