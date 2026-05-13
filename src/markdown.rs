use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// Convert a Markdown string to Pango markup suitable for `gtk::Label::set_markup()`.
///
/// Supported: bold, italic, strikethrough, inline code, fenced code blocks,
/// headings (H1–H3), unordered and ordered lists, blockquotes, links, rules.
/// Unsupported elements are rendered as plain text.
pub fn to_pango(markdown: &str) -> String {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);

    let parser = Parser::new_ext(markdown, opts);
    let mut out = String::new();
    let mut list_ordered: Vec<bool> = Vec::new();
    let mut list_counter: Vec<u64> = Vec::new();

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

            Event::Start(Tag::CodeBlock(_)) => out.push_str("<tt>"),
            Event::End(TagEnd::CodeBlock) => out.push_str("</tt>\n\n"),

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
                out.push_str(&format!("<a href=\"{}\">", escape_xml(&dest_url)));
            }
            Event::End(TagEnd::Link) => out.push_str("</a>"),

            Event::Code(code) => {
                out.push_str(&format!("<tt>{}</tt>", escape_xml(&code)));
            }

            Event::Text(text) => out.push_str(&escape_xml(&text)),
            Event::SoftBreak => out.push(' '),
            Event::HardBreak => out.push('\n'),

            _ => {}
        }
    }

    out.trim_end().to_string()
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
