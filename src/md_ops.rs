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

/// Zero-based number of the line `index` is on, counted the way the footer counts lines
/// ([`crate::line_endings::count_lines`]).
pub fn line_index_at(text: &[u16], index: usize) -> usize {
    crate::line_endings::count_lines(&text[..find_line_start(text, index)]) - 1
}

/// Where zero-based line `n` starts, or the last line's start when there are fewer lines.
pub fn line_start_of(text: &[u16], n: usize) -> usize {
    let mut start = 0;
    for _ in 0..n {
        let end = find_line_end(text, start);
        if end >= text.len() {
            break;
        }
        start = skip_line_break(text, end);
    }
    start
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
    let (block_start, block_end) = line_block(text, start, len, single_line_only);

    let mut prefixed = Vec::new();
    for (line_index, (i, line_end)) in lines_in(text, block_start, block_end).enumerate() {
        if line_index > 0 {
            prefixed.push(b'\r' as u16);
        }
        prefixed.extend(w(&prefix_for(line_index)));
        prefixed.extend_from_slice(&text[i..line_end]);
    }

    let caret = if len == 0 { start + w(&prefix_for(0)).len() } else { block_start + prefixed.len() };
    Edit { start: block_start, end: block_end, text: prefixed, sel_start: caret, sel_len: 0 }
}

/// The whole lines the selection touches, as (start of the first, end of the last —
/// before its line break). Only the caret's line when `single_line_only` or `len == 0`.
fn line_block(text: &[u16], start: usize, len: usize, single_line_only: bool) -> (usize, usize) {
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
    (block_start, block_end)
}

/// (start, end) of each line in a block from [`line_block`]; ends exclude the break.
fn lines_in(text: &[u16], block_start: usize, block_end: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut next = Some(block_start);
    std::iter::from_fn(move || {
        let i = next?;
        let line_end = find_line_end(text, i).min(block_end);
        next = (line_end < block_end).then(|| skip_line_break(text, line_end));
        Some((i, line_end))
    })
}

/// Spaces one Tab adds to a list item. Four nests under both `- ` and `N. ` (whose
/// content starts at column 2 and 3) without reaching code-block indentation.
const INDENT: usize = 4;

