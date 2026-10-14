//! The Markdown toolbar and list auto-continuation, as pure functions over the editor's
//! UTF-16 text.
//!
//! Every operation is expressed as a single [`Edit`] (one replaced range plus the
//! selection to leave afterwards), so each toolbar click is exactly one undo step.
//! Positions are UTF-16 code units, which is what the editor's selection uses, and the
//! line breaks written are lone `\r`, which is what the editor stores.

use crate::line_endings::is_break;

#[derive(Debug, PartialEq, Eq)]
pub struct Edit {
    /// Range of the current text to replace.
    pub start: usize,
    pub end: usize,
    pub text: Vec<u16>,
    /// Selection to set once the replacement is in place.
    pub sel_start: usize,
    pub sel_len: usize,
}

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn concat(parts: &[&[u16]]) -> Vec<u16> {
    parts.concat()
}

pub fn find_line_start(text: &[u16], index: usize) -> usize {
    if index == 0 {
        return 0;
    }
    text[..index].iter().rposition(|&c| is_break(c)).map_or(0, |i| i + 1)
}

pub fn find_line_end(text: &[u16], from: usize) -> usize {
    text[from..].iter().position(|&c| is_break(c)).map_or(text.len(), |i| from + i)
}

fn skip_line_break(text: &[u16], at: usize) -> usize {
    if at >= text.len() {
        return at;
    }
    if text[at] == b'\r' as u16 && text.get(at + 1) == Some(&(b'\n' as u16)) { at + 2 } else { at + 1 }
}

/// Bold / italic / strikethrough / inline code: wrap the selection (keeping the original
/// text selected), or insert a placeholder and select it so typing replaces it.
pub fn wrap_selection(text: &[u16], start: usize, len: usize, prefix: &str, suffix: &str, placeholder: &str) -> Edit {
    let (prefix, suffix) = (w(prefix), w(suffix));
    let inner = if len > 0 { text[start..start + len].to_vec() } else { w(placeholder) };
    Edit {
        start,
        end: start + len,
        text: concat(&[&prefix, &inner, &suffix]),
        sel_start: start + prefix.len(),
        sel_len: inner.len(),
    }
}

/// Link / image: `[text](url)` with the `url` part selected afterwards, so a URL already
/// on the clipboard can be pasted straight over it.
pub fn insert_link(text: &[u16], start: usize, len: usize, opening: &str, text_placeholder: &str, url_placeholder: &str) -> Edit {
    let (opening, close_label, close_url) = (w(opening), w("]("), w(")"));
    let label = if len > 0 { text[start..start + len].to_vec() } else { w(text_placeholder) };
    let url = w(url_placeholder);
    Edit {
        start,
        end: start + len,
        sel_start: start + opening.len() + label.len() + close_label.len(),
        sel_len: url.len(),
        text: concat(&[&opening, &label, &close_label, &url, &close_url]),
    }
}

/// Heading / bulleted list / numbered list / quote: prefix the start of each line the
/// selection touches (only the caret's line when `single_line_only`).
pub fn prefix_lines(text: &[u16], start: usize, len: usize, single_line_only: bool, prefix_for: impl Fn(usize) -> String) -> Edit {
    let selection_end = start + len;
    let mut block_start = find_line_start(text, start);

    // A selection whose first character is the break ending this line doesn't touch
    // this line at all — begin at the next one.
    if len > 0 {
        let first_line_end = find_line_end(text, block_start);
        if first_line_end <= start {
            block_start = skip_line_break(text, first_line_end);
        }
    }

    let block_end = if single_line_only || len == 0 {
        find_line_end(text, block_start)
    } else {
        let mut end = block_start;
        loop {
            let line_end = find_line_end(text, end);
            end = line_end;
            if line_end >= text.len() {
                break;
            }
            let next = skip_line_break(text, line_end);
            if next >= selection_end {
                break;
            }
            end = next;
        }
        end
    };

    let mut prefixed = Vec::new();
    let mut line_index = 0;
    let mut i = block_start;
    loop {
        let line_end = find_line_end(text, i).min(block_end);
        if line_index > 0 {
            prefixed.push(b'\r' as u16);
        }
        prefixed.extend(w(&prefix_for(line_index)));
        prefixed.extend_from_slice(&text[i..line_end]);
        if line_end >= block_end {
            break;
        }
        i = skip_line_break(text, line_end);
        line_index += 1;
    }

    let caret = if len == 0 { start + w(&prefix_for(0)).len() } else { block_start + prefixed.len() };
    Edit { start: block_start, end: block_end, text: prefixed, sel_start: caret, sel_len: 0 }
}

