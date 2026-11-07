//! Turn ready semantic ranges into current-theme spans. No lexing occurs here.
use crate::{
    features::{highlighting::HighlightId, syntax::TokenSpan},
    ui::theme,
};
use lgui::prelude::Color;
use loog_editor_core::text::TextBuffer;
use std::ops::Range;
type ColoredSpans = Vec<(String, Color)>;

pub(crate) fn highlight_window(
    buffer: &TextBuffer,
    window: Range<usize>,
    ready: Option<&[TokenSpan]>,
) -> ColoredSpans {
    if window.is_empty() {
        return Vec::new();
    }
    let Some(ready) = ready else {
        return vec![(buffer.text()[window].to_owned(), theme::c().text)];
    };
    let ceil = |offset| {
        let floor = buffer.grapheme_floor(offset);
        if floor == offset {
            offset
        } else {
            buffer.next_grapheme(floor)
        }
    };
    let mut result: ColoredSpans = Vec::new();
    let mut cursor = window.start;
    for span in ready {
        let from = ceil(span.range.start).max(cursor).min(window.end);
        let to = ceil(span.range.end).min(window.end);
        if cursor < from {
            result.push((buffer.text()[cursor..from].to_owned(), theme::c().text));
            cursor = from;
        }
        if from >= to {
            continue;
        }
        let color = color_for(span.class);
        let text = &buffer.text()[from..to];
        if let Some((previous, previous_color)) = result.last_mut()
            && *previous_color == color
        {
            previous.push_str(text);
        } else {
            result.push((text.to_owned(), color));
        }
        cursor = to;
    }
    if cursor < window.end {
        result.push((
            buffer.text()[cursor..window.end].to_owned(),
            theme::c().text,
        ));
    }
    result
}

fn color_for(class: Option<HighlightId>) -> Color {
    class.map_or(theme::c().text, theme::syntax_color)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::{highlighting::HighlightId, syntax::TokenSpan};

    #[test]
    fn pending_and_ready_spans_preserve_exact_text_and_combining_graphemes() {
        let buffer = TextBuffer::new("const e\u{301}=\"中👩‍💻\"; // comment");
        let window = 0..buffer.len();
        // The rendering contract consumes spans; parsing is verified separately.
        let string_start = buffer.text().find('"').unwrap();
        let string_end = buffer.text().rfind('"').unwrap() + 1;
        let comment = buffer.text().find("//").unwrap();
        let tokens = vec![
            TokenSpan {
                range: 0..5,
                class: Some(HighlightId::KEYWORD),
            },
            TokenSpan {
                range: 5..string_start,
                class: None,
            },
            TokenSpan {
                range: string_start..string_end,
                class: Some(HighlightId::STRING),
            },
            TokenSpan {
                range: string_end..comment,
                class: None,
            },
            TokenSpan {
                range: comment..buffer.len(),
                class: Some(HighlightId::COMMENT),
            },
        ];
        assert!(tokens.len() > 3, "the sample has several spans");
        for ready in [
            None,
            Some(tokens.as_slice()),
            Some(&[][..]),
            Some(&tokens[2..]),
        ] {
            let spans = highlight_window(&buffer, window.clone(), ready);
            assert_eq!(
                spans
                    .iter()
                    .map(|(text, _)| text.as_str())
                    .collect::<String>(),
                buffer.text()
            );
            let mut offset = 0;
            for (text, _) in spans {
                assert!(buffer.is_grapheme_boundary(offset));
                offset += text.len();
                assert!(buffer.is_grapheme_boundary(offset));
            }
        }
        let spans = highlight_window(&buffer, window, Some(&tokens));
        assert_eq!(spans[0].1, theme::syntax_color(HighlightId::KEYWORD));
        assert!(spans.iter().any(|(text, color)| text.contains("comment")
            && *color == theme::syntax_color(HighlightId::COMMENT)));
    }
}
