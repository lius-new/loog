//! A small highlighter for text that no grammar covers.
//!
//! It reads one line at a time and keeps no state, so it needs no parse, no
//! language pack and no cache. It colours only what is hard to mistake in any
//! language: comments (by the markers the language declares), quoted strings on
//! one line, numbers, a few keywords and the usual constants. It never guesses
//! at functions or types; a missed colour costs less than a wrong one.
//!
//! It produces the same `TokenSpan`s a grammar does, so themes and the display
//! do not know which one made them. Whatever provides real syntax (today
//! Tree-sitter, later an extension) replaces it for the languages it covers.
use super::TokenSpan;
use crate::features::highlighting::HighlightId;
use std::ops::Range;

/// Longer lines (minified code, data dumps) are left plain.
pub(crate) const MAX_LINE_BYTES: usize = 4096;

/// Words that rarely appear in prose, so they are safe to colour in code.
const KEYWORDS: &[&str] = &[
    "if",
    "else",
    "elif",
    "for",
    "while",
    "do",
    "return",
    "break",
    "continue",
    "switch",
    "case",
    "match",
    "fn",
    "func",
    "function",
    "def",
    "lambda",
    "class",
    "struct",
    "enum",
    "impl",
    "trait",
    "interface",
    "let",
    "const",
    "var",
    "static",
    "pub",
    "public",
    "private",
    "protected",
    "import",
    "export",
    "use",
    "mod",
    "namespace",
    "async",
    "await",
    "yield",
    "try",
    "catch",
    "finally",
    "throw",
    "throws",
    "raise",
];
const CONSTANTS: &[&str] = &[
    "true",
    "false",
    "null",
    "nil",
    "None",
    "True",
    "False",
    "undefined",
];

/// What a language declares that the scan can use.
pub(crate) struct Rules<'a> {
    pub line_comment: Option<&'a str>,
    pub block_comment: Option<(&'a str, &'a str)>,
    /// Colour keywords and constants. Off for prose-like or data languages,
    /// where those words are ordinary text.
    pub keywords: bool,
}

