use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// Convert a Markdown string to Pango markup suitable for `gtk::Label::set_markup()`.
///
/// Supported: bold, italic, strikethrough, inline code, fenced code blocks,
/// headings (H1–H3), unordered and ordered lists, blockquotes, links, rules.
/// Bare http(s) URLs in plain text are auto-linked.
/// Unsupported elements are rendered as plain text.
pub fn to_pango(markdown: &str) -> String {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);

    let parser = Parser::new_ext(markdown, opts);
    let mut out = String::new();
    let mut list_ordered: Vec<bool> = Vec::new();
    let mut list_counter: Vec<u64> = Vec::new();
    // Track nesting so we skip auto-linking inside explicit links or code.
    let mut in_link: u32 = 0;
    let mut in_code: u32 = 0;

    for event in parser {
        match event {
            // ── Block elements ──────────────────────────────────────────────

            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => out.push_str("\n\n"),

            Event::Start(Tag::Heading { level, .. }) => match level {
                HeadingLevel::H1 => out.push_str("<span size=\"x-large\"><b>"),
                HeadingLevel::H2 => out.push_str("<span size=\"large\"><b>"),
                _ => out.push_str("<b>"),
            },
            Event::End(TagEnd::Heading(level)) => match level {
                HeadingLevel::H1 | HeadingLevel::H2 => out.push_str("</b></span>\n\n"),
                _ => out.push_str("</b>\n\n"),
            },

            Event::Start(Tag::BlockQuote(_)) => out.push_str("<i>▌ "),
            Event::End(TagEnd::BlockQuote) => out.push_str("</i>"),

            Event::Start(Tag::CodeBlock(_)) => {
                in_code += 1;
                out.push_str("<tt>");
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code -= 1;
                out.push_str("</tt>\n\n");
            }

            Event::Start(Tag::List(start)) => {
                if let Some(n) = start {
                    list_ordered.push(true);
                    list_counter.push(n);
                } else {
                    list_ordered.push(false);
                    list_counter.push(0);
                }
            }
            Event::End(TagEnd::List(_)) => {
                list_ordered.pop();
                list_counter.pop();
                out.push('\n');
            }
            Event::Start(Tag::Item) => {
                let depth = list_ordered.len();
                let indent = "  ".repeat(depth.saturating_sub(1));
                let is_ordered = *list_ordered.last().unwrap_or(&false);
                if is_ordered {
                    let n = list_counter.last_mut().unwrap();
                    out.push_str(&format!("{indent}{n}. "));
                    *n += 1;
                } else {
                    out.push_str(&format!("{indent}• "));
                }
            }
            Event::End(TagEnd::Item) => out.push('\n'),

            Event::Rule => out.push_str("──────────────────\n\n"),

            // ── Inline elements ─────────────────────────────────────────────

            Event::Start(Tag::Strong) => out.push_str("<b>"),
            Event::End(TagEnd::Strong) => out.push_str("</b>"),

            Event::Start(Tag::Emphasis) => out.push_str("<i>"),
            Event::End(TagEnd::Emphasis) => out.push_str("</i>"),

            Event::Start(Tag::Strikethrough) => out.push_str("<s>"),
            Event::End(TagEnd::Strikethrough) => out.push_str("</s>"),

            Event::Start(Tag::Link { dest_url, .. }) => {
                in_link += 1;
                out.push_str(&format!("<a href=\"{}\">", escape_xml(&dest_url)));
            }
            Event::End(TagEnd::Link) => {
                in_link -= 1;
                out.push_str("</a>");
            }

            Event::Code(code) => {
                out.push_str(&format!("<tt>{}</tt>", escape_xml(&code)));
            }

            Event::Text(text) => {
                if in_link > 0 || in_code > 0 {
                    out.push_str(&escape_xml(&text));
                } else {
                    out.push_str(&linkify(&text));
                }
            }
            Event::SoftBreak => out.push(' '),
            Event::HardBreak => out.push('\n'),

            _ => {}
        }
    }

    out.trim_end().to_string()
}

/// Escape plain text for use in Pango markup, then wrap any bare http(s) URLs
/// in anchor tags. Use this for content that is plain text (e.g. user bubbles).
pub fn linkify_plain(text: &str) -> String {
    linkify(text)
}

/// Detect bare `http://` / `https://` URLs in plain text and wrap them in
/// `<a href="…">…</a>` tags. Non-URL segments are XML-escaped.
fn linkify(text: &str) -> String {
    let mut result = String::new();
    let mut rest = text;

    loop {
        // Pick the earlier of http:// and https://, if any.
        let pos = match (rest.find("http://"), rest.find("https://")) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) | (None, Some(a)) => Some(a),
            (None, None) => None,
        };

        match pos {
            None => {
                result.push_str(&escape_xml(rest));
                break;
            }
            Some(idx) => {
                result.push_str(&escape_xml(&rest[..idx]));

                let after = &rest[idx..];
                let len = url_end(after);
                let url = &after[..len];
                let escaped = escape_xml(url);
                result.push_str(&format!("<a href=\"{escaped}\">{escaped}</a>"));
                rest = &after[len..];
            }
        }
    }

    result
}

/// Find the byte length of the URL starting at the beginning of `s`.
/// Stops at whitespace and strips trailing punctuation unlikely to be part of the URL.
fn url_end(s: &str) -> usize {
    let raw_end = s.find(|c: char| c.is_whitespace()).unwrap_or(s.len());
    let url = s[..raw_end].trim_end_matches(|c| matches!(c, '.' | ',' | ')' | ']' | '\'' | '"'));
    url.len()
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