/// Tab / Shift+Tab on list items: indents every line the selection touches by
/// [`INDENT`] spaces, or removes up to that many. `None` when any of those lines is not a
/// list item, so the key keeps its ordinary meaning there. A caret moves with its text;
/// a selection becomes the whole lines afterwards.
pub fn indent_lines(text: &[u16], start: usize, len: usize, outdent: bool) -> Option<Edit> {
    let (block_start, block_end) = line_block(text, start, len, false);
    let mut out = Vec::new();
    let mut caret = start;
    for (line_index, (i, line_end)) in lines_in(text, block_start, block_end).enumerate() {
        let marker = list_marker(&text[i..line_end])?;
        if line_index > 0 {
            out.push(b'\r' as u16);
        }
        let new_line_start = block_start + out.len();
        if outdent {
            let removed = marker.indent.min(INDENT);
            out.extend_from_slice(&text[i + removed..line_end]);
            if len == 0 {
                caret = new_line_start + (start - i).saturating_sub(removed);
            }
        } else {
            out.extend(w(&" ".repeat(INDENT)));
            out.extend_from_slice(&text[i..line_end]);
            if len == 0 {
                caret = start + INDENT;
            }
        }
    }
    let (sel_start, sel_len) = if len == 0 { (caret, 0) } else { (block_start, out.len()) };
    Some(Edit { start: block_start, end: block_end, text: out, sel_start, sel_len })
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

/// A list item's prefix: indentation, then `- [ ] ` / `- [x] `, `- ` or `N. `.
struct ListMarker {
    /// Leading spaces.
    indent: usize,
    /// Indentation plus marker, up to where the item's content begins.
    len: usize,
    /// The marker the next item of the same list gets (without the indentation).
    next: String,
}

fn list_marker(line: &[u16]) -> Option<ListMarker> {
    let indent = line.iter().take_while(|&&c| c == b' ' as u16).count();
    let rest = &line[indent..];
    let (marker_len, next) = if task_state(rest).is_some() {
        (TASK_MARKER_LEN, "- [ ] ".to_string())
    } else if rest.starts_with(&w("- ")) {
        (2, "- ".to_string())
    } else {
        let (number, marker_len) = parse_numbered(rest)?;
        (marker_len, format!("{}. ", number + 1))
    };
    Some(ListMarker { indent, len: indent + marker_len, next })
}

/// Length of `- [ ] `.
const TASK_MARKER_LEN: usize = 6;

/// `Some(checked)` when `rest` starts with a task marker (`- [ ] `, `- [x] `, `- [X] `).
fn task_state(rest: &[u16]) -> Option<bool> {
    if rest.len() < TASK_MARKER_LEN || !rest.starts_with(&w("- [")) || rest[4..6] != w("] ")[..] {
        return None;
    }
    match char::from_u32(rest[3] as u32) {
        Some(' ') => Some(false),
        Some('x' | 'X') => Some(true),
        _ => None,
    }
}

/// Task list: flips `- [ ] ` ↔ `- [x] ` on every line the selection touches; any other
/// list marker becomes `- [ ] `, and a plain line gets one after its indentation. Blank
/// lines inside a multi-line selection are left alone. A caret moves with its text; a
/// selection becomes the whole lines afterwards.
pub fn toggle_task(text: &[u16], start: usize, len: usize) -> Edit {
    let (block_start, block_end) = line_block(text, start, len, false);
    let mut out = Vec::new();
    let mut caret = start;
    for (line_index, (i, line_end)) in lines_in(text, block_start, block_end).enumerate() {
        if line_index > 0 {
            out.push(b'\r' as u16);
        }
        let line = &text[i..line_end];
        let indent = line.iter().take_while(|&&c| c == b' ' as u16).count();
        let rest = &line[indent..];
        if len > 0 && rest.is_empty() {
            out.extend_from_slice(line);
            continue;
        }
        let (old_marker, new_marker) = match task_state(rest) {
            Some(checked) => (TASK_MARKER_LEN, if checked { "- [ ] " } else { "- [x] " }),
            None => (list_marker(line).map_or(0, |m| m.len - indent), "- [ ] "),
        };
        out.extend_from_slice(&line[..indent]);
        out.extend(w(new_marker));
        out.extend_from_slice(&rest[old_marker..]);
        if len == 0 {
            let content = indent + old_marker;
            let offset = start - i;
            caret = i + if offset < content { indent + TASK_MARKER_LEN } else { offset - old_marker + TASK_MARKER_LEN };
        }
    }
    let (sel_start, sel_len) = if len == 0 { (caret, 0) } else { (block_start, out.len()) };
    Edit { start: block_start, end: block_end, text: out, sel_start, sel_len }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Heading {
    pub level: usize,
    /// Where the heading's line starts, for moving the caret there.
    pub line_start: usize,
    pub title: String,
}

/// The ATX headings (`#`..`######`) in `text`, in order, leaving out lines inside fenced
/// code blocks (``` or ~~~), where a `#` is code rather than a heading.
pub fn headings(text: &[u16]) -> Vec<Heading> {
    let mut out = Vec::new();
    // The open fence's character and length; it closes on a run at least as long.
    let mut fence: Option<(u16, usize)> = None;
    let mut i = 0;
    while i <= text.len() {
        let line_end = find_line_end(text, i);
        let line = &text[i..line_end];
        let indent = line.iter().take(4).take_while(|&&c| c == b' ' as u16).count();
        let rest = if indent < 4 { &line[indent..] } else { &[][..] };
        let run = |c: u16| rest.iter().take_while(|&&x| x == c).count();

        if let Some((c, n)) = fence {
            if run(c) >= n {
                fence = None;
            }
        } else if let Some(c) = [b'`' as u16, b'~' as u16].into_iter().find(|&c| run(c) >= 3) {
            fence = Some((c, run(c)));
        } else {
            let level = run(b'#' as u16);
            if (1..=6).contains(&level) && rest.get(level).is_none_or(|&c| c == b' ' as u16) {
                let title = String::from_utf16_lossy(&rest[level..]);
                let title = title.trim();
                // A closing run of #s is decoration, but only after a space: `# C#` is "C#".
                let stripped = title.trim_end_matches('#');
                let title = if stripped.is_empty() || stripped.ends_with(' ') { stripped.trim_end() } else { title };
                out.push(Heading { level, line_start: i, title: title.to_string() });
            }
        }
        if line_end >= text.len() {
            break;
        }
        i = skip_line_break(text, line_end);
    }
    out
}

/// Enter at the end of a list line (`- `, `N. `, `- [ ] `, at any indentation) continues
/// the list at the same indentation (numbers count up, tasks start unchecked); Enter on a
/// line holding only the marker leaves the list. `None` means "not a list situation —
/// let Enter do its normal thing".
pub fn on_enter(text: &[u16], caret: usize, sel_len: usize) -> Option<Edit> {
    if sel_len > 0 {
        return None;
    }
    let line_start = find_line_start(text, caret);
    let line_end = find_line_end(text, line_start);
    let marker = list_marker(&text[line_start..line_end])?;

    // Caret inside the indentation or the marker: an ordinary Enter.
    if caret < line_start + marker.len {
        return None;
    }

    if line_start + marker.len == line_end {
        // Leave the list: the marker-only line becomes an empty line.
        return Some(Edit { start: line_start, end: line_end, text: w("\r"), sel_start: line_start + 1, sel_len: 0 });
    }

    let insert = concat(&[&w("\r"), &w(&" ".repeat(marker.indent)), &w(&marker.next)]);
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

    #[test]
    fn enter_keeps_indentation() {
        let t = "    - a";
        let e = on_enter(&w(t), 7, 0).unwrap();
        assert_eq!(apply(t, &e).0, "    - a\r    - ");
        assert_eq!(e.sel_start, 14);
        let t = "    2. a";
        let e = on_enter(&w(t), 8, 0).unwrap();
        assert_eq!(apply(t, &e).0, "    2. a\r    3. ");
        // Caret inside the indentation or the marker: an ordinary Enter.
        assert_eq!(on_enter(&w("    - a"), 5, 0), None);
        let t = "    - ";
        let e = on_enter(&w(t), 6, 0).unwrap();
        assert_eq!(apply(t, &e).0, "\r");
    }

    #[test]
    fn tab_indents_list_line_and_moves_caret() {
        let t = "x\r- a";
        let e = indent_lines(&w(t), 5, 0, false).unwrap();
        assert_eq!(apply(t, &e).0, "x\r    - a");
        assert_eq!((e.sel_start, e.sel_len), (9, 0));
    }

    #[test]
    fn tab_ignores_non_list_lines() {
        assert_eq!(indent_lines(&w("abc"), 1, 0, false), None);
        // Every touched line must be a list item.
        assert_eq!(indent_lines(&w("- a\rb"), 0, 5, false), None);
    }

    #[test]
    fn tab_indents_every_selected_line_and_selects_them() {
        let t = "- a\r1. b\r- [ ] c\rz";
        let e = indent_lines(&w(t), 1, 12, false).unwrap();
        assert_eq!(apply(t, &e), ("    - a\r    1. b\r    - [ ] c\rz".into(), "    - a\r    1. b\r    - [ ] c".into()));
    }

    #[test]
    fn shift_tab_removes_up_to_four_spaces() {
        let t = "      - a";
        let e = indent_lines(&w(t), 9, 0, true).unwrap();
        assert_eq!(apply(t, &e).0, "  - a");
        assert_eq!(e.sel_start, 5);
        let t = "  - a\r- b";
        let e = indent_lines(&w(t), 0, 9, true).unwrap();
        assert_eq!(apply(t, &e).0, "- a\r- b");
    }

    #[test]
    fn shift_tab_on_unindented_list_line_changes_nothing() {
        let t = "- a";
        let e = indent_lines(&w(t), 2, 0, true).unwrap();
        assert_eq!(apply(t, &e).0, "- a");
        assert_eq!(e.sel_start, 2);
    }

    #[test]
    fn shift_tab_caret_inside_removed_indent_goes_to_line_start() {
        let t = "x\r    - a";
        let e = indent_lines(&w(t), 4, 0, true).unwrap();
        assert_eq!(apply(t, &e).0, "x\r- a");
        assert_eq!(e.sel_start, 2);
    }

    #[test]
    fn task_toggle_checks_and_unchecks() {
        let t = "- [ ] a";
        let e = toggle_task(&w(t), 7, 0);
        assert_eq!(apply(t, &e).0, "- [x] a");
        assert_eq!(e.sel_start, 7);
        let t = "  - [X] a";
        assert_eq!(apply(t, &toggle_task(&w(t), 0, 0)).0, "  - [ ] a");
    }

    #[test]
    fn task_toggle_turns_other_lines_into_tasks() {
        let t = "- a";
        let e = toggle_task(&w(t), 3, 0);
        assert_eq!(apply(t, &e).0, "- [ ] a");
        assert_eq!(e.sel_start, 7);
        let t = "  3. a";
        assert_eq!(apply(t, &toggle_task(&w(t), 6, 0)).0, "  - [ ] a");
        let t = "abc";
        let e = toggle_task(&w(t), 1, 0);
        assert_eq!(apply(t, &e).0, "- [ ] abc");
        assert_eq!(e.sel_start, 7);
        assert_eq!(apply("", &toggle_task(&w(""), 0, 0)).0, "- [ ] ");
    }

    #[test]
    fn task_toggle_over_selection_skips_blank_lines() {
        let t = "- [ ] a\r\r- [x] b\rc";
        let e = toggle_task(&w(t), 0, t.encode_utf16().count());
        assert_eq!(apply(t, &e), ("- [x] a\r\r- [ ] b\r- [ ] c".into(), "- [x] a\r\r- [ ] b\r- [ ] c".into()));
    }

    fn heading_list(t: &str) -> Vec<(usize, usize, String)> {
        headings(&w(t)).into_iter().map(|h| (h.level, h.line_start, h.title)).collect()
    }

    #[test]
    fn headings_lists_atx_headings_with_positions() {
        let t = "# 一\rtext\r  ### 三 ##\r####### seven\r#nospace\r##";
        assert_eq!(heading_list(t), vec![(1, 0, "一".into()), (3, 9, "三".into()), (2, 43, "".into())]);
    }

    #[test]
    fn heading_keeps_hash_that_is_part_of_the_title() {
        assert_eq!(heading_list("# C#\r## #"), vec![(1, 0, "C#".into()), (2, 5, "".into())]);
    }

    #[test]
    fn headings_skips_fenced_code() {
        let t = "```\r# not\r```\r~~~~\r# not\r~~~\r# not\r~~~~\r# yes";
        assert_eq!(heading_list(t), vec![(1, 40, "yes".into())]);
    }

    #[test]
    fn line_index_and_line_start_agree() {
        let t = w("ab\rc\r\rd");
        assert_eq!([0, 2, 3, 4, 5, 6, 7].map(|p| line_index_at(&t, p)), [0, 0, 1, 1, 2, 3, 3]);
        assert_eq!([0, 1, 2, 3].map(|n| line_start_of(&t, n)), [0, 3, 5, 6]);
        // Past the last line: the last line's start.
        assert_eq!(line_start_of(&t, 9), 6);
        assert_eq!(line_start_of(&w(""), 0), 0);
    }

    #[test]
    fn enter_continues_task_list_unchecked() {
        let t = "- [x] a";
        let e = on_enter(&w(t), 7, 0).unwrap();
        assert_eq!(apply(t, &e).0, "- [x] a\r- [ ] ");
        assert_eq!(e.sel_start, 14);
        let t = "  - [ ] a";
        let e = on_enter(&w(t), 9, 0).unwrap();
        assert_eq!(apply(t, &e).0, "  - [ ] a\r  - [ ] ");
        let t = "- [ ] ";
        let e = on_enter(&w(t), 6, 0).unwrap();
        assert_eq!(apply(t, &e).0, "\r");
    }
}