/// Replaces an existing heading marker (`#`..`######` + space) on the caret's line with
/// one of `level`, rather than stacking markers.
pub fn set_heading(text: &[u16], start: usize, level: usize) -> Edit {
    let line_start = find_line_start(text, start);
    let line_end = find_line_end(text, line_start);
    let line = &text[line_start..line_end];

    let hashes = line.iter().take(6).take_while(|&&c| c == b'#' as u16).count();
    let existing = if hashes > 0 && line.get(hashes) == Some(&(b' ' as u16)) { hashes + 1 } else { 0 };
    let marker = w(&format!("{} ", "#".repeat(level)));
    let caret = line_start + marker.len();
    Edit { start: line_start, end: line_start + existing, text: marker, sel_start: caret, sel_len: 0 }
}

/// The line breaks to put before/after an inserted block so it stands on its own line
/// (and, with `blank_line`, has an empty line on each side — Markdown needs that for a
/// table or rule to render as one).
fn boundary(text: &[u16], index: usize, before: bool, blank_line: bool) -> Vec<u16> {
    if (before && index == 0) || (!before && index == text.len()) {
        return Vec::new();
    }
    let adjacent_is_break = if before { is_break(text[index - 1]) } else { is_break(text[index]) };
    let s = match (blank_line, adjacent_is_break) {
        (false, true) => "",
        (false, false) => "\r",
        (true, true) => "\r",
        (true, false) => "\r\r",
    };
    w(s)
}

/// Horizontal rule / table: insert `block` as its own paragraph, replacing any selection,
/// and select it.
pub fn insert_block(text: &[u16], start: usize, len: usize, block: &str, blank_line: bool) -> Edit {
    let end = start + len;
    let leading = boundary(text, start, true, blank_line);
    let trailing = boundary(text, end, false, blank_line);
    let block = w(block);
    Edit {
        start,
        end,
        sel_start: start + leading.len(),
        sel_len: block.len(),
        text: concat(&[&leading, &block, &trailing]),
    }
}

/// Fenced code block around the selection (or a placeholder), with the inside selected.
pub fn code_block(text: &[u16], start: usize, len: usize) -> Edit {
    let end = start + len;
    let inner = if len > 0 { text[start..end].to_vec() } else { w("コード") };
    let leading = boundary(text, start, true, false);
    let trailing = boundary(text, end, false, false);
    let (open, close) = (w("```\r"), w("\r```"));
    Edit {
        start,
        end,
        sel_start: start + leading.len() + open.len(),
        sel_len: inner.len(),
        text: concat(&[&leading, &open, &inner, &close, &trailing]),
    }
}

/// Enter at the end of a `- ` / `N. ` list line continues the list (numbers count up);
/// Enter on a line holding only the marker leaves the list. `None` means "not a list
/// situation — let Enter do its normal thing".
pub fn on_enter(text: &[u16], caret: usize, sel_len: usize) -> Option<Edit> {
    if sel_len > 0 {
        return None;
    }
    let line_start = find_line_start(text, caret);
    let line_end = find_line_end(text, line_start);
    let line = &text[line_start..line_end];

    let (marker_len, content_len, next_marker) = if line.starts_with(&w("- ")) {
        (2, line.len() - 2, "- ".to_string())
    } else {
        let (number, marker_len) = parse_numbered(line)?;
        (marker_len, line.len() - marker_len, format!("{}. ", number + 1))
    };

    // Caret before the marker: an ordinary Enter.
    if caret < line_start + marker_len {
        return None;
    }

    if content_len == 0 {
        // Leave the list: the marker-only line becomes an empty line.
        return Some(Edit { start: line_start, end: line_end, text: w("\r"), sel_start: line_start + 1, sel_len: 0 });
    }

    let insert = concat(&[&w("\r"), &w(&next_marker)]);
    let caret_after = caret + insert.len();
    Some(Edit { start: caret, end: caret, text: insert, sel_start: caret_after, sel_len: 0 })
}

