//! Markdown → a complete HTML document for the preview.
//!
//! Raw HTML in the Markdown is passed through, minus the elements in `FETCHING_TAGS`.
//! Execution and outbound traffic are shut off in three layers instead, because opening
//! a third party's `.md` is a real path: the CSP below, `IsScriptEnabled = false`, and
//! the handlers in `preview.rs`.

use comrak::nodes::{AstNode, NodeHtmlBlock, NodeValue};
use comrak::{Arena, Options, format_html, parse_document};

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

/// A rendered preview page and the text its copy buttons hand out, by index.
pub struct Rendered {
    pub html: String,
    pub code_blocks: Vec<String>,
}

/// `copy_token` goes into every copy button's link (`copy_link`) and must be unguessable
/// for the document: it is what keeps raw HTML in the Markdown from forging a button.
pub fn to_html_document(markdown: &str, settings: &AppSettings, copy_token: u64) -> Rendered {
    let options = options();
    let arena = Arena::new();
    let root = parse_document(&arena, markdown, &options);
    let code_blocks = add_copy_buttons(&arena, root, copy_token);
    let mut html = String::new();
    // Writing into a String cannot fail.
    let _ = format_html(root, &options, &mut html);
    let body = filter_fetching_tags(&html);
    // Assembled in one pass: substituting into a template would re-scan what was
    // already substituted, so a setting containing a marker could duplicate the body.
    Rendered { html: [HTML_HEAD, &build_css(settings), HTML_MIDDLE, &body, HTML_TAIL].concat(), code_blocks }
}

/// The scheme of a copy button's link. Not a network scheme, so the request filter has
/// nothing to stop; the NavigationStarting handler always cancels it (`preview.rs`).
const COPY_SCHEME: &str = "mymdedit-copy:";

fn copy_link(token: u64, index: usize) -> String {
    format!("{COPY_SCHEME}{token:016x}/{index}")
}

/// The code block a copy link names, if `uri` is one carrying `token`. A link with any
/// other token was not written by this render — forged by the document, or left over
/// from a page that has since been replaced — and names nothing.
pub fn parse_copy_link(uri: &str, token: u64) -> Option<usize> {
    let rest = uri.get(COPY_SCHEME.len()..).filter(|_| is_copy_link(uri))?;
    let (t, index) = rest.split_once('/')?;
    (u64::from_str_radix(t, 16).ok()? == token).then_some(())?;
    index.parse().ok()
}

/// Whether `uri` uses the copy scheme at all, whatever it names.
pub fn is_copy_link(uri: &str) -> bool {
    has_scheme(uri, COPY_SCHEME)
}

/// Whether `uri` starts with `scheme` (`"https:"`), ignoring ASCII case. Compared as
/// bytes: slicing the &str at the scheme's length can land inside a multi-byte character
/// and panic — inside a WebView2 callback, where a panic aborts the process and takes
/// the unsaved document with it.
pub fn has_scheme(uri: &str, scheme: &str) -> bool {
    uri.as_bytes().get(..scheme.len()).is_some_and(|p| p.eq_ignore_ascii_case(scheme.as_bytes()))
}

/// Wraps every code block in a box with a copy button and returns the blocks' text, in
/// the order of the buttons' indexes. Done on the AST, not the HTML: raw HTML can hold
/// `<pre>` too, and counting those would shift every index after it.
fn add_copy_buttons<'a>(arena: &'a Arena<'a>, root: &'a AstNode<'a>, token: u64) -> Vec<String> {
    let blocks: Vec<_> =
        root.descendants().filter(|n| matches!(n.data.borrow().value, NodeValue::CodeBlock(_))).collect();
    let html_block =
        |literal: String| arena.alloc(NodeValue::HtmlBlock(NodeHtmlBlock { block_type: 6, literal }).into());
    let mut texts = Vec::with_capacity(blocks.len());
    for (index, node) in blocks.into_iter().enumerate() {
        let NodeValue::CodeBlock(ref code) = node.data.borrow().value else { unreachable!() };
        texts.push(clipboard_text(&code.literal));
        node.insert_before(html_block(format!(
            "<div class=\"code-block\"><a class=\"copy\" href=\"{}\"><span>コピー</span></a>\n",
            copy_link(token, index)
        )));
        node.insert_after(html_block("</div>\n".to_string()));
    }
    texts
}

