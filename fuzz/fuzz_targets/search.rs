#![no_main]
use libfuzzer_sys::fuzz_target;
use mymdedit_fuzz::{reference_find_all, search};

// Find/replace over any text and query: the matches are exactly those an independent
// scan finds (none missed, none extra, word boundaries kept), the Replace button agrees
// with them, next/previous pick the right one, and replace_all's single edit produces
// the text a match-by-match replacement would.
fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else { return };
    let mut parts = s.splitn(3, '\u{1}');
    let (Some(text), Some(query)) = (parts.next(), parts.next()) else { return };
    let replacement: Vec<u16> = parts.next().unwrap_or("").encode_utf16().collect();
    let text: Vec<u16> = text.encode_utf16().collect();
    let query: Vec<u16> = query.encode_utf16().collect();
    for (case, word) in [(false, false), (true, false), (false, true), (true, true)] {
        let m = search::find_all(&text, &query, case, word);
        assert_eq!(m, reference_find_all(&text, &query, case, word), "case {case}, word {word}");
        for &i in &m {
            assert!(search::is_match_at(&text, i, query.len(), &query, case, word));
        }
        let from = m.first().map_or(0, |&i| i + 1);
        assert_eq!(search::next(&m, from), m.iter().copied().find(|&i| i >= from).or(m.first().copied()));
        assert_eq!(search::previous(&m, from), m.iter().rev().copied().find(|&i| i < from).or(m.last().copied()));
        if let Some((start, end, new)) = search::replace_all(&text, &m, query.len(), &replacement) {
            assert!(start <= end && end <= text.len());
            let mut expected = Vec::new();
            let mut cursor = 0;
            for &i in &m {
                expected.extend_from_slice(&text[cursor..i]);
                expected.extend_from_slice(&replacement);
                cursor = i + query.len();
            }
            expected.extend_from_slice(&text[cursor..]);
            assert_eq!([&text[..start], &new[..], &text[end..]].concat(), expected);
        } else {
            assert!(m.is_empty());
        }
    }
});
