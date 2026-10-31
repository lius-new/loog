//! The scrolling body of the detail pane: the readme, what the extension adds,
//! its changelog and runtime messages, and the facts beside them.
use lgui::core::{Color, TextStyle};
use lgui::prelude::{Element, TextAlign, UiRect, VisualStyle, panel, scroll_view};

use super::widgets::{self, Ids, Kind, Tone, blend, line, paragraph};
use crate::ui::extensions_binding::{
    Action, Binding, Block, Command, Contribution, DetailTab, Entry, LogLevel, Notice, Table,
    ThemeCard,
};
use crate::ui::tabs::{ellipsize, ellipsize_sans, icon, measure, measure_sans};
use crate::ui::theme;

const PAD_X: f32 = 32.0;
const PAD_TOP: f32 = 24.0;
const MAIN_MAX_W: f32 = 720.0;
const SIDE_W: f32 = 220.0;
const SIDE_GAP: f32 = 36.0;
/// The narrowest content width that still has room for the side column.
const SIDE_MIN_INNER: f32 = 640.0;
const TEXT_H: f32 = 19.0;
const CODE_H: f32 = 17.0;
const ROW_H: f32 = 28.0;
const PREVIEW_H: f32 = 92.0;
const THEME_NAME_H: f32 = 30.0;
const THEME_GAP: f32 = 12.0;
/// Column starts and widths of a contribution table, as fractions of its width.
const COLUMNS: [(f32, f32); 3] = [(0.0, 0.34), (0.34, 0.40), (0.74, 0.26)];

pub(super) struct Body<'a> {
    pub entry: &'a Entry,
    /// Shown from the marketplace rather than from the installed list.
    pub market: bool,
    pub tab: DetailTab,
    pub state: &'a Binding,
}

/// Content placed down a column, from the region's top-left corner.
struct Flow {
    x: f32,
    width: f32,
    start: f32,
    y: f32,
    items: Vec<Element>,
}

impl Flow {
    fn new(x: f32, y: f32, width: f32) -> Self {
        Self {
            x,
            width,
            start: y,
            y,
            items: Vec::new(),
        }
    }

    fn push(&mut self, element: Element) {
        self.items.push(element);
    }

    fn rect(&self, left: f32, height: f32) -> UiRect {
        UiRect::new(left, self.y, self.x + self.width, self.y + height)
    }

    fn heading(&mut self, value: &str) {
        if self.y > self.start {
            self.y += 16.0;
        }
        let rect = self.rect(self.x, 24.0);
        self.push(line(
            rect,
            value.to_owned(),
            theme::sans_semibold(theme::c().text_bright, 15.0),
        ));
        self.y += 30.0;
    }

    fn paragraph(&mut self, value: &str, style: TextStyle) {
        let (elements, height) = paragraph(self.x, self.y, self.width, value, style, TEXT_H);
        self.items.extend(elements);
        self.y += height + 8.0;
    }

    fn bullets(&mut self, values: &[String]) {
        let style = theme::sans(theme::c().text_muted, theme::UI_SIZE);
        for value in values {
            let dot = self.rect(self.x + 4.0, TEXT_H);
            self.push(line(dot, "•", style));
            let (elements, height) = paragraph(
                self.x + 18.0,
                self.y,
                self.width - 18.0,
                value,
                style,
                TEXT_H,
            );
            self.items.extend(elements);
            self.y += height + 4.0;
        }
        self.y += 6.0;
    }

    fn code(&mut self, lines: &[String]) {
        let c = theme::c();
        let height = lines.len() as f32 * CODE_H + 22.0;
        let rect = UiRect::new(self.x, self.y, self.x + self.width, self.y + height);
        self.push(theme::bordered(rect, c.sidebar, c.border, 6.0, 1.0));
        let style = theme::mono(c.text, theme::SMALL + 1.0);
        for (index, text) in lines.iter().enumerate() {
            let top = self.y + 11.0 + index as f32 * CODE_H;
            self.push(line(
                UiRect::new(self.x + 14.0, top, self.x + self.width - 14.0, top + CODE_H),
                ellipsize(text, self.width - 28.0, theme::SMALL + 1.0, 400),
                style,
            ));
        }
        self.y += height + 12.0;
    }

