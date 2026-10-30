#![no_main]
use libfuzzer_sys::fuzz_target;
use mymdedit_fuzz::settings::{AppSettings, Parsed, parse_settings};
use mymdedit_fuzz::{css_blocks, is_contained_font_family, is_strict_hex_color, render};

/// What any settings may do to the page's CSS: stay inside `<style>`, never end it, and
/// add exactly the one rule an accepted background color is, verbatim, and nothing else.
fn check(settings: &AppSettings, input: &str) {
    let html = render::to_html_document("x", settings, 0).html;
    // The style element's content: from the end of its start tag (attributes or not) to
    // its first end tag.
    let open = html.find("<style").unwrap();
    let content_start = open + html[open..].find('>').unwrap() + 1;
    let style = &html[content_start..html.find("</style>").unwrap()];
    assert!(!style.contains('<'), "markup inside <style>: {input:?}");
    assert_eq!(html.matches("</style").count(), 1, "<style> closed early: {input:?}");

    let color = settings.preview_background_color.as_deref();
    let accepted = color.is_some_and(is_strict_hex_color);
    let without = AppSettings { preview_background_color: None, ..settings.clone() };
    let base = css_blocks(&render::to_html_document("x", &without, 0).html);
    assert_eq!(css_blocks(&html), base + usize::from(accepted), "CSS rule count: {input:?}");
    if let Some(c) = color.filter(|_| accepted) {
        assert!(html.contains(&format!("background: {c} !important;")), "accepted color not as written: {input:?}");
    }
}

// Hostile settings.json values, two ways: the preview's font and background exactly as
// typed, not even sanitized; and a whole file through the app's own reading path
// (`parse_settings`), so every field — including ones added later — is covered. Either
// way the CSS they become never leaves its declaration, adds a rule, or ends <style>.
fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else { return };
    let (json, raw) = s.split_once('\u{2}').unwrap_or(("", s));
    let (family, color) = raw.split_once('\u{1}').unwrap_or((raw, ""));

    let family_css = render::format_css_font_family(family);
    assert!(is_contained_font_family(&family_css), "{family:?} -> {family_css:?}");
    let raw_settings = AppSettings {
        preview_font_family: family.to_string(),
        preview_background_color: Some(color.to_string()),
        ..AppSettings::default()
    };
    check(&raw_settings, raw);

    let loaded = match parse_settings(json) {
        Parsed::Clean(settings) | Parsed::Partial(settings, _) => settings,
        Parsed::Unreadable(_) => return,
    };
    check(&loaded, json);
});
