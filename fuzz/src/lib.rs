//! The app's pure-logic modules, compiled from `../src` for the fuzz targets, plus the
//! independent checkers the targets assert with.
#![allow(dead_code)]

#[path = "../../src/document.rs"]
pub mod document;
#[path = "../../src/line_endings.rs"]
pub mod line_endings;
#[path = "../../src/log.rs"]
pub mod log;
#[path = "../../src/md_ops.rs"]
pub mod md_ops;
#[path = "../../src/render.rs"]
pub mod render;
#[path = "../../src/search.rs"]
pub mod search;
#[path = "../../src/settings.rs"]
pub mod settings;

/// Tag names the HTML tokenizer would see, written independently of `render`'s filter:
/// `<` or `</`, an ASCII letter, then everything up to whitespace, `/` or `>`,
/// ASCII-lowercased, NUL read as U+FFFD (HTML Standard, "tag open" / "tag name").
pub fn tag_names(html: &str) -> Vec<String> {
    let chars: Vec<char> = html.chars().collect();
    let mut names = Vec::new();
    for i in 0..chars.len() {
        if chars[i] != '<' {
            continue;
        }
        let mut j = i + 1;
        if chars.get(j) == Some(&'/') {
            j += 1;
        }
        if !chars.get(j).is_some_and(|c| c.is_ascii_alphabetic()) {
            continue;
        }
        let mut name = String::new();
        // CR counts as whitespace: the input stream turns it into LF before tokenizing.
        while let Some(&c) = chars.get(j).filter(|c| !matches!(c, '\t' | '\n' | '\x0c' | '\r' | ' ' | '/' | '>')) {
            name.push(if c == '\0' { '\u{FFFD}' } else { c.to_ascii_lowercase() });
            j += 1;
        }
        names.push(name);
    }
    names
}

pub const FETCHING: [&str; 10] =
    ["base", "embed", "fencedframe", "frame", "frameset", "iframe", "link", "meta", "object", "portal"];

/// `{` that open a CSS block: those outside quoted strings.
pub fn css_blocks(css: &str) -> usize {
    let (mut count, mut quote, mut escaped) = (0, None, false);
    for c in css.chars() {
        match quote {
            Some(_) if escaped => escaped = false,
            Some(_) if c == '\\' => escaped = true,
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '{' => count += 1,
            None => {}
        }
    }
    count
}

/// `#RGB` or `#RRGGBB` and nothing else: the only background color the preview may
/// write into its CSS. Written apart from `settings::is_hex_color`, which it checks.
pub fn is_strict_hex_color(value: &str) -> bool {
    let b = value.as_bytes();
    (b.len() == 4 || b.len() == 7)
        && b[0] == b'#'
        && b[1..].iter().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f' | b'A'..=b'F'))
}

/// Whether `value` holds only what a `font-family` value may: closed quoted strings
/// without a raw newline, bare idents, commas and spaces. Anything else (`;`, `{`, `}`,
/// an unclosed string) could reach past the declaration.
pub fn is_contained_font_family(value: &str) -> bool {
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => loop {
                match chars.next() {
                    None | Some('\n' | '\r' | '\x0c') => return false,
                    Some('\\') => {
                        if chars.next().is_none() {
                            return false;
                        }
                    }
                    Some('"') => break,
                    Some(_) => {}
                }
            },
            ',' | ' ' | '-' => {}
            c if c.is_ascii_alphanumeric() => {}
            _ => return false,
        }
    }
    !value.contains('<')
}

/// Every non-overlapping match of `query` in `text`, scanning left to right, written
/// over `char`s and `String`s rather than `search`'s code-unit folding. `text` and
/// `query` are valid UTF-16. Ignore-case is the simple uppercase of each character when
/// it keeps the UTF-16 length; "whole word" means no alphanumeric character touches
/// either end.
pub fn reference_find_all(text: &[u16], query: &[u16], case_sensitive: bool, whole_word: bool) -> Vec<usize> {
    let mut found = Vec::new();
    let mut pos = 0;
    while pos + query.len() <= text.len() {
        if reference_is_match_at(text, pos, query.len(), query, case_sensitive, whole_word) {
            found.push(pos);
            pos += query.len();
        } else {
            pos += 1;
        }
    }
    found
}