    /// A message box, with an optional action under its text.
    fn notice(
        &mut self,
        ids: &mut Ids,
        tone: Tone,
        icon_key: &'static str,
        notice: &Notice,
        action: Option<(&str, Command, bool)>,
        key: &str,
        state: &Binding,
    ) {
        let c = theme::c();
        let (fill, border, ink) = match tone {
            Tone::Bad => (
                blend(c.bg, c.error, 0.06),
                blend(c.border, c.error, 0.35),
                c.error,
            ),
            _ => (c.surface, c.border, c.text_dim),
        };
        let text_x = self.x + 14.0 + 16.0 + 12.0;
        let text_w = self.width - (text_x - self.x) - 14.0;
        let body_style = theme::sans(c.text_muted, theme::UI_SIZE);
        let (body, body_h) = paragraph(
            text_x,
            self.y + 14.0 + 22.0,
            text_w,
            &notice.body,
            body_style,
            TEXT_H,
        );
        let action_h = if action.is_some() { 36.0 } else { 0.0 };
        let height = 14.0 + 22.0 + body_h + action_h + 12.0;
        let rect = UiRect::new(self.x, self.y, self.x + self.width, self.y + height);
        self.push(theme::bordered(rect, fill, border, 8.0, 1.0));
        self.push(icon(
            ids.next(),
            icon_key,
            UiRect::new(self.x + 14.0, self.y + 14.0, self.x + 30.0, self.y + 30.0),
            ink,
        ));
        self.push(line(
            UiRect::new(
                text_x,
                self.y + 12.0,
                self.x + self.width - 14.0,
                self.y + 34.0,
            ),
            notice.title.clone(),
            theme::sans_semibold(c.text_bright, theme::UI_SIZE + 1.0),
        ));
        self.items.extend(body);
        if let Some((label, command, primary)) = action {
            let top = self.y + height - 12.0 - 26.0;
            let run = state.clone();
            let key = format!("ext.notice.{key}");
            let width = widgets::button_width(label, false, theme::UI_SIZE);
            let button = widgets::button(
                ids,
                key,
                UiRect::new(text_x, top, text_x + width, top + 26.0),
                label,
                None,
                if primary { Kind::Primary } else { Kind::Plain },
                move || run.run(command.clone()),
            );
            self.push(button);
        }
        self.y += height + 16.0;
    }

    fn caption(&mut self, ids: &mut Ids, icon_key: &'static str, title: &str, total: usize) {
        let c = theme::c();
        self.y += if self.y > self.start { 14.0 } else { 0.0 };
        self.push(icon(
            ids.next(),
            icon_key,
            UiRect::new(self.x, self.y + 4.0, self.x + 16.0, self.y + 20.0),
            c.accent,
        ));
        let title_w = measure_sans(title, 13.0, 600) + 6.0;
        self.push(line(
            UiRect::new(
                self.x + 24.0,
                self.y,
                self.x + 24.0 + title_w,
                self.y + 24.0,
            ),
            title.to_owned(),
            theme::sans_semibold(c.text_bright, 13.0),
        ));
        self.push(line(
            UiRect::new(
                self.x + 24.0 + title_w + 6.0,
                self.y,
                self.x + self.width,
                self.y + 24.0,
            ),
            total.to_string(),
            theme::mono(c.text_dim, theme::SMALL),
        ));
        self.y += 30.0;
    }

    fn table(&mut self, ids: &mut Ids, table: &Table) {
        let c = theme::c();
        self.caption(ids, table.icon, table.title, table.total);
        let cell = |top: f32, column: usize| {
            let (start, span) = COLUMNS[column];
            UiRect::new(
                self.x + self.width * start,
                top,
                self.x + self.width * (start + span) - 10.0,
                top + ROW_H,
            )
        };
        let heading = theme::mono(c.text_dim, theme::SMALL).tracking(0.8);
        let mut cells = Vec::new();
        for (column, title) in table.head.iter().enumerate() {
            cells.push(line(cell(self.y, column), title.to_uppercase(), heading));
        }
        let rule_at =
            |top: f32| widgets::rule(UiRect::new(self.x, top - 1.0, self.x + self.width, top));
        cells.push(rule_at(self.y + ROW_H));
        let mut top = self.y + ROW_H;
        for row in &table.rows {
            for (column, value) in row.iter().enumerate() {
                let rect = cell(top, column);
                let first = column == 0;
                let size = if table.mono[column] {
                    theme::SMALL + 1.0
                } else {
                    theme::UI_SIZE
                };
                let shown = if table.mono[column] {
                    ellipsize(value, rect.width(), size, 400)
                } else {
                    ellipsize_sans(value, rect.width(), size, 400)
                };
                let ink = if first { c.text } else { c.text_muted };
                let style = if table.mono[column] {
                    theme::mono(ink, size)
                } else {
                    theme::sans(ink, size)
                };
                cells.push(line(rect, shown, style));
            }
            cells.push(rule_at(top + ROW_H));
            top += ROW_H;
        }
        self.items.extend(cells);
        self.y = top;
        if table.more > 0 {
            let rect = self.rect(self.x, 24.0);
            self.push(line(
                rect,
                format!("and {} more", table.more),
                theme::sans(c.text_dim, theme::UI_SIZE),
            ));
            self.y += 24.0;
        }
        self.y += 6.0;
    }

