#![no_main]
use libfuzzer_sys::fuzz_target;
use mymdedit_fuzz::search;

// Find/replace over any text and query: matches are ordered, do not overlap, lie inside
// the text and really match; replace_all's range stays inside the text.
fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else { return };
    let mut parts = s.splitn(3, '\u{1}');
    let (Some(text), Some(query)) = (parts.next(), parts.next()) else { return };
    let replacement: Vec<u16> = parts.next().unwrap_or("").encode_utf16().collect();
    let text: Vec<u16> = text.encode_utf16().collect();
    let query: Vec<u16> = query.encode_utf16().collect();
    for (case, word) in [(false, false), (true, false), (false, true), (true, true)] {
        let m = search::find_all(&text, &query, case, word);
        assert!(m.windows(2).all(|w| w[0] + query.len() <= w[1]), "overlapping or unordered");
        for &i in &m {
            assert!(i + query.len() <= text.len());
            assert!(search::units_equal(&text[i..i + query.len()], &query, case));
        }
        if let Some((start, end, _)) = search::replace_all(&text, &m, query.len(), &replacement) {
            assert!(start <= end && end <= text.len());
        }
    }
});