/// Whether `text[start..start + len]` is a match by the rules of [`reference_find_all`]:
/// what the Replace button may replace. `query` is valid UTF-16; `start` and `len` are
/// anything (a selection can split a surrogate pair, or end past the text).
pub fn reference_is_match_at(
    text: &[u16],
    start: usize,
    len: usize,
    query: &[u16],
    case_sensitive: bool,
    whole_word: bool,
) -> bool {
    let upper = |units: &[u16]| -> Vec<u16> {
        let s = String::from_utf16(units).unwrap();
        s.chars()
            .flat_map(|c| {
                let u: Vec<char> = c.to_uppercase().collect();
                let c = if !case_sensitive && u.len() == 1 && u[0].len_utf16() == c.len_utf16() { u[0] } else { c };
                c.encode_utf16(&mut [0; 2]).to_vec()
            })
            .collect()
    };
    let word_at = |units: &[u16], last: bool| {
        let s = String::from_utf16_lossy(units);
        let c = if last { s.chars().next_back() } else { s.chars().next() };
        c.is_some_and(char::is_alphanumeric)
    };
    let end = start + len;
    !query.is_empty()
        && len == query.len()
        && end <= text.len()
        // Only whole characters: a slice that splits a surrogate pair is no match.
        && String::from_utf16(&text[start..end]).is_ok()
        && upper(&text[start..end]) == upper(query)
        && (!whole_word || (!word_at(&text[..start], true) && !word_at(&text[end..], false)))
}

/// The heading level the md_ops target passes to `set_heading`, from the byte whose bit 0
/// already picks how the text is read. Taken from a byte the operation choice does not
/// use: derived from the operation byte, only levels 0, 2, 3 and 5 could ever come up.
pub fn heading_level(byte: u8) -> usize {
    usize::from(byte >> 1) % 7
}

#[cfg(test)]
mod tests {
    use super::*;

    // The checkers must catch what they guard against, or a clean fuzz run means nothing.
    #[test]
    fn checkers_catch_violations() {
        assert_eq!(tag_names("<LINK rel=x><meta/><a>&lt;base</Iframe\r>"), ["link", "meta", "a", "iframe"]);
        assert!(!is_contained_font_family("\"a\"; } body { x"));
        assert!(!is_contained_font_family("\"a"));
        assert!(!is_contained_font_family("\"a\nb\""));
        assert!(is_contained_font_family("\"Segoe UI\", sans-serif, \"a\\\"{\""));
        assert_eq!(css_blocks("a { x: \"{\" } b{}"), 2);
        assert!(is_strict_hex_color("#1e1E1e") && is_strict_hex_color("#fff"));
        assert!(!is_strict_hex_color("#ffff") && !is_strict_hex_color("red") && !is_strict_hex_color("#fff;x:y"));
        assert!(!is_strict_hex_color("#ｆｆｆ"), "full-width digits are not hex");
        // The real pipeline: a fetching tag in raw HTML is neutralised.
        let html =
            render::to_html_document("<link rel=preconnect href=https://x>", &settings::AppSettings::default(), 0).html;
        assert!(!tag_names(&html).iter().any(|n| n == "link"));
        let w = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        assert_eq!(reference_find_all(&w("Cat concat CAT"), &w("cat"), false, true), [0, 11]);
        assert_eq!(reference_find_all(&w("Cat concat CAT"), &w("cat"), true, false), [7]);
        assert_eq!(reference_find_all(&w("x𐐨y 𐐨cat"), &w("𐐀"), false, false), [1, 5]);
        assert_eq!(reference_find_all(&w("𐐨cat"), &w("cat"), false, true), [] as [usize; 0]);
        assert_eq!(reference_find_all(&w("aaa"), &w("aa"), true, false), [0]);
        assert!(!reference_is_match_at(&w("category"), 0, 3, &w("cat"), false, true), "part of a word");
        assert!(reference_is_match_at(&w("category"), 0, 3, &w("CAT"), false, false));
        assert!(!reference_is_match_at(&w("cat"), 0, 2, &w("cat"), false, false), "length differs");
        assert!(!reference_is_match_at(&w("cat"), 2, 3, &w("cat"), false, false), "past the end");
    }

    // Every level (and the 0 that removes a heading) must be reachable with either way of
    // reading the text, or part of set_heading is never fuzzed at all.
    #[test]
    fn every_heading_level_is_reachable() {
        for text_mode in 0..2u8 {
            let levels: std::collections::BTreeSet<usize> =
                (0..=255u8).filter(|b| b & 1 == text_mode).map(heading_level).collect();
            assert_eq!(levels, (0..=6).collect(), "text mode {text_mode}");
        }
    }
}
