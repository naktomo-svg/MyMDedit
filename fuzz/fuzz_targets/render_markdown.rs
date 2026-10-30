#![no_main]
use std::sync::LazyLock;

use libfuzzer_sys::fuzz_target;
use mymdedit_fuzz::{FETCHING, render, settings::AppSettings, tag_names};

/// The <meta> elements of an empty document: the page's own charset and CSP.
static OWN_METAS: LazyLock<usize> = LazyLock::new(|| {
    tag_names(&render::to_html_document("", &AppSettings::default(), 0).html).iter().filter(|n| *n == "meta").count()
});

// Any Markdown: no panic, and no element that fetches or connects survives — the only
// <meta> elements left are the page's own charset and CSP.
fuzz_target!(|data: &[u8]| {
    let Ok(md) = std::str::from_utf8(data) else { return };
    let html = render::to_html_document(md, &AppSettings::default(), 0).html;
    let names = tag_names(&html);
    assert_eq!(names.iter().filter(|n| *n == "meta").count(), *OWN_METAS, "<meta> survived: {md:?}");
    for n in names.iter().filter(|n| *n != "meta") {
        assert!(!FETCHING.contains(&n.as_str()), "<{n}> survived: {md:?}");
    }
});