    fn themes(&mut self, ids: &mut Ids, themes: &[ThemeCard]) {
        self.caption(ids, "extension-theme", "Color themes", themes.len());
        let card_w = (self.width - THEME_GAP) / 2.0;
        for (index, theme_card) in themes.iter().enumerate() {
            let column = (index % 2) as f32;
            if index > 0 && index % 2 == 0 {
                self.y += PREVIEW_H + THEME_NAME_H + THEME_GAP;
            }
            let left = self.x + column * (card_w + THEME_GAP);
            let card = UiRect::new(
                left,
                self.y,
                left + card_w,
                self.y + PREVIEW_H + THEME_NAME_H,
            );
            let items = theme_card_elements(ids, card, theme_card);
            self.items.extend(items);
        }
        self.y += PREVIEW_H + THEME_NAME_H + THEME_GAP + 6.0;
    }

    fn release(&mut self, version: &str, date: &str, notes: &str) {
        let c = theme::c();
        let x = self.x + 16.0;
        let (body, body_h) = paragraph(
            x,
            self.y + 24.0,
            self.width - 16.0,
            notes,
            theme::sans(c.text_muted, theme::UI_SIZE),
            TEXT_H,
        );
        let height = 24.0 + body_h + 2.0;
        self.push(widgets::rule(UiRect::new(
            self.x,
            self.y + 2.0,
            self.x + 2.0,
            self.y + height,
        )));
        let version_w = measure_sans(version, theme::UI_SIZE + 1.0, 600) + 8.0;
        self.push(line(
            UiRect::new(x, self.y, x + version_w, self.y + 22.0),
            version.to_owned(),
            theme::sans_semibold(c.text_bright, theme::UI_SIZE + 1.0),
        ));
        self.push(line(
            UiRect::new(
                x + version_w + 6.0,
                self.y,
                self.x + self.width,
                self.y + 22.0,
            ),
            date.to_owned(),
            theme::mono(c.text_dim, theme::SMALL),
        ));
        self.items.extend(body);
        self.y += height + 16.0;
    }

    fn log(&mut self, lines: &[(LogLevel, String)]) {
        let c = theme::c();
        let size = theme::SMALL + 1.0;
        let per_line = ((self.width - 32.0) / mono_width(size)).floor().max(8.0) as usize;
        let rows: Vec<(LogLevel, String)> = lines
            .iter()
            .flat_map(|(level, text)| {
                chunks(text, per_line)
                    .into_iter()
                    .map(move |part| (*level, part))
            })
            .collect();
        let height = rows.len().max(1) as f32 * 19.0 + 28.0;
        let rect = UiRect::new(self.x, self.y, self.x + self.width, self.y + height);
        self.push(theme::bordered(rect, c.sidebar, c.border, 6.0, 1.0));
        for (index, (level, text)) in rows.into_iter().enumerate() {
            let top = self.y + 14.0 + index as f32 * 19.0;
            let ink = match level {
                LogLevel::Info => c.text_muted,
                LogLevel::Warning => c.warning,
                LogLevel::Error => c.error,
            };
            self.push(line(
                UiRect::new(self.x + 16.0, top, self.x + self.width - 8.0, top + 19.0),
                text,
                theme::mono(ink, size),
            ));
        }
        self.y += height + 12.0;
    }
}

/// The advance of one character of the small monospace face.
fn mono_width(size: f32) -> f32 {
    measure("0", size, 400).max(size * 0.5)
}

