#![no_main]
use libfuzzer_sys::fuzz_target;
use mymdedit_fuzz::{css_blocks, is_contained_font_family, render, settings::AppSettings};

// Values straight from a hostile settings.json, not even sanitized: the CSS they become
// never leaves its declaration, adds a rule, or ends the <style> element.
fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else { return };
    let (family, color) = s.split_once('\u{1}').unwrap_or((s, ""));
    let family_css = render::format_css_font_family(family);
    assert!(is_contained_font_family(&family_css), "{family:?} -> {family_css:?}");

    let settings = AppSettings {
        preview_font_family: family.to_string(),
        preview_background_color: Some(color.to_string()),
        ..AppSettings::default()
    };
    let html = render::to_html_document("x", &settings, 0).html;
    let style = &html[html.find("<style>").unwrap() + 1..html.find("</style>").unwrap()];
    assert!(!style.contains('<'), "markup inside <style>: {s:?}");
    assert_eq!(html.matches("</style").count(), 1, "<style> closed early: {s:?}");
    // A valid color adds exactly one rule; nothing else may add any.
    let base = render::to_html_document("x", &AppSettings::default(), 0).html;
    assert!(css_blocks(&html) <= css_blocks(&base) + 1, "extra CSS rule: {s:?}");
});