/// A code block's text as it goes on the clipboard: CRLF line breaks, which every
/// Windows program pastes correctly, and without the line break that ends the block.
fn clipboard_text(literal: &str) -> String {
    let body = literal.strip_suffix("\r\n").or_else(|| literal.strip_suffix(['\n', '\r'])).unwrap_or(literal);
    body.replace("\r\n", "\n").replace('\r', "\n").replace('\n', "\r\n")
}

/// Elements whose only effect in the preview is to fetch or connect. Security: the
/// runtime layers only see *requests*, and these also act without one — `<link
/// rel=dns-prefetch/preconnect>` resolves and connects to any host, `<iframe src>`
/// opens a connection before FrameNavigationStarting can cancel it (both observed in
/// the 2026-10-14 audit, F8). The content is the only place they can be stopped.
const FETCHING_TAGS: [&str; 10] =
    ["base", "embed", "fencedframe", "frame", "frameset", "iframe", "link", "meta", "object", "portal"];

/// GFM's tagfilter, over `FETCHING_TAGS`: the `<` of such a tag becomes `&lt;`, so it
/// shows as text instead of becoming an element. Safe to run over the whole rendered
/// body: comrak escapes every `<` of Markdown text, so a literal `<tag` can only come
/// from raw HTML.
fn filter_fetching_tags(html: &str) -> String {
    let bytes = html.as_bytes();
    let mut out = String::with_capacity(html.len());
    let mut copied = 0;
    for (i, _) in html.match_indices('<') {
        let name_start = i + 1 + usize::from(bytes.get(i + 1) == Some(&b'/'));
        let fetching = FETCHING_TAGS.iter().any(|tag| {
            let end = name_start + tag.len();
            bytes.get(name_start..end).is_some_and(|name| name.eq_ignore_ascii_case(tag.as_bytes()))
                // Where the HTML tokenizer ends a tag name; anything else is a different
                // (unknown, harmless) element such as <linkx>.
                && bytes.get(end).is_none_or(|b| matches!(b, b'\t' | b'\n' | b'\x0c' | b'\r' | b' ' | b'/' | b'>'))
        });
        if fetching {
            out.push_str(&html[copied..i]);
            out.push_str("&lt;");
            copied = i + 1;
        }
    }
    out.push_str(&html[copied..]);
    out
}

fn build_css(settings: &AppSettings) -> String {
    let mut css = String::from(BASE_CSS);
    match settings.theme {
        Theme::Light => {}
        Theme::Dark => css.push_str(DARK_PALETTE_CSS),
        Theme::System => css.push_str(&format!("\n@media (prefers-color-scheme: dark) {{{DARK_PALETTE_CSS}}}\n")),
    }
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
        let selector = if settings.table_header_gray_background {
            "table tbody tr:nth-child(even)"
        } else {
            "table tbody tr:nth-child(odd)"
        };
        css.push_str(&format!("\n{selector} {{ background: rgba(128,128,128,0.15); }}"));
    }
    css
}

const GENERIC_FAMILIES: [&str; 6] = ["serif", "sans-serif", "monospace", "cursive", "fantasy", "system-ui"];