/// `value` cut into pieces of at most `per_line` characters.
fn chunks(value: &str, per_line: usize) -> Vec<String> {
    let chars: Vec<char> = value.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    chars
        .chunks(per_line.max(1))
        .map(|part| part.iter().collect())
        .collect()
}

fn theme_card_elements(ids: &mut Ids, card: UiRect, theme_card: &ThemeCard) -> Vec<Element> {
    let c = theme::c();
    let mut items = vec![theme::bordered(card, c.surface, c.border, 8.0, 1.0)];
    let preview = UiRect::new(
        card.left + 1.0,
        card.top + 1.0,
        card.right - 1.0,
        card.top + PREVIEW_H,
    );
    let name_y = card.top + PREVIEW_H;
    if let Some(reason) = &theme_card.rejected {
        let fill = blend(c.surface, c.error, 0.05);
        items.push(panel(preview, VisualStyle::filled(fill).radius(7.0)).into());
        items.push(
            panel(
                UiRect::new(
                    preview.left,
                    preview.bottom - 8.0,
                    preview.right,
                    preview.bottom,
                ),
                VisualStyle::filled(fill),
            )
            .into(),
        );
        let middle = (preview.left + preview.right) / 2.0;
        items.push(icon(
            ids.next(),
            "warning",
            UiRect::new(
                middle - 8.0,
                preview.top + 14.0,
                middle + 8.0,
                preview.top + 30.0,
            ),
            c.error,
        ));
        let style = widgets::aligned(theme::sans(c.error, theme::UI_SIZE), TextAlign::Center);
        let (lines, _) = paragraph(
            preview.left + 14.0,
            preview.top + 36.0,
            preview.width() - 28.0,
            reason,
            style,
            17.0,
        );
        items.extend(lines);
        items.push(line(
            UiRect::new(
                card.left + 14.0,
                name_y,
                card.right - 14.0,
                name_y + THEME_NAME_H,
            ),
            theme_card.name.clone(),
            theme::sans(c.text, theme::UI_SIZE),
        ));
        items.push(line(
            UiRect::new(
                card.left + 14.0,
                name_y,
                card.right - 14.0,
                name_y + THEME_NAME_H,
            ),
            "not loaded".to_owned(),
            widgets::aligned(theme::sans(c.text_dim, theme::SMALL), TextAlign::Right),
        ));
        return items;
    }

    let background = Color(theme_card.background);
    items.push(panel(preview, VisualStyle::filled(background).radius(7.0)).into());
    items.push(
        panel(
            UiRect::new(
                preview.left,
                preview.bottom - 8.0,
                preview.right,
                preview.bottom,
            ),
            VisualStyle::filled(background),
        )
        .into(),
    );
    let size = theme::SMALL + 1.0;
    let char_w = mono_width(size);
    let sample: [&[(&str, u32)]; 5] = [
        &[
            ("fn ", theme_card.keyword),
            ("greet", theme_card.function),
            ("(name) {", theme_card.foreground),
        ],
        &[("  // say hello", theme_card.comment)],
        &[
            ("  let ", theme_card.keyword),
            ("n = ", theme_card.foreground),
            ("42", theme_card.number),
            (";", theme_card.foreground),
        ],
        &[("  \"Hello\"", theme_card.string)],
        &[("}", theme_card.foreground)],
    ];
    for (row, spans) in sample.iter().enumerate() {
        let top = preview.top + 9.0 + row as f32 * 15.0;
        let mut left = preview.left + 14.0;
        for (word, color) in spans.iter() {
            let width = word.chars().count() as f32 * char_w;
            items.push(line(
                UiRect::new(left, top, left + width + 8.0, top + 15.0),
                (*word).to_owned(),
                theme::mono(Color(*color), size),
            ));
            left += width;
        }
    }
    items.push(line(
        UiRect::new(
            card.left + 14.0,
            name_y,
            card.right - 14.0,
            name_y + THEME_NAME_H,
        ),
        theme_card.name.clone(),
        theme::sans(c.text, theme::UI_SIZE),
    ));
    let swatches = [
        theme_card.background,
        theme_card.keyword,
        theme_card.function,
        theme_card.string,
    ];
    let middle = name_y + THEME_NAME_H / 2.0;
    for (index, color) in swatches.iter().enumerate() {
        let right = card.right - 14.0 - (swatches.len() - 1 - index) as f32 * 14.0;
        items.push(theme::bordered(
            UiRect::new(right - 10.0, middle - 5.0, right, middle + 5.0),
            Color(*color),
            blend(c.surface, c.text, 0.25),
            5.0,
            1.0,
        ));
    }
    items
}

