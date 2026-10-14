//! Markdown → a complete HTML document for the preview.
//!
//! Raw HTML in the Markdown is passed through (not sanitized). Execution and outbound
//! traffic are shut off in three layers instead, because opening a third party's `.md`
//! is a real path: the CSP below, `IsScriptEnabled = false`, and the navigation guard
//! in `preview.rs`.

use comrak::{Options, markdown_to_html};

use crate::settings::{AppSettings, Theme, is_hex_color};

fn options() -> Options<'static> {
    let mut o = Options::default();
    o.extension.strikethrough = true;
    o.extension.table = true;
    o.extension.autolink = true;
    o.extension.tasklist = true;
    o.extension.superscript = true;
    o.extension.subscript = true;
    o.extension.footnotes = true;
    o.extension.description_lists = true;
    o.extension.header_id_prefix = Some(String::new());
    // Required for Japanese text, not a nicety. CommonMark decides whether a ** run
    // opens or closes emphasis from its neighbours, and Japanese has no inter-word
    // spaces while 、。「」（）： are all punctuation — so **注意：**内容 and
    // これは**「太字」**です render as literal asterisks without this. The tradeoff,
    // accepted deliberately: the preview shows bold for some text GitHub would not.
    o.extension.cjk_friendly_emphasis = true;
    o.render.r#unsafe = true;
    o
}

pub fn to_html_document(markdown: &str, settings: &AppSettings) -> String {
    let body = markdown_to_html(markdown, &options());
    HTML_TEMPLATE.replace("__CSS__", &build_css(settings)).replace("__BODY__", &body)
}

fn build_css(settings: &AppSettings) -> String {
    let mut css = String::from(BASE_CSS);
    css.push_str(match settings.theme {
        Theme::Light => "",
        Theme::Dark => DARK_PALETTE_CSS,
        Theme::System => DARK_PALETTE_MEDIA_QUERY_CSS,
    });
    css.push_str(&format!(
        "\nbody {{ font-family: {}; font-size: {}px; }}",
        format_css_font_family(&settings.preview_font_family),
        settings.preview_font_size
    ));

    // Security: the value comes from a settings file writable from outside the app and
    // is interpolated straight into a CSS rule; "#fff; } body {display:none} x {" would
    // escape it. Emit only a validated hex color.
    if let Some(color) = settings.preview_background_color.as_deref().filter(|c| is_hex_color(c)) {
        css.push_str(&format!("\nbody {{ background: {color} !important; }}"));
    }
    if settings.table_header_gray_background {
        css.push_str("\ntable thead tr { background: rgba(128,128,128,0.15); }");
    }
    if settings.table_row_striping {
        // With a gray header too, start striping on even rows so the two never touch.
        let selector =
            if settings.table_header_gray_background { "table tbody tr:nth-child(even)" } else { "table tbody tr:nth-child(odd)" };
        css.push_str(&format!("\n{selector} {{ background: rgba(128,128,128,0.15); }}"));
    }
    css
}

const GENERIC_FAMILIES: [&str; 6] = ["serif", "sans-serif", "monospace", "cursive", "fantasy", "system-ui"];