/// Formats a comma-separated family list for CSS.
///
/// Security: escaping for the CSS string grammar is not enough here — the result lands
/// in a `<style>` element, which the HTML tokenizer ends at a literal `</style`
/// regardless of CSS quoting. Parts containing `<` or `>` are dropped outright. So are
/// parts with control characters: a CSS string also ends at a newline (LF, CR or FF),
/// as a bad string, and a `}` after it would close the rule. No font name has one.
pub fn format_css_font_family(value: &str) -> String {
    value
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty() && !p.contains(['<', '>']) && !p.contains(char::is_control))
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
const HTML_HEAD: &str = r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8" />
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data: https:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'" />
<style>"#;
const HTML_MIDDLE: &str = "</style>\n</head>\n<body>";
const HTML_TAIL: &str = "</body>\n</html>";

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
.code-block { position: relative; }
.code-block .copy {
    position: absolute; top: 6px; right: 6px; opacity: 0;
    font: 12px 'Segoe UI', sans-serif; color: inherit; text-decoration: none; user-select: none;
    background: rgba(128,128,128,0.25); border: 1px solid rgba(128,128,128,0.4); border-radius: 4px; padding: 2px 8px;
}
.code-block:hover .copy, .code-block .copy:focus { opacity: 1; }
.code-block .copy:hover { background: rgba(128,128,128,0.4); }
/* Scripts are off, so this is the only feedback: a click focuses the link without
   :focus-visible, which Tab does not. */