/// Spans that tile `window` exactly, which lies inside `line` of `text`.
pub(crate) fn highlight(
    text: &str,
    line: &Range<usize>,
    window: &Range<usize>,
    rules: &Rules,
) -> Vec<TokenSpan> {
    let plain = || {
        vec![TokenSpan {
            range: window.clone(),
            class: None,
        }]
    };
    if window.is_empty() || !(line.start <= window.start && window.end <= line.end) {
        return if window.is_empty() {
            Vec::new()
        } else {
            plain()
        };
    }
    let Some(source) = text.get(line.clone()).filter(|l| l.len() <= MAX_LINE_BYTES) else {
        return plain();
    };
    let mut out: Vec<TokenSpan> = Vec::new();
    let mut cursor = window.start;
    let mut add = |range: Range<usize>, class: Option<HighlightId>| {
        if range.is_empty() {
            return;
        }
        match out.last_mut() {
            Some(last) if last.class == class && last.range.end == range.start => {
                last.range.end = range.end;
            }
            _ => out.push(TokenSpan { range, class }),
        }
    };
    for (token, class) in tokens(source, rules) {
        let start = (token.start + line.start).max(cursor);
        let end = (token.end + line.start).min(window.end);
        if start >= end {
            continue;
        }
        add(cursor..start, None);
        add(start..end, Some(class));
        cursor = end;
    }
    add(cursor..window.end, None);
    out
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The coloured runs of one line, as offsets into it.
fn tokens(line: &str, rules: &Rules) -> Vec<(Range<usize>, HighlightId)> {
    let mut out = Vec::new();
    let mut previous: Option<char> = None;
    let mut at = 0;
    while at < line.len() {
        let rest = &line[at..];
        let c = rest.chars().next().unwrap_or(' ');
        let (end, class) = scan(rest, c, previous, rules);
        let end = at + end.max(c.len_utf8());
        if let Some(class) = class {
            out.push((at..end, class));
        }
        previous = line[..end].chars().next_back();
        at = end;
    }
    out
}

/// How far the token at the start of `rest` reaches, and its class if it has one.
fn scan(
    rest: &str,
    c: char,
    previous: Option<char>,
    rules: &Rules,
) -> (usize, Option<HighlightId>) {
    if let Some((open, close)) = rules.block_comment.filter(|(open, _)| !open.is_empty())
        && rest.starts_with(open)
    {
        let end = rest[open.len()..]
            .find(close)
            .map_or(rest.len(), |at| open.len() + at + close.len());
        return (end, Some(HighlightId::COMMENT));
    }
    if let Some(marker) = rules.line_comment.filter(|m| !m.is_empty())
        && rest.starts_with(marker)
    {
        return (rest.len(), Some(HighlightId::COMMENT));
    }
    if matches!(c, '"' | '\'' | '`') {
        // An apostrophe inside a word is not a quote.
        let apostrophe = c == '\'' && previous.is_some_and(is_word);
        if !apostrophe && let Some(end) = string_end(rest, c) {
            return (end, Some(HighlightId::STRING));
        }
        return (c.len_utf8(), None);
    }
    if c.is_ascii_digit() && !previous.is_some_and(is_word) {
        let mut end = 0;
        let mut chars = rest.char_indices().peekable();
        while let Some((at, ch)) = chars.next() {
            let dot_in_number = ch == '.' && chars.peek().is_some_and(|(_, n)| n.is_ascii_digit());
            if !(ch.is_ascii_alphanumeric() || ch == '_' || dot_in_number) {
                break;
            }
            end = at + ch.len_utf8();
        }
        return (end, Some(HighlightId::NUMBER));
    }
    if c.is_alphabetic() || c == '_' {
        let end = rest.find(|ch: char| !is_word(ch)).unwrap_or(rest.len());
        let word = &rest[..end];
        let class = if !rules.keywords {
            None
        } else if KEYWORDS.contains(&word) {
            Some(HighlightId::KEYWORD)
        } else if CONSTANTS.contains(&word) {
            Some(HighlightId::CONSTANT_BUILTIN)
        } else {
            None
        };
        return (end, class);
    }
    (c.len_utf8(), None)
}

/// The end of the string that opens `rest` with `quote`, if it closes on this line.
fn string_end(rest: &str, quote: char) -> Option<usize> {
    let mut chars = rest.char_indices().skip(1);
    while let Some((at, ch)) = chars.next() {
        if ch == '\\' {
            chars.next();
        } else if ch == quote {
            return Some(at + ch.len_utf8());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const C_LIKE: Rules = Rules {
        line_comment: Some("//"),
        block_comment: Some(("/*", "*/")),
        keywords: true,
    };

    /// `(text, class)` pairs of the whole single-line `text`.
    fn parts<'a>(text: &'a str, rules: &Rules) -> Vec<(&'a str, Option<HighlightId>)> {
        let whole = 0..text.len();
        highlight(text, &whole, &whole, rules)
            .into_iter()
            .map(|span| (&text[span.range], span.class))
            .collect()
    }

    #[test]
    fn colours_what_is_unmistakable_and_leaves_the_rest_plain() {
        type H = HighlightId;
        assert_eq!(
            parts(r#"let x = "a\"b" + 0x1F; // done"#, &C_LIKE),
            [
                ("let", Some(H::KEYWORD)),
                (" x = ", None),
                (r#""a\"b""#, Some(H::STRING)),
                (" + ", None),
                ("0x1F", Some(H::NUMBER)),
                ("; ", None),
                ("// done", Some(H::COMMENT)),
            ]
        );
    }

    #[test]
    fn block_comments_close_on_the_line_or_run_to_its_end() {
        type H = HighlightId;
        assert_eq!(
            parts("a /* b */ c /* open", &C_LIKE),
            [
                ("a ", None),
                ("/* b */", Some(H::COMMENT)),
                (" c ", None),
                ("/* open", Some(H::COMMENT)),
            ]
        );
    }

    #[test]
    fn words_numbers_and_quotes_are_not_taken_from_the_middle_of_text() {
        type H = HighlightId;
        // `x1` is a name, `don't` has an apostrophe, a lone quote stays plain.
        assert_eq!(
            parts("x1 don't say \"hi", &C_LIKE),
            [("x1 don't say \"hi", None)]
        );
        // Words that only contain a keyword are not keywords.
        assert_eq!(parts("format iffy", &C_LIKE), [("format iffy", None)]);
        assert_eq!(
            parts("1.5e3 v2 3rd", &C_LIKE),
            [
                ("1.5e3", Some(H::NUMBER)),
                (" v2 ", None),
                ("3rd", Some(H::NUMBER)),
            ]
        );
    }

    #[test]
    fn keywords_are_off_for_languages_that_ask_for_it() {
        let prose = Rules {
            keywords: false,
            ..C_LIKE
        };
        assert_eq!(
            parts("if true then 1", &prose),
            [("if true then ", None), ("1", Some(HighlightId::NUMBER))]
        );
        assert_eq!(parts("if true", &C_LIKE).len(), 3);
    }

    #[test]
    fn a_window_inside_a_line_is_tiled_exactly_and_keeps_context() {
        let text = "ab \"cd ef\" gh";
        let line = 0..text.len();
        // The window starts inside the string; the string is still a string.
        let spans = highlight(text, &line, &(5..11), &C_LIKE);
        assert_eq!(spans.first().unwrap().range.start, 5);
        assert_eq!(spans.last().unwrap().range.end, 11);
        assert_eq!(
            spans.iter().map(|s| s.range.len()).sum::<usize>(),
            6,
            "no gaps and no overlap"
        );
        assert_eq!(spans[0].class, Some(HighlightId::STRING));
        assert_eq!(spans[0].range, 5..10);
    }

    #[test]
    fn offsets_follow_the_line_and_multibyte_text_is_safe() {
        let text = "первая\nvar é = 'ж' // ü";
        let line = 13..text.len();
        let spans = highlight(text, &line, &line, &C_LIKE);
        let classes: Vec<_> = spans
            .iter()
            .map(|s| (&text[s.range.clone()], s.class))
            .collect();
        assert_eq!(classes[0], ("var", Some(HighlightId::KEYWORD)));
        assert_eq!(classes[2], ("'ж'", Some(HighlightId::STRING)));
        assert_eq!(classes[4], ("// ü", Some(HighlightId::COMMENT)));
    }

    #[test]
    fn very_long_lines_and_empty_windows_stay_plain() {
        let long = "1 ".repeat(MAX_LINE_BYTES);
        let all = 0..long.len();
        let spans = highlight(&long, &all, &all, &C_LIKE);
        assert_eq!(
            spans,
            [TokenSpan {
                range: all.clone(),
                class: None
            }]
        );
        assert!(highlight("x", &(0..1), &(0..0), &C_LIKE).is_empty());
    }

    #[test]
    fn without_declared_markers_nothing_is_a_comment() {
        let none = Rules {
            line_comment: None,
            block_comment: None,
            keywords: true,
        };
        assert_eq!(parts("// x # y", &none), [("// x # y", None)]);
    }
}
