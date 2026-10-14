#![no_main]
use libfuzzer_sys::fuzz_target;
use mymdedit_fuzz::md_ops::{self, Edit};

fn check(text: &[u16], e: &Edit) {
    assert!(e.start <= e.end && e.end <= text.len(), "range {}..{} of {}", e.start, e.end, text.len());
    let new_len = text.len() - (e.end - e.start) + e.text.len();
    assert!(e.sel_start + e.sel_len <= new_len, "selection past the end");
}

// Every toolbar action and list continuation, at any in-range selection: no panic, and
// the edit and the selection after it stay inside the text.
fuzz_target!(|data: &[u8]| {
    let Some((head, rest)) = data.split_first_chunk::<5>() else { return };
    let text: Vec<u16> = String::from_utf8_lossy(rest).encode_utf16().collect();
    let n = text.len();
    let start = usize::from(u16::from_le_bytes([head[0], head[1]])) % (n + 1);
    let len = usize::from(u16::from_le_bytes([head[2], head[3]])) % (n - start + 1);
    let op = head[4];
    let e = match op % 8 {
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
