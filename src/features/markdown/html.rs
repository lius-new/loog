//! A small, closed subset of HTML found in READMEs, turned into operations the
//! parser applies to the document: centered headings and paragraphs, images,
//! links, line breaks and basic emphasis. Anything outside the subset makes
//! [`plan`] return `None`, and the block stays literal so no content is lost.
use crate::features::markdown::model::{ColumnAlignment, NodeKind};
use std::ops::Range;

/// Elements nested deeper than this are not interpreted.
const MAX_DEPTH: usize = 24;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// Starts a paragraph, heading or inline wrapper; a later `Close` ends it.
    Open {
        kind: NodeKind,
        align: ColumnAlignment,
        range: Range<usize>,
    },
    Close,
    Text {
        text: String,
        range: Range<usize>,
    },
    Code {
        text: String,
        range: Range<usize>,
    },
    Image {
        destination: String,
        title: String,
        alt: String,
        align: ColumnAlignment,
        range: Range<usize>,
    },
    HardBreak {
        range: Range<usize>,
    },
    Rule {
        range: Range<usize>,
    },
}

#[derive(Debug)]
enum Token {
    Open {
        name: String,
        attrs: Vec<(String, String)>,
        self_closing: bool,
        range: Range<usize>,
    },
    Close {
        name: String,
        range: Range<usize>,
    },
    Text {
        range: Range<usize>,
    },
}

fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

fn tokenize(html: &str) -> Vec<Token> {
    let bytes = html.as_bytes();
    let mut tokens = Vec::new();
    let mut text_start = 0;
    let mut at = 0;
    let flush = |tokens: &mut Vec<Token>, from: usize, to: usize| {
        if from < to {
            tokens.push(Token::Text { range: from..to });
        }
    };
    while at < bytes.len() {
        if bytes[at] != b'<' {
            at += 1;
            continue;
        }
        let rest = &html[at..];
        if rest.starts_with("<!--") {
            flush(&mut tokens, text_start, at);
            at = rest.find("-->").map_or(html.len(), |end| at + end + 3);
            text_start = at;
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            flush(&mut tokens, text_start, at);
            at = rest.find('>').map_or(html.len(), |end| at + end + 1);
            text_start = at;
            continue;
        }
        let closing = rest.starts_with("</");
        let name_at = at + if closing { 2 } else { 1 };
        if !bytes.get(name_at).is_some_and(u8::is_ascii_alphabetic) {
            at += 1;
            continue;
        }
        let Some((end, name, attrs, self_closing)) = tag(html, name_at) else {
            at += 1;
            continue;
        };
        flush(&mut tokens, text_start, at);
        tokens.push(if closing {
            Token::Close {
                name,
                range: at..end,
            }
        } else {
            Token::Open {
                name,
                attrs,
                self_closing,
                range: at..end,
            }
        });
        at = end;
        text_start = at;
    }
    flush(&mut tokens, text_start, html.len());
    tokens
}

type Tag = (usize, String, Vec<(String, String)>, bool);

/// Reads a tag whose name starts at `name_at`; returns the end of the tag.
fn tag(html: &str, name_at: usize) -> Option<Tag> {
    let bytes = html.as_bytes();
    let mut at = name_at;
    while bytes
        .get(at)
        .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'-')
    {
        at += 1;
    }
    let name = html[name_at..at].to_ascii_lowercase();
    let mut attrs = Vec::new();
    loop {
        while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        match bytes.get(at)? {
            b'>' => return Some((at + 1, name, attrs, false)),
            b'/' => {
                at += 1;
                if bytes.get(at) == Some(&b'>') {
                    return Some((at + 1, name, attrs, true));
                }
            }
            _ => {
                let key_at = at;
                while bytes
                    .get(at)
                    .is_some_and(|b| !b.is_ascii_whitespace() && !matches!(b, b'=' | b'>' | b'/'))
                {
                    at += 1;
                }
                if at == key_at {
                    return None;
                }
                let key = html[key_at..at].to_ascii_lowercase();
                while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
                    at += 1;
                }
                let mut value = String::new();
                if bytes.get(at) == Some(&b'=') {
                    at += 1;
                    while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
                        at += 1;
                    }
                    match bytes.get(at)? {
                        quote @ (b'"' | b'\'') => {
                            let begin = at + 1;
                            let end = begin + html[begin..].find(*quote as char)?;
                            value = decode(&html[begin..end]);
                            at = end + 1;
                        }
                        _ => {
                            let begin = at;
                            while bytes
                                .get(at)
                                .is_some_and(|b| !b.is_ascii_whitespace() && *b != b'>')
                            {
                                at += 1;
                            }
                            value = decode(&html[begin..at]);
                        }
                    }
                }
                attrs.push((key, value));
            }
        }
    }
}

