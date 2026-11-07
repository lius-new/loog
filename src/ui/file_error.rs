//! Failed files keep their normal tabs, with the reason centered in the pane.
use lgui::core::{SemanticRole, Semantics, TextStyle, TextWrap, UiElement, UiId};
use lgui::prelude::{Element, TextAlign, UiRect, VisualStyle, scroll_view, text};
use lgui::text::{self, TextLayoutRequest};

use crate::ui::theme;

const TITLE_H: f32 = 24.0;
const DETAIL_GAP: f32 = 12.0;
const LINE_H: f32 = 20.0;
const TEXT_PAD: f32 = 12.0;

pub(crate) fn render(rect: UiRect, error: &str, surface_id: UiId) -> Element {
    let inset_x = 24.0_f32.min(rect.width().max(0.0) / 2.0);
    let inset_y = 16.0_f32.min(rect.height().max(0.0) / 2.0);
    let viewport = UiRect::new(
        rect.left + inset_x,
        rect.top + inset_y,
        rect.right - inset_x,
        rect.bottom - inset_y,
    );
    let width = viewport.width().clamp(0.0, 520.0);
    let center = (rect.left + rect.right) / 2.0;
    let mut title_style = theme::sans_semibold(theme::c().text_soft, 15.0);
    title_style.align = TextAlign::Center;
    let detail_style = theme::sans(theme::c().text_muted, theme::UI_SIZE);
    let lines = wrapped_lines(error, (width - TEXT_PAD).max(1.0), detail_style);
    let content_h = TITLE_H + DETAIL_GAP + lines.len() as f32 * LINE_H + 4.0;
    let top = viewport.top + ((viewport.height() - content_h) / 2.0).max(0.0);
    let key = format!("file-error:{}:{error}", surface_id.as_str());
    let page = Element::new(move |cx| {
        UiElement::panel(surface_id, rect, VisualStyle::filled(theme::c().bg))
            .semantics(Semantics::new(SemanticRole::Group).name("Unable to open file"))
            .children(cx.children)
    })
    .key(key);
    if viewport.width() <= 0.0 || viewport.height() <= 0.0 {
        return page;
    }
    let mut content = scroll_view(
        viewport,
        (viewport.width(), content_h.max(viewport.height())),
    )
    .vertical()
    .thumb_color(theme::c().scrollbar)
    .child(text(
        UiRect::new(
            center - width / 2.0,
            top,
            center + width / 2.0,
            top + TITLE_H,
        ),
        "Unable to open file",
        title_style,
    ));
    for (index, line) in lines.into_iter().enumerate() {
        let natural_width = crate::ui::tabs::measure_sans(&line, theme::UI_SIZE, 400);
        let left = center - natural_width / 2.0;
        let y = top + TITLE_H + DETAIL_GAP + index as f32 * LINE_H;
        // Wrap once, then draw each line in a wide single-line rect. Physical
        // font rounding must not cause a second wrap inside a tightly sized box.
        content = content.child(text(
            UiRect::new(left, y, left + natural_width + width + TEXT_PAD, y + LINE_H),
            line,
            detail_style,
        ));
    }
    page.child(content)
}

pub(crate) fn wrapped_lines(value: &str, width: f32, style: TextStyle) -> Vec<String> {
    let request = TextLayoutRequest::for_style(
        value,
        UiRect::new(0.0, 0.0, width, 10000.0),
        &style.wrap(TextWrap::Words),
    );
    if let Some(layout) = text::layout(&request) {
        let chars: Vec<char> = value.chars().collect();
        let lines: Option<Vec<String>> = layout
            .lines()
            .iter()
            .map(|line| {
                chars.get(line.range.clone()).map(|chars| {
                    chars
                        .iter()
                        .collect::<String>()
                        .trim_end_matches(['\r', '\n'])
                        .to_owned()
                })
            })
            .collect();
        if let Some(lines) = lines.filter(|lines| !lines.is_empty()) {
            return lines;
        }
    }
    // A renderer may not yet be installed on the first frame. Preserve every
    // character instead of estimating a paragraph height and hiding its tail.
    let mut lines = Vec::new();
    let mut line = String::new();
    for ch in value.chars() {
        if ch == '\n' {
            lines.push(std::mem::take(&mut line));
            continue;
        }
        line.push(ch);
        if line.len() > ch.len_utf8()
            && crate::ui::tabs::measure_sans(&line, style.height, style.weight) > width
        {
            line.pop();
            lines.push(std::mem::take(&mut line));
            line.push(ch);
        }
    }
    lines.push(line);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use lgui::application::{AppView, ApplicationContext};
    use lgui::core::UiScale;
    use lgui::session::UiSession;
    use std::sync::Arc;

    #[test]
    fn long_errors_keep_every_character_and_do_not_rewrap_when_scaled() {
        let error = r"Could not open \\?\C:\Users\yys\loog\.packaging\windows\stage\Loog-0.1.5-windows-x86_64\resources\中文文件目录\a-long-file-name-with-many-separators.exe: Binary file";
        let context = ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        for (width, height) in [(320.0, 110.0), (760.0, 480.0)] {
            let rect = UiRect::new(0.0, 0.0, width, height);
            let view: AppView =
                Arc::new(move |_| render(rect, error, UiId::new("error-page-test")));
            for factor in [0.75, 1.0, 1.25, 1.5, 2.0] {
                let scale = UiScale::new(factor);
                let mut session = UiSession::new();
                session.render_view(&view, rect, scale);
                let details: Vec<_> = session
                    .tree()
                    .nodes()
                    .iter()
                    .filter(|node| {
                        node.text
                            .as_deref()
                            .is_some_and(|value| value != "Unable to open file")
                    })
                    .collect();
                let restored: String = details
                    .iter()
                    .filter_map(|node| node.text.as_deref())
                    .collect();
                assert_eq!(
                    restored, error,
                    "the last line and error reason must remain present"
                );
                for node in details {
                    let mut style = node.text_style.unwrap();
                    let bounds = scale.physical_ui_rect(node.layout_rect);
                    style.height = scale.physical_ui_signed_length(style.height);
                    let request =
                        TextLayoutRequest::for_style(node.text.as_deref().unwrap(), bounds, &style);
                    let actual = text::layout(&request).unwrap();
                    assert!(
                        !actual.did_exceed_max_lines,
                        "a planned line must not wrap a second time"
                    );
                    assert!(
                        actual.height <= bounds.height() + 0.5,
                        "a line must fit its drawn height"
                    );
                }
                let scrolls = session
                    .tree()
                    .nodes()
                    .iter()
                    .any(|node| node.id.as_str().ends_with("thumb-y"));
                assert_eq!(
                    scrolls,
                    height == 110.0,
                    "small panes must let the full error scroll into view"
                );
            }
        }
    }
}