/// Formats a comma-separated family list for CSS.
///
/// Security: escaping for the CSS string grammar is not enough here — the result lands
/// in a `<style>` element, which the HTML tokenizer ends at a literal `</style`
/// regardless of CSS quoting. Parts containing `<` or `>` are dropped outright.
pub fn format_css_font_family(value: &str) -> String {
    value
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty() && !p.contains('<') && !p.contains('>'))
        .map(|p| {
            if GENERIC_FAMILIES.iter().any(|g| g.eq_ignore_ascii_case(p)) {
                p.to_string()
            } else {
                format!("\"{}\"", p.replace('\\', "\\\\").replace('"', "\\\""))
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

// Security: default-src 'none' blocks remote stylesheets, fonts, frames and — since
// script-src is unlisted and falls back to it — every form of script. img-src allows
// https: and data: so the toolbar's image button works with remote URLs; the accepted
// cost is that a remote image can confirm the file was opened (no cookies or other
// credentials ride along). base-uri and form-action do NOT fall back to default-src,
// so they are listed explicitly. CSP does not govern top-level navigation; that is the
// navigation guard's job (preview.rs).
const HTML_TEMPLATE: &str = r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8" />
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data: https:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'" />
<style>__CSS__</style>
</head>
<body>__BODY__</body>
</html>"#;

const BASE_CSS: &str = r#"
body {
    font-family: 'Segoe UI', sans-serif;
    font-size: 15px;
    line-height: 1.6;
    margin: 16px 24px;
    color: #1a1a1a;
    background: #ffffff;
    word-wrap: break-word;
}
h1, h2, h3, h4, h5, h6 { font-weight: 600; margin-top: 1.4em; margin-bottom: 0.5em; }
h1 { font-size: 1.8em; border-bottom: 1px solid rgba(128,128,128,0.3); padding-bottom: 0.3em; }
h2 { font-size: 1.5em; border-bottom: 1px solid rgba(128,128,128,0.2); padding-bottom: 0.3em; }
code { font-family: Consolas, 'Cascadia Mono', monospace; background: rgba(128,128,128,0.15); padding: 0.15em 0.35em; border-radius: 4px; }
pre { background: rgba(128,128,128,0.15); padding: 12px; border-radius: 6px; overflow-x: auto; }
pre code { background: none; padding: 0; }
blockquote { border-left: 4px solid rgba(128,128,128,0.4); margin: 0.5em 0; padding: 0.2em 1em; color: rgba(128,128,128,1); }
table { border-collapse: collapse; margin: 0.5em 0; }
table th, table td { border: 1px solid rgba(128,128,128,0.3); padding: 6px 12px; }
img { max-width: 100%; }
a { color: #0067c0; }
"#;

const DARK_PALETTE_MEDIA_QUERY_CSS: &str = r#"
@media (prefers-color-scheme: dark) {
    body { color: #e6e6e6; background: #1e1e1e; }
    code, pre { background: #2b2b2b !important; }
    a { color: #6cb6ff; }
    blockquote { border-left-color: #555; color: #bbb; }
    table th, table td { border-color: #444; }
    hr { border-color: #444; }
}
"#;

const DARK_PALETTE_CSS: &str = r#"
body { color: #e6e6e6; background: #1e1e1e; }
code, pre { background: #2b2b2b !important; }
a { color: #6cb6ff; }
blockquote { border-left-color: #555; color: #bbb; }
table th, table td { border-color: #444; }
hr { border-color: #444; }
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cjk_emphasis_renders_bold() {
        let html = to_html_document("**注意：**内容", &AppSettings::default());
        assert!(html.contains("<strong>注意：</strong>内容"), "{html}");
    }

    #[test]
    fn gfm_extensions() {
        let html = to_html_document("|a|b|\n|-|-|\n|1|2|\n\n~~x~~", &AppSettings::default());
        assert!(html.contains("<table>"));
        assert!(html.contains("<del>x</del>"));
    }

    #[test]
    fn font_family_cannot_break_out_of_style() {
        assert_eq!(format_css_font_family("a</style><img src=x>, Segoe UI, serif"), "\"Segoe UI\", serif");
        assert_eq!(format_css_font_family("A\"B"), "\"A\\\"B\"");
    }

    #[test]
    fn invalid_background_is_not_emitted() {
        let s = AppSettings { preview_background_color: Some("red;}".into()), ..AppSettings::default() };
        assert!(!to_html_document("", &s).contains("red;}"));
    }

    #[test]
    fn striping_starts_on_even_rows_with_gray_header() {
        let s = AppSettings { table_header_gray_background: true, table_row_striping: true, ..AppSettings::default() };
        assert!(to_html_document("", &s).contains("nth-child(even)"));
    }
}
