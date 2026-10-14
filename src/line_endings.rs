//! The single definition of what counts as a line break.
//!
//! The editor (RichEdit in plain-text mode) stores every line break as a lone `\r`,
//! while files on disk use CRLF, LF or CR. Everything that splits, counts or writes
//! lines goes through here so the three never disagree.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LineEnding {
    #[default]
    Crlf,
    Lf,
    Cr,
}

impl LineEnding {
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Crlf => "\r\n",
            LineEnding::Lf => "\n",
            LineEnding::Cr => "\r",
        }
    }
}

/// The style of the first line break in `text`, or CRLF when there is none.
///
/// The first break decides rather than a majority vote: the two only disagree for a
/// file that already mixes styles, and such a file has no single original form to keep.
pub fn detect(text: &str) -> LineEnding {
    let bytes = text.as_bytes();
    match bytes.iter().position(|&b| b == b'\r' || b == b'\n') {
        Some(i) if bytes[i] == b'\n' => LineEnding::Lf,
        Some(i) if bytes.get(i + 1) == Some(&b'\n') => LineEnding::Crlf,
        Some(_) => LineEnding::Cr,
        None => LineEnding::Crlf,
    }
}

/// Rewrites every line break (`\r\n`, `\r` or `\n`) in `text` as `ending`.
pub fn normalize(text: &str, ending: LineEnding) -> String {
    let target = ending.as_str();
    let mut out = String::with_capacity(text.len() + text.len() / 32);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            // \r\n must be consumed as one break before \r alone is considered,
            // or every CRLF turns into two breaks and blank lines appear.
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str(target);
            }
            '\n' => out.push_str(target),
            _ => out.push(c),
        }
    }
    out
}

pub fn is_break(unit: u16) -> bool {
    unit == b'\r' as u16 || unit == b'\n' as u16
}

/// Number of lines in UTF-16 `text`: breaks + 1, with `\r\n` counting as one break.
/// An empty document is one line.
pub fn count_lines(text: &[u16]) -> usize {
    let mut lines = 1;
    let mut i = 0;
    while i < text.len() {
        if text[i] == b'\r' as u16 {
            lines += 1;
            if text.get(i + 1) == Some(&(b'\n' as u16)) {
                i += 1;
            }
        } else if text[i] == b'\n' as u16 {
            lines += 1;
        }
        i += 1;
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn detects_first_break() {
        assert_eq!(detect("a\nb\r\nc"), LineEnding::Lf);
        assert_eq!(detect("a\r\nb\nc"), LineEnding::Crlf);
        assert_eq!(detect("a\rb"), LineEnding::Cr);
        assert_eq!(detect("abc"), LineEnding::Crlf);
        assert_eq!(detect("a\r"), LineEnding::Cr);
    }

    #[test]
    fn normalizes_all_styles() {
        assert_eq!(normalize("a\rb\r\nc\nd", LineEnding::Lf), "a\nb\nc\nd");
        assert_eq!(normalize("a\rb\r\nc\nd", LineEnding::Crlf), "a\r\nb\r\nc\r\nd");
        assert_eq!(normalize("a\r\n\r\nb", LineEnding::Cr), "a\r\rb");
    }

    #[test]
    fn counts_lines() {
        assert_eq!(count_lines(&w("")), 1);
        assert_eq!(count_lines(&w("a")), 1);
        assert_eq!(count_lines(&w("a\rb\r")), 3);
        assert_eq!(count_lines(&w("a\r\nb")), 2);
        assert_eq!(count_lines(&w("a\n\nb")), 3);
    }
}