/// The body of the detail pane, scrolling inside `region`.
pub(super) fn render(region: UiRect, ids: &mut Ids, body: Body<'_>) -> Element {
    let c = theme::c();
    let entry = body.entry;
    let inner = (region.width() - PAD_X * 2.0).max(120.0);
    let side = inner >= SIDE_MIN_INNER;
    let main_w = if side {
        (inner - SIDE_W - SIDE_GAP).min(MAIN_MAX_W)
    } else {
        inner.min(MAIN_MAX_W)
    };
    let left = region.left + PAD_X;
    let mut main = Flow::new(left, region.top + PAD_TOP, main_w);

    // Notices lead the first two tabs.
    if matches!(body.tab, DetailTab::Details | DetailTab::Contributions) {
        notices(&mut main, ids, &body);
    }
    if let Some(document) = body.state.documentation(entry, body.tab) {
        let top = main.y.min(region.bottom - 1.0);
        let height = (region.bottom - top - 16.0).max(1.0);
        let layout = crate::ui::markdown::card::Layout {
            width: main_w,
            height,
            measured: true,
            document,
        };
        let mut root = panel(region, VisualStyle::default())
            .children(main.items)
            .child(crate::ui::markdown::card::render(
                &layout, left, top, height, None,
            ));
        if side {
            let x = left + main_w + SIDE_GAP;
            let mut aside = Flow::new(x, region.top + PAD_TOP, SIDE_W);
            facts(&mut aside, entry, body.market);
            root = root.child(
                scroll_view(
                    UiRect::new(x, region.top, region.right, region.bottom),
                    (region.right - x, aside.y - region.top + 40.0),
                )
                .vertical()
                .thumb_color(c.scrollbar)
                .children(aside.items),
            );
        }
        return root;
    }
    let muted = theme::sans(c.text_muted, theme::UI_SIZE);
    match body.tab {
        DetailTab::Details => {
            for block in &entry.readme {
                match block {
                    Block::Heading(value) => main.heading(value),
                    Block::Paragraph(value) => main.paragraph(value, muted),
                    Block::Bullets(values) => main.bullets(values),
                    Block::Code(lines) => main.code(lines),
                }
            }
        }
        DetailTab::Contributions => {
            if entry.contributions.is_empty() {
                main.paragraph("This extension adds nothing yet.", muted);
            }
            for contribution in &entry.contributions {
                match contribution {
                    Contribution::Table(table) => main.table(ids, table),
                    Contribution::Themes(themes) => main.themes(ids, themes),
                }
            }
        }
        DetailTab::Changelog => {
            for release in &entry.changelog {
                main.release(&release.version, &release.date, &release.notes);
            }
        }
        DetailTab::Runtime => {
            if let Some(installed) = &entry.installed {
                main.heading("Runtime");
                main.paragraph(&installed.runtime.summary, muted);
                main.log(&installed.runtime.log);
                main.paragraph(
                    "Recent messages from the extension. Document text is never recorded.",
                    theme::sans(c.text_dim, theme::UI_SIZE),
                );
            }
        }
    }

    let mut height = main.y;
    let mut items = std::mem::take(&mut main.items);
    if side {
        let mut aside = Flow::new(left + main_w + SIDE_GAP, region.top + PAD_TOP, SIDE_W);
        facts(&mut aside, entry, body.market);
        height = height.max(aside.y);
        items.append(&mut aside.items);
    }
    let content_h = height - region.top + 40.0;
    let key = format!("ext.body.{}.{:?}.{}", entry.id, body.tab, body.market);
    Element::from(
        scroll_view(region, (region.width(), content_h))
            .vertical()
            .thumb_color(c.scrollbar)
            .children(items),
    )
    .key(key)
}