.code-block .copy:focus:not(:focus-visible) span { display: none; }
.code-block .copy:focus:not(:focus-visible)::after { content: "コピーしました"; }
blockquote { border-left: 4px solid rgba(128,128,128,0.4); margin: 0.5em 0; padding: 0.2em 1em; color: rgba(128,128,128,1); }
table { border-collapse: collapse; margin: 0.5em 0; }
table th, table td { border: 1px solid rgba(128,128,128,0.3); padding: 6px 12px; }
img { max-width: 100%; }
a { color: #0067c0; }
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
        let html = to_html_document("**注意：**内容", &AppSettings::default(), 0).html;
        assert!(html.contains("<strong>注意：</strong>内容"), "{html}");
    }

    #[test]
    fn gfm_extensions() {
        let html = to_html_document("|a|b|\n|-|-|\n|1|2|\n\n~~x~~", &AppSettings::default(), 0).html;
        assert!(html.contains("<table>"));
        assert!(html.contains("<del>x</del>"));
    }

    #[test]
    fn font_family_cannot_break_out_of_style() {
        assert_eq!(format_css_font_family("a</style><img src=x>, Segoe UI, serif"), "\"Segoe UI\", serif");
        assert_eq!(format_css_font_family("A\"B"), "\"A\\\"B\"");
    }

    #[test]
    fn fetching_tags_render_as_text() {
        let md = "<link rel=\"preconnect\" href=\"https://evil/\">\n\n\
                  <IFRAME\tsrc=\"https://evil/\"></iframe>\n\n\
                  text <meta http-equiv=\"refresh\" content=\"0;url=http://evil/\"> <base/href=\"https://evil/\">";
        let html = to_html_document(md, &AppSettings::default(), 0).html;
        let body = &html[html.find("<body>").unwrap()..];
        for tag in ["<link", "<iframe", "<IFRAME", "</iframe", "<meta", "<base"] {
            assert!(!body.contains(tag), "{tag} survived: {body}");
        }
        assert!(body.contains("&lt;link rel"), "{body}");
        // The app's own CSP <meta> sits in the head, outside the filter.
        assert!(html.contains("<meta http-equiv=\"Content-Security-Policy\""));
    }

    #[test]
    fn harmless_html_is_untouched() {
        let md = "<details><summary>s</summary>x</details>\n\n<img src=\"https://example.com/a.png\"> <linkx> <a href=\"#m\">metadata</a>";
        let html = to_html_document(md, &AppSettings::default(), 0).html;
        for kept in ["<details>", "<img src=\"https://example.com/a.png\">", "<linkx>", "<a href=\"#m\">metadata</a>"] {
            assert!(html.contains(kept), "{kept} lost: {html}");
        }
        // Markdown text is already escaped by comrak and must not be double-escaped.
        assert!(to_html_document("a < link", &AppSettings::default(), 0).html.contains("a &lt; link"));
        assert_eq!(filter_fetching_tags("日本語<link>"), "日本語&lt;link>");
        assert_eq!(filter_fetching_tags("<"), "<");
    }

    #[test]
    fn font_family_cannot_break_out_of_its_rule() {
        // A newline ends a CSS string as a <bad-string-token>; the `}` after it then
        // closes the body rule and everything behind it becomes a rule of its own.
        for nl in ["\n", "\r", "\x0c"] {
            let css = format_css_font_family(&format!("a{nl}}} body{{display:none}} x{{, serif"));
            assert_eq!(css, "serif", "{nl:?}");
        }
    }

    #[test]
    fn invalid_background_is_not_emitted() {
        let s = AppSettings { preview_background_color: Some("red;}".into()), ..AppSettings::default() };
        assert!(!to_html_document("", &s, 0).html.contains("red;}"));
    }

    #[test]
    fn css_cannot_inject_template_markers() {
        let s = AppSettings { preview_font_family: "__BODY__".into(), ..AppSettings::default() };
        let html = to_html_document("unique-body-text", &s, 0).html;
        assert_eq!(html.matches("unique-body-text").count(), 1);
    }

    #[test]
    fn system_theme_wraps_the_same_dark_palette_in_a_media_query() {
        let html = to_html_document("", &AppSettings::default(), 0).html;
        assert!(html.contains("@media (prefers-color-scheme: dark)"));
        assert!(html.contains("background: #1e1e1e"));
    }

    #[test]
    fn code_blocks_get_copy_buttons_in_order() {
        let md = "<pre>raw</pre>\n\n```rust\nfn a() {}\r\nb\n```\n\n- item\n\n      indented\n";
        let r = to_html_document(md, &AppSettings::default(), 0xab);
        assert_eq!(r.code_blocks, ["fn a() {}\r\nb", "indented"]);
        for i in 0..2 {
            assert!(r.html.contains(&format!("href=\"mymdedit-copy:00000000000000ab/{i}\"")), "{}", r.html);
        }
        assert_eq!(r.html.matches("<div class=\"code-block\">").count(), 2);
        assert_eq!(r.html.matches("<div").count(), r.html.matches("</div>").count());
    }

    #[test]
    fn copy_links_only_name_blocks_of_this_render() {
        assert_eq!(parse_copy_link(&copy_link(7, 3), 7), Some(3));
        assert_eq!(parse_copy_link("MYMDEDIT-COPY:0000000000000007/3", 7), Some(3));
        assert_eq!(parse_copy_link(&copy_link(8, 3), 7), None);
        for bad in ["mymdedit-copy:", "mymdedit-copy:7", "mymdedit-copy:7/x", "mymdedit-copy:日本/1", "mymdedit-cop"]
        {
            assert_eq!(parse_copy_link(bad, 7), None, "{bad}");
        }
        assert!(is_copy_link("mymdedit-copy:x") && !is_copy_link("https://x") && !is_copy_link("日本語"));
        assert!(has_scheme("HTTPS://x", "https:") && !has_scheme("ht日", "https:") && !has_scheme("http", "https:"));
    }

    #[test]
    fn clipboard_text_uses_crlf_without_the_final_break() {
        assert_eq!(clipboard_text("a\nb\n"), "a\r\nb");
        assert_eq!(clipboard_text("a\rb\r"), "a\r\nb");
        assert_eq!(clipboard_text("a\r\n\r\n"), "a\r\n");
        assert_eq!(clipboard_text(""), "");
    }

    #[test]
    fn striping_starts_on_even_rows_with_gray_header() {
        let s = AppSettings { table_header_gray_background: true, table_row_striping: true, ..AppSettings::default() };
        assert!(to_html_document("", &s, 0).html.contains("nth-child(even)"));
    }
}
