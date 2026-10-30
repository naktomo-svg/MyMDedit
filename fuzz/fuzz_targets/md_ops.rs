#![no_main]
use libfuzzer_sys::fuzz_target;
use mymdedit_fuzz::md_ops::{self, Edit};

fn check(text: &[u16], e: &Edit) {
    assert!(e.start <= e.end && e.end <= text.len(), "range {}..{} of {}", e.start, e.end, text.len());
    let new_len = text.len() - (e.end - e.start) + e.text.len();
    assert!(e.sel_start + e.sel_len <= new_len, "selection past the end");
}

// Every toolbar action, list continuation and indentation, at any in-range selection: no
// panic, and the edit and the selection after it stay inside the text. Heading and line
// lookups only ever point at line starts inside the text.
fuzz_target!(|data: &[u8]| {
    let Some((head, rest)) = data.split_first_chunk::<6>() else { return };
    // The editor's buffer is any u16 sequence (a paste can carry a lone surrogate), so
    // half the inputs are raw code units; the other half go through UTF-8, which reaches
    // Markdown syntax far sooner.
    let text: Vec<u16> = if head[5] & 1 == 1 {
        rest.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect()
    } else {
        String::from_utf8_lossy(rest).encode_utf16().collect()
    };
    let n = text.len();
    let start = usize::from(u16::from_le_bytes([head[0], head[1]])) % (n + 1);
    let len = usize::from(u16::from_le_bytes([head[2], head[3]])) % (n - start + 1);
    let op = head[4];
    let is_line_start = |i: usize| i <= n && md_ops::find_line_start(&text, i) == i;
    let e = match op % 14 {
        8 | 9 => match md_ops::indent_lines(&text, start, len, op % 14 == 9) {
            Some(e) => e,
            None => return,
        },
        10 => md_ops::toggle_task(&text, start, len),
        13 => md_ops::inline_code(&text, start, len),
        11 => {
            for h in md_ops::headings(&text) {
                assert!(is_line_start(h.line_start) && (1..=6).contains(&h.level), "heading at {}", h.line_start);
            }
            return;
        }
        12 => {
            // The editor's text never holds \n (RichEdit stores lone \r); inside a \r\n pair
            // find_line_start sees the \n as a line of its own, which no count agrees with.
            // So check the text the editor would hold, rather than skip such inputs.
            let text: Vec<u16> =
                text.iter().map(|&u| if u == u16::from(b'\n') { u16::from(b'\r') } else { u }).collect();
            let line = md_ops::line_index_at(&text, start);
            let line_start = md_ops::line_start_of(&text, line);
            assert!(md_ops::find_line_start(&text, line_start) == line_start, "line {line} starts at {line_start}");
            assert_eq!(line_start, md_ops::find_line_start(&text, start), "line {line} of position {start}");
            return;
        }
        0 => md_ops::wrap_selection(&text, start, len, "**", "**", "太字"),
        1 => md_ops::insert_link(&text, start, len, "[", "テキスト", "https://"),
        2 => md_ops::prefix_lines(&text, start, len, false, |i| format!("{}. ", i + 1)),
        3 => md_ops::set_heading(&text, start, usize::from(op / 8 % 7)),
        4 => md_ops::insert_block(&text, start, len, "---", true),
        5 => md_ops::code_block(&text, start, len),
        6 => match md_ops::on_enter(&text, start, len) {
            Some(e) => e,
            None => return,
        },
        _ => md_ops::prefix_lines(&text, start, len, true, |_| "> ".into()),
    };
    check(&text, &e);
});