/// `N. ` at the start of `line` → (N, marker length).
fn parse_numbered(line: &[u16]) -> Option<(i32, usize)> {
    let digits = line.iter().take_while(|&&c| (b'0' as u16..=b'9' as u16).contains(&c)).count();
    if digits == 0 || line.get(digits) != Some(&(b'.' as u16)) || line.get(digits + 1) != Some(&(b' ' as u16)) {
        return None;
    }
    let number: i32 = String::from_utf16_lossy(&line[..digits]).parse().ok()?;
    (number != i32::MAX).then_some((number, digits + 2))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(text: &str, e: &Edit) -> (String, String) {
        let t = w(text);
        let out = concat(&[&t[..e.start], &e.text, &t[e.end..]]);
        let sel = String::from_utf16_lossy(&out[e.sel_start..e.sel_start + e.sel_len]);
        (String::from_utf16_lossy(&out), sel)
    }

    #[test]
    fn wraps_selection_or_inserts_placeholder() {
        let e = wrap_selection(&w("abc"), 1, 1, "**", "**", "太字");
        assert_eq!(apply("abc", &e), ("a**b**c".into(), "b".into()));
        let e = wrap_selection(&w("abc"), 3, 0, "**", "**", "太字");
        assert_eq!(apply("abc", &e), ("abc**太字**".into(), "太字".into()));
    }

    #[test]
    fn link_selects_url() {
        let e = insert_link(&w("x"), 0, 1, "[", "リンクテキスト", "url");
        assert_eq!(apply("x", &e), ("[x](url)".into(), "url".into()));
    }

    #[test]
    fn image_link_selects_url() {
        let e = insert_link(&w("x"), 0, 1, "![", "代替テキスト", "url");
        assert_eq!(apply("x", &e), ("![x](url)".into(), "url".into()));
    }

    #[test]
    fn numbered_list_over_selection() {
        let t = "a\rb\rc";
        let e = prefix_lines(&w(t), 0, 3, false, |i| format!("{}. ", i + 1));
        assert_eq!(apply(t, &e).0, "1. a\r2. b\rc");
        assert_eq!(e.sel_start, 9);
    }

    #[test]
    fn bullet_on_caret_line() {
        let t = "a\rbc";
        let e = prefix_lines(&w(t), 3, 0, false, |_| "- ".into());
        assert_eq!(apply(t, &e).0, "a\r- bc");
        assert_eq!(e.sel_start, 5);
    }

    #[test]
    fn heading_replaces_existing_marker() {
        let t = "x\r## 見出し";
        let e = set_heading(&w(t), 4, 1);
        assert_eq!(apply(t, &e).0, "x\r# 見出し");
        let e = set_heading(&w("abc"), 1, 3);
        assert_eq!(apply("abc", &e).0, "### abc");
    }

    #[test]
    fn block_gets_blank_lines() {
        let e = insert_block(&w("ab"), 1, 0, "---", true);
        assert_eq!(apply("ab", &e), ("a\r\r---\r\rb".into(), "---".into()));
        let e = insert_block(&w(""), 0, 0, "---", true);
        assert_eq!(apply("", &e).0, "---");
    }

    #[test]
    fn code_block_wraps_selection() {
        let e = code_block(&w("xy"), 1, 1);
        assert_eq!(apply("xy", &e), ("x\r```\ry\r```".into(), "y".into()));
    }

    #[test]
    fn enter_continues_and_exits_lists() {
        let e = on_enter(&w("- a"), 3, 0).unwrap();
        assert_eq!(apply("- a", &e).0, "- a\r- ");
        assert_eq!(e.sel_start, 6);
        let e = on_enter(&w("9. a"), 4, 0).unwrap();
        assert_eq!(apply("9. a", &e).0, "9. a\r10. ");
        let e = on_enter(&w("x\r- "), 4, 0).unwrap();
        assert_eq!(apply("x\r- ", &e).0, "x\r\r");
        assert_eq!(on_enter(&w("- a"), 1, 0), None);
        assert_eq!(on_enter(&w("abc"), 3, 0), None);
        assert_eq!(on_enter(&w("- a"), 3, 1), None);
    }
}
