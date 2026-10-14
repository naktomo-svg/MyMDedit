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
        // The real pipeline: a fetching tag in raw HTML is neutralised.
        let html = render::to_html_document("<link rel=preconnect href=https://x>", &settings::AppSettings::default());
        assert!(!tag_names(&html).iter().any(|n| n == "link"));
    }
}