/// Decodes the common named entities and numeric references.
fn decode(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let decoded = rest.find(';').filter(|end| *end <= 10).and_then(|end| {
            let name = &rest[1..end];
            let value = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some('\u{a0}'),
                _ => name
                    .strip_prefix('#')
                    .and_then(|number| match number.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok(),
                        None => number.parse().ok(),
                    })
                    .and_then(char::from_u32),
            };
            value.map(|c| (c, end + 1))
        });
        match decoded {
            Some((c, used)) => {
                out.push(c);
                rest = &rest[used..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn alignment(value: Option<&str>, inherited: ColumnAlignment) -> ColumnAlignment {
    match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("center") => ColumnAlignment::Center,
        Some("right") => ColumnAlignment::Right,
        Some("left") => ColumnAlignment::Left,
        _ => inherited,
    }
}

enum Class {
    Heading(u8),
    Paragraph,
    Container,
    Inline,
    Image,
    Break,
    Rule,
    Skip,
    Unsupported,
}

fn classify(name: &str) -> Class {
    match name {
        "h1" => Class::Heading(1),
        "h2" => Class::Heading(2),
        "h3" => Class::Heading(3),
        "h4" => Class::Heading(4),
        "h5" => Class::Heading(5),
        "h6" => Class::Heading(6),
        "p" => Class::Paragraph,
        "div" | "center" | "section" | "header" | "footer" | "article" | "main" | "nav"
        | "aside" | "figure" | "figcaption" => Class::Container,
        "a" | "b" | "strong" | "i" | "em" | "s" | "del" | "strike" | "code" | "span" | "u"
        | "small" | "sup" | "sub" | "kbd" | "font" | "picture" | "source" | "mark" | "abbr" => {
            Class::Inline
        }
        "img" => Class::Image,
        "br" => Class::Break,
        "hr" => Class::Rule,
        "script" | "style" => Class::Skip,
        _ => Class::Unsupported,
    }
}

struct Emitter {
    ops: Vec<Op>,
    /// Index in `ops` of the open paragraph or heading, if any.
    block: Option<usize>,
    /// Alignment of the open block, inherited by what it contains.
    block_align: ColumnAlignment,
    /// Open inline wrappers that produced an `Open` op, innermost last.
    inline: Vec<(String, bool)>,
    containers: Vec<ColumnAlignment>,
    /// Text collected for an open `<code>`.
    code: Option<(String, Range<usize>)>,
    /// Whether the last emitted piece ended with a break or the block's start.
    skip_space: bool,
}

impl Emitter {
    fn align(&self) -> ColumnAlignment {
        self.containers
            .last()
            .copied()
            .unwrap_or(ColumnAlignment::Left)
    }

    fn open_block(&mut self, kind: NodeKind, align: ColumnAlignment, range: Range<usize>) {
        self.close_block();
        self.block = Some(self.ops.len());
        self.block_align = align;
        self.ops.push(Op::Open { kind, align, range });
        self.skip_space = true;
    }

    fn ensure_block(&mut self, range: &Range<usize>) {
        if self.block.is_none() {
            let align = self.align();
            self.open_block(NodeKind::Paragraph, align, range.start..range.start);
        }
    }

    fn close_inline(&mut self) {
        while let Some((_, opened)) = self.inline.pop() {
            if opened {
                self.ops.push(Op::Close);
            }
        }
    }

    fn close_block(&mut self) {
        self.flush_code();
        self.close_inline();
        let Some(open) = self.block.take() else {
            return;
        };
        // Trailing white space is not part of the text.
        if let Some(Op::Text { text, .. }) = self.ops.last_mut() {
            text.truncate(text.trim_end_matches(' ').len());
            if text.is_empty() {
                self.ops.pop();
            }
        }
        if open + 1 == self.ops.len() {
            self.ops.pop();
        } else {
            self.ops.push(Op::Close);
        }
    }

    fn flush_code(&mut self) {
        if let Some((text, range)) = self.code.take()
            && !text.is_empty()
        {
            self.ops.push(Op::Code { text, range });
            self.skip_space = false;
        }
    }

    fn text(&mut self, raw: &str, range: Range<usize>) {
        let decoded = decode(raw);
        if let Some((code, span)) = self.code.as_mut() {
            code.push_str(&decoded);
            span.end = range.end;
            return;
        }
        let mut text = String::with_capacity(decoded.len());
        let mut space = self.skip_space;
        for c in decoded.chars() {
            if matches!(c, ' ' | '\t' | '\n' | '\r') {
                if !space {
                    text.push(' ');
                    space = true;
                }
            } else {
                text.push(c);
                space = false;
            }
        }
        if text.is_empty() || (self.block.is_none() && text.trim().is_empty()) {
            return;
        }
        self.ensure_block(&range);
        self.skip_space = text.ends_with(' ');
        self.ops.push(Op::Text { text, range });
    }
}

/// The operations for an HTML block, or `None` when it uses anything outside
/// the supported subset.
pub fn plan(html: &str) -> Option<Vec<Op>> {
    let mut e = Emitter {
        ops: Vec::new(),
        block: None,
        block_align: ColumnAlignment::Left,
        inline: Vec::new(),
        containers: Vec::new(),
        code: None,
        skip_space: true,
    };
    let mut skip: Option<String> = None;
    let mut open_blocks: Vec<String> = Vec::new();
    for token in tokenize(html) {
        if let Some(name) = &skip {
            if matches!(&token, Token::Close { name: close, .. } if close == name) {
                skip = None;
            }
            continue;
        }
        match token {
            Token::Text { range } => e.text(&html[range.clone()], range),
            Token::Open {
                name,
                attrs,
                self_closing,
                range,
            } => {
                if e.inline.len() + e.containers.len() + open_blocks.len() > MAX_DEPTH {
                    return None;
                }
                let class = classify(&name);
                let align = alignment(attr(&attrs, "align"), e.align());
                match class {
                    Class::Unsupported => return None,
                    Class::Skip => {
                        if !self_closing {
                            skip = Some(name);
                        }
                    }
                    Class::Heading(level) => {
                        e.open_block(NodeKind::Heading { level }, align, range);
                        if !self_closing {
                            open_blocks.push(name);
                        }
                    }
                    Class::Paragraph => {
                        e.open_block(NodeKind::Paragraph, align, range);
                        if !self_closing {
                            open_blocks.push(name);
                        }
                    }
                    Class::Container => {
                        e.close_block();
                        let center = name == "center";
                        e.containers.push(if center {
                            ColumnAlignment::Center
                        } else {
                            align
                        });
                        if self_closing {
                            e.containers.pop();
                        } else {
                            open_blocks.push(name);
                        }
                    }
                    Class::Image => {
                        let Some(src) = attr(&attrs, "src").filter(|src| !src.is_empty()) else {
                            continue;
                        };
                        e.ensure_block(&range);
                        e.flush_code();
                        e.ops.push(Op::Image {
                            destination: src.to_owned(),
                            title: attr(&attrs, "title").unwrap_or("").to_owned(),
                            alt: attr(&attrs, "alt").unwrap_or("").to_owned(),
                            align: alignment(attr(&attrs, "align"), e.block_align),
                            range,
                        });
                        e.skip_space = false;
                    }
                    Class::Break => {
                        e.ensure_block(&range);
                        e.flush_code();
                        e.ops.push(Op::HardBreak { range });
                        e.skip_space = true;
                    }
                    Class::Rule => {
                        e.close_block();
                        e.ops.push(Op::Rule { range });
                    }
                    Class::Inline => {
                        if self_closing {
                            continue;
                        }
                        let kind = match name.as_str() {
                            "a" => attr(&attrs, "href").map(|href| NodeKind::Link {
                                destination: href.to_owned(),
                                title: attr(&attrs, "title").unwrap_or("").to_owned(),
                            }),
                            "b" | "strong" => Some(NodeKind::Strong),
                            "i" | "em" => Some(NodeKind::Emphasis),
                            "s" | "del" | "strike" => Some(NodeKind::Strikethrough),
                            _ => None,
                        };
                        let mut opened = false;
                        if name == "code" {
                            e.ensure_block(&range);
                            e.code = Some((String::new(), range.end..range.end));
                        } else if let Some(kind) = kind {
                            e.ensure_block(&range);
                            e.flush_code();
                            e.ops.push(Op::Open {
                                kind,
                                align: ColumnAlignment::Left,
                                range,
                            });
                            opened = true;
                        }
                        e.inline.push((name, opened));
                    }
                }
            }
            Token::Close { name, .. } => match classify(&name) {
                Class::Heading(_) | Class::Paragraph => {
                    if open_blocks.last() == Some(&name) {
                        open_blocks.pop();
                        e.close_block();
                    }
                }
                Class::Container => {
                    if open_blocks.last() == Some(&name) {
                        open_blocks.pop();
                        e.close_block();
                        e.containers.pop();
                    }
                }
                Class::Inline => {
                    if name == "code" {
                        e.flush_code();
                    }
                    if let Some(index) = e.inline.iter().rposition(|(open, _)| *open == name) {
                        // Close anything opened inside it first.
                        while e.inline.len() > index {
                            let (_, opened) = e.inline.pop().expect("length checked");
                            if opened {
                                e.ops.push(Op::Close);
                            }
                        }
                    }
                }
                _ => {}
            },
        }
    }
    e.close_block();
    Some(e.ops)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(ops: &[Op]) -> Vec<String> {
        ops.iter()
            .map(|op| match op {
                Op::Open { kind, align, .. } => format!("open {kind:?} {align:?}"),
                Op::Close => "close".to_owned(),
                Op::Text { text, .. } => format!("text {text:?}"),
                Op::Code { text, .. } => format!("code {text:?}"),
                Op::Image {
                    destination,
                    alt,
                    align,
                    ..
                } => format!("image {destination} {alt:?} {align:?}"),
                Op::HardBreak { .. } => "break".to_owned(),
                Op::Rule { .. } => "rule".to_owned(),
            })
            .collect()
    }

    #[test]
    fn a_centered_logo_heading_and_tagline_become_aligned_blocks() {
        let html = "<p align=\"center\">\n  <img src=\"a.png\" alt=\"Loog\" width=\"96\">\n</p>\n<h1 align=\"center\">Loog</h1>\n<p align=\"center\">\n  A fast editor.\n</p>\n";
        let ops = plan(html).unwrap();
        assert_eq!(
            kinds(&ops),
            [
                "open Paragraph Center",
                "image a.png \"Loog\" Center",
                "close",
                "open Heading { level: 1 } Center",
                "text \"Loog\"",
                "close",
                "open Paragraph Center",
                "text \"A fast editor.\"",
                "close",
            ]
        );
    }

    #[test]
    fn badges_are_links_around_images() {
        let html = "<p align=\"center\">\n<a href=\"https://x.y/ci\"><img src=\"https://x.y/b.svg\" alt=\"CI\"></a>\n<a href=\"#license\"><img src=\"l.svg\" alt=\"License\"></a>\n</p>";
        let ops = plan(html).unwrap();
        let shown = kinds(&ops);
        assert_eq!(shown[0], "open Paragraph Center");
        assert!(shown[1].starts_with("open Link"));
        assert_eq!(shown[2], "image https://x.y/b.svg \"CI\" Center");
        assert_eq!(shown[3], "close");
        assert_eq!(shown.iter().filter(|op| *op == "close").count(), 3);
    }

    #[test]
    fn center_container_applies_to_loose_text_and_nested_blocks() {
        let ops = plan("<center>Hello <b>big</b> &amp; <code>x</code><br>next</center>").unwrap();
        assert_eq!(
            kinds(&ops),
            [
                "open Paragraph Center",
                "text \"Hello \"",
                "open Strong Left",
                "text \"big\"",
                "close",
                "text \" & \"",
                "code \"x\"",
                "break",
                "text \"next\"",
                "close",
            ]
        );
    }

    #[test]
    fn comments_scripts_and_blank_space_produce_nothing() {
        let ops = plan("<!-- hidden -->\n<script>alert(1)</script>\n\n").unwrap();
        assert!(ops.is_empty());
    }

    #[test]
    fn unsupported_elements_keep_the_block_literal() {
        for html in [
            "<table><tr><td>x</td></tr></table>",
            "<pre>a  b</pre>",
            "<details><summary>s</summary>x</details>",
            "<ul><li>x</li></ul>",
            "<svg></svg>",
        ] {
            assert!(plan(html).is_none(), "{html}");
        }
    }

    #[test]
    fn entities_and_attribute_forms_are_read() {
        let ops = plan("<p>&lt;&#65;&#x42;&nbsp;&bogus;</p><img src=a.png alt='x y'>").unwrap();
        let shown = kinds(&ops);
        // Debug output escapes the no-break space.
        assert_eq!(shown[1], r#"text "<AB\u{a0}&bogus;""#);
        assert_eq!(shown[3], "open Paragraph Left");
        assert_eq!(shown[4], "image a.png \"x y\" Left");
    }

    #[test]
    fn multibyte_text_ranges_stay_on_char_boundaries() {
        let html = "<h2>概览（说明）</h2>";
        let ops = plan(html).unwrap();
        for op in &ops {
            if let Op::Text { range, .. } | Op::Open { range, .. } = op {
                assert!(html.is_char_boundary(range.start));
                assert!(html.is_char_boundary(range.end));
            }
        }
    }
}