fn notices(main: &mut Flow, ids: &mut Ids, body: &Body<'_>) {
    let Some(installed) = &body.entry.installed else {
        return;
    };
    let id = body.entry.id.as_str();
    if body.market {
        return;
    }
    if let Some(update) = &installed.update {
        let notice = Notice {
            title: format!("Version {} is available", update.version),
            body: update.notes.clone(),
        };
        let label = format!("Update to {}", update.version);
        main.notice(
            ids,
            Tone::Quiet,
            "update-available",
            &notice,
            Some((&label, Command::Act(Action::Update, id.to_owned()), true)),
            &format!("update.{id}"),
            body.state,
        );
    }
    if let (true, Some(error)) = (installed.enabled, &installed.error) {
        main.notice(
            ids,
            Tone::Bad,
            "warning",
            error,
            Some((
                "See what happened →",
                Command::SetTab(DetailTab::Runtime),
                false,
            )),
            &format!("error.{id}"),
            body.state,
        );
    }
    if let (true, Some(warning)) = (installed.enabled, &installed.warning) {
        main.notice(
            ids,
            Tone::Quiet,
            "warning",
            warning,
            None,
            &format!("warning.{id}"),
            body.state,
        );
    }
    if !installed.enabled {
        let notice = Notice {
            title: "This extension is disabled.".to_owned(),
            body: "Nothing it adds is active. Enable it to use it again.".to_owned(),
        };
        main.notice(
            ids,
            Tone::Quiet,
            "warning",
            &notice,
            None,
            &format!("disabled.{id}"),
            body.state,
        );
    }
}

/// The facts column: identity, location and categories.
fn facts(aside: &mut Flow, entry: &Entry, market: bool) {
    let c = theme::c();
    let heading = theme::mono(c.text_dim, theme::SMALL).tracking(0.8);
    let rect = aside.rect(aside.x, 18.0);
    aside.push(line(rect, "MORE INFORMATION", heading));
    aside.y += 26.0;

    let mut rows: Vec<(&str, String, bool)> = vec![
        ("Identifier", entry.id.clone(), true),
        ("Version", entry.version.clone(), false),
        ("Publisher", entry.publisher.clone(), false),
        ("Updated", entry.updated.clone(), false),
    ];
    if market {
        if let Some(info) = &entry.market {
            rows.push(("Downloads", info.downloads.clone(), false));
            rows.push(("Rating", format!("★ {}", info.rating), false));
            rows.push(("License", info.license.clone(), false));
        }
    } else if let Some(installed) = &entry.installed {
        rows.push(("Location", installed.location.clone(), true));
    }
    let label_w = 76.0;
    let value_w = aside.width - label_w - 8.0;
    for (label, value, mono) in rows {
        aside.push(line(
            UiRect::new(aside.x, aside.y, aside.x + label_w, aside.y + 18.0),
            label.to_owned(),
            theme::sans(c.text_dim, theme::UI_SIZE),
        ));
        let size = if mono {
            theme::SMALL + 1.0
        } else {
            theme::UI_SIZE
        };
        let per_line = ((value_w / mono_width(size)).floor() as usize).max(6);
        let parts = if mono {
            chunks(&value, per_line)
        } else {
            vec![value]
        };
        let count = parts.len();
        for (index, part) in parts.into_iter().enumerate() {
            let top = aside.y + index as f32 * 17.0;
            let style = if mono {
                theme::mono(c.text_muted, size)
            } else {
                theme::sans(c.text_muted, size)
            };
            aside.push(line(
                UiRect::new(aside.x + label_w, top, aside.x + aside.width, top + 18.0),
                part,
                style,
            ));
        }
        aside.y += (count as f32 * 17.0).max(18.0) + 6.0;
    }

    aside.y += 8.0;
    aside.push(widgets::rule(UiRect::new(
        aside.x,
        aside.y,
        aside.x + aside.width,
        aside.y + 1.0,
    )));
    aside.y += 20.0;
    let rect = aside.rect(aside.x, 18.0);
    aside.push(line(rect, "CATEGORIES", heading));
    aside.y += 26.0;
    let style = theme::sans(c.text_muted, theme::SMALL + 1.0);
    let mut left = aside.x;
    for category in &entry.categories {
        let width = measure_sans(category, theme::SMALL + 1.0, 400) + 20.0;
        if left > aside.x && left + width > aside.x + aside.width {
            left = aside.x;
            aside.y += 26.0;
        }
        aside.push(widgets::chip(
            UiRect::new(left, aside.y, left + width, aside.y + 20.0),
            category,
            style,
        ));
        left += width + 6.0;
    }
    aside.y += 26.0;
}
