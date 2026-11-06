//! Screen projections and selection rules without a theme or runtime.
use crate::features::terminal::{TerminalCellStyle, TerminalRun};

#[derive(Clone, Copy, Debug)]
pub(crate) struct Palette {
    pub ansi: [u32; 16],
    pub foreground: u32,
    pub background: u32,
}
/// A cell boundary in coordinates that survive scrolling the view: row `0` is
/// the top of the live screen and negative rows are in the scrollback.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct GridPoint {
    pub(crate) row: i64,
    pub(crate) col: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Selection {
    pub(crate) anchor: GridPoint,
    pub(crate) head: GridPoint,
}

impl Selection {
    pub(crate) fn ordered(self) -> Option<(GridPoint, GridPoint)> {
        let (start, end) = if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        };
        (start != end).then_some((start, end))
    }
}

/// Characters that keep a double-click word together, so paths, flags and
/// URLs select whole.
const WORD_PUNCTUATION: &str = "_-./\\:~@%+=?&#";

fn char_class(cell: &str) -> u8 {
    let Some(c) = cell.chars().next() else {
        return 0;
    };
    if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() || WORD_PUNCTUATION.contains(c) {
        1
    } else {
        2
    }
}

/// Cell range `[start, end)` of the word, blank run or single symbol under
/// `col` in one row of cell contents.
pub(crate) fn word_range(cells: &[String], col: usize) -> (u16, u16) {
    if cells.is_empty() {
        return (0, 0);
    }
    let col = col.min(cells.len() - 1);
    let class = char_class(&cells[col]);
    if class == 2 {
        return (col as u16, col as u16 + 1);
    }
    let mut start = col;
    while start > 0 && char_class(&cells[start - 1]) == class {
        start -= 1;
    }
    let mut end = col + 1;
    while end < cells.len() && char_class(&cells[end]) == class {
        end += 1;
    }
    (start as u16, end as u16)
}

/// The `http(s)` link under `col` in one row of cell contents, without the
/// punctuation that usually trails a link in prose or logs.
pub(crate) fn url_at(cells: &[String], col: usize) -> Option<String> {
    url_span(cells, col).map(|(_, _, url)| url)
}

/// Like [`url_at`], with the link's cell range `[start, end)`.
pub(crate) fn url_span(cells: &[String], col: usize) -> Option<(u16, u16, String)> {
    let mut text = String::new();
    let mut starts = Vec::with_capacity(cells.len());
    for cell in cells {
        starts.push(text.len());
        text.push_str(if cell.is_empty() { " " } else { cell });
    }
    let at = *starts.get(col)?;
    if text[at..].chars().next().is_none_or(char::is_whitespace) {
        return None;
    }
    let begin = text[..at].rfind(char::is_whitespace).map_or(0, |i| {
        i + text[i..].chars().next().map_or(1, char::len_utf8)
    });
    let end = text[at..]
        .find(char::is_whitespace)
        .map_or(text.len(), |i| at + i);
    let token = &text[begin..end];
    let lower = token.to_ascii_lowercase();
    let scheme = ["https://", "http://"]
        .iter()
        .filter_map(|scheme| lower.find(scheme).map(|i| (i, scheme.len())))
        .min()?;
    if begin + scheme.0 > at {
        return None;
    }
    let mut url = &token[scheme.0..];
    loop {
        let Some(last) = url.chars().last() else {
            break;
        };
        let unbalanced = |open: char, close: char| {
            last == close && url.matches(open).count() < url.matches(close).count()
        };
        if ".,;:!?'\"".contains(last)
            || unbalanced('(', ')')
            || unbalanced('[', ']')
            || unbalanced('{', '}')
            || last == '>'
        {
            url = &url[..url.len() - last.len_utf8()];
        } else {
            break;
        }
    }
    if url.len() <= scheme.1 {
        return None;
    }
    let from = begin + scheme.0;
    let first = starts.partition_point(|&start| start < from);
    let last = starts.partition_point(|&start| start < from + url.len());
    Some((first as u16, last as u16, url.to_owned()))
}

/// End of the text in a row, so a line selection skips trailing blanks.
pub(crate) fn line_end(cells: &[String]) -> u16 {
    cells
        .iter()
        .rposition(|cell| char_class(cell) != 0)
        .map_or(0, |last| last as u16 + 1)
}

pub(crate) fn visible_selection(
    start: GridPoint,
    end: GridPoint,
    scrollback: usize,
    rows: u16,
    cols: u16,
) -> Vec<(u16, u16, u16)> {
    (0..rows)
        .filter_map(|row| {
            let grid_row = i64::from(row) - scrollback as i64;
            if grid_row < start.row || grid_row > end.row {
                return None;
            }
            let from = if grid_row == start.row { start.col } else { 0 };
            let to = if grid_row == end.row { end.col } else { cols };
            (from < to).then_some((row, from, to))
        })
        .collect()
}

/// Reads the selected text row by row, scrolling the parser's view as needed
/// so rows above the visible screen can still be copied.
pub(crate) fn selection_text(
    screen: &mut vt100::Screen,
    start: GridPoint,
    end: GridPoint,
) -> String {
    let original_scrollback = screen.scrollback();
    let (rows, cols) = screen.size();
    let mut text = String::new();
    for grid_row in start.row..=end.row {
        screen.set_scrollback(usize::try_from(-grid_row).unwrap_or(0));
        let Ok(row) = u16::try_from(grid_row + screen.scrollback() as i64) else {
            continue;
        };
        if row >= rows {
            continue;
        }
        let from = if grid_row == start.row { start.col } else { 0 };
        let to = if grid_row == end.row { end.col } else { cols };
        if from < to {
            let line = screen
                .rows(from, to - from)
                .nth(usize::from(row))
                .unwrap_or_default();
            text.push_str(line.trim_end_matches(' '));
        }
        if grid_row != end.row && !screen.row_wrapped(row) {
            text.push('\n');
        }
    }
    screen.set_scrollback(original_scrollback);
    text
}

/// Every case-insensitive occurrence of `query` in the scrollback and the live
/// screen, in reading order, as cell ranges ready to be selected. A match stays
/// on one row; text wrapped across rows is not joined.
pub(crate) fn find_matches(screen: &mut vt100::Screen, query: &str) -> Vec<(GridPoint, GridPoint)> {
    let needle = query
        .chars()
        .filter_map(|c| c.to_lowercase().next())
        .collect::<Vec<_>>();
    if needle.is_empty() {
        return Vec::new();
    }
    let original = screen.scrollback();
    let (rows, cols) = screen.size();
    let history = screen.scrollback_rows();
    // Read the buffer one screenful at a time; the last page may overlap the one
    // before it, so key the rows by their grid position.
    let mut lines = std::collections::BTreeMap::new();
    let mut offset = 0;
    loop {
        screen.set_scrollback(offset);
        let shown = screen.scrollback();
        for row in 0..rows {
            let grid_row = i64::from(row) - shown as i64;
            lines.entry(grid_row).or_insert_with(|| {
                let mut chars = Vec::new();
                let mut col = 0;
                while col < cols {
                    let Some(cell) = screen.cell(row, col) else {
                        break;
                    };
                    let width = if cell.is_wide() { 2 } else { 1 };
                    if !cell.is_wide_continuation() {
                        let text = if cell.has_contents() {
                            cell.contents()
                        } else {
                            " "
                        };
                        for c in text.chars() {
                            chars.push((c.to_lowercase().next().unwrap_or(c), col, width));
                        }
                    }
                    col += 1;
                }
                chars
            });
        }
        if offset >= history {
            break;
        }
        offset = (offset + usize::from(rows)).min(history);
    }
    screen.set_scrollback(original);
    let mut found = Vec::new();
    for (grid_row, chars) in &lines {
        let mut start = 0;
        while start + needle.len() <= chars.len() {
            if chars[start..start + needle.len()]
                .iter()
                .map(|(c, _, _)| c)
                .eq(needle.iter())
            {
                let (_, first, _) = chars[start];
                let (_, last, width) = chars[start + needle.len() - 1];
                found.push((
                    GridPoint {
                        row: *grid_row,
                        col: first,
                    },
                    GridPoint {
                        row: *grid_row,
                        col: last + width,
                    },
                ));
                // Matches never overlap, as in the shell's own search tools.
                start += needle.len();
            } else {
                start += 1;
            }
        }
    }
    found
}

pub(crate) fn line_runs(
    screen: &vt100::Screen,
    row: u16,
    cols: u16,
    palette: &Palette,
) -> Vec<TerminalRun> {
    let mut runs = Vec::new();
    let mut current: Option<TerminalRun> = None;

    let mut col = 0;
    while col < cols {
        let Some(cell) = screen.cell(row, col) else {
            col += 1;
            continue;
        };
        if cell.is_wide_continuation() {
            col += 1;
            continue;
        }
        let style = cell_style(cell, palette);
        let cell_text = if cell.has_contents() {
            cell.contents()
        } else {
            " "
        };
        let columns = if cell.is_wide() && col + 1 < cols {
            2
        } else {
            1
        };

        // Skia shapes a fallback CJK font by its own glyph advances, while a
        // terminal positions every wide cell on an exact two-column grid.
        // Keeping a wide cell out of neighboring text runs prevents paragraph
        // shaping from moving or clipping it when the two width models differ.
        if columns == 2 {
            if let Some(run) = current.take() {
                push_visible_run(&mut runs, run);
            }
            push_visible_run(
                &mut runs,
                TerminalRun {
                    start_col: col,
                    columns,
                    text: cell_text.to_owned(),
                    style,
                },
            );
            col += columns;
            continue;
        }

        match current.as_mut() {
            Some(run) if run.style == style => {
                run.columns += columns;
                run.text.push_str(cell_text);
            }
            Some(_) => {
                let run = current.take().expect("current terminal run exists");
                push_visible_run(&mut runs, run);
                current = Some(TerminalRun {
                    start_col: col,
                    columns,
                    text: cell_text.to_owned(),
                    style,
                });
            }
            None => {
                current = Some(TerminalRun {
                    start_col: col,
                    columns,
                    text: cell_text.to_owned(),
                    style,
                });
            }
        }
        col += columns;
    }
    if let Some(run) = current {
        push_visible_run(&mut runs, run);
    }
    runs
}

fn push_visible_run(runs: &mut Vec<TerminalRun>, mut run: TerminalRun) {
    if run.style.background.is_none() {
        let trimmed = run.text.trim_end_matches(' ');
        let removed = run.text.len() - trimmed.len();
        run.columns = run.columns.saturating_sub(removed as u16);
        run.text.truncate(trimmed.len());
    }
    if !run.text.is_empty() || run.style.background.is_some() {
        runs.push(run);
    }
}

fn cell_style(cell: &vt100::Cell, palette: &Palette) -> TerminalCellStyle {
    let mut foreground = terminal_color(cell.fgcolor(), palette);
    let mut background = terminal_color(cell.bgcolor(), palette);
    if cell.inverse() {
        std::mem::swap(&mut foreground, &mut background);
        if foreground.is_none() {
            foreground = Some(palette.background);
        }
        if background.is_none() {
            background = Some(palette.foreground);
        }
    }
    TerminalCellStyle {
        foreground,
        background,
        bold: cell.bold(),
        dim: cell.dim(),
        underline: cell.underline(),
    }
}

fn terminal_color(color: vt100::Color, palette: &Palette) -> Option<u32> {
    match color {
        vt100::Color::Default => None,
        vt100::Color::Rgb(red, green, blue) => {
            Some((u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue))
        }
        vt100::Color::Idx(index) => Some(indexed_color(index, palette)),
    }
}

fn indexed_color(index: u8, palette: &Palette) -> u32 {
    let ansi = &palette.ansi;
    match index {
        0..=15 => ansi[index as usize],
        16..=231 => {
            let index = index - 16;
            let red = index / 36;
            let green = (index % 36) / 6;
            let blue = index % 6;
            let channel = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
            (u32::from(channel(red)) << 16)
                | (u32::from(channel(green)) << 8)
                | u32::from(channel(blue))
        }
        232..=255 => {
            let value = 8 + (index - 232) * 10;
            (u32::from(value) << 16) | (u32::from(value) << 8) | u32::from(value)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn palette() -> Palette {
        let mut ansi = [0; 16];
        ansi[1] = 0xcd3131;
        Palette {
            ansi,
            foreground: 0xffffff,
            background: 0x101010,
        }
    }

    #[test]
    fn indexed_terminal_colors_include_the_ansi_and_rgb_cube_ranges() {
        assert_eq!(indexed_color(1, &palette()), 0xcd3131);
        assert_eq!(indexed_color(16, &palette()), 0x000000);
        assert_eq!(indexed_color(231, &palette()), 0xffffff);
        assert_eq!(indexed_color(255, &palette()), 0xeeeeee);
    }

    #[test]
    fn ansi_parser_produces_colored_runs() {
        let mut parser = vt100::Parser::new(2, 20, 0);
        parser.process(b"plain \x1b[31mred\x1b[0m");
        let runs = line_runs(parser.screen(), 0, 20, &palette());
        assert!(runs.iter().any(|run| run.text.contains("plain")));
        assert!(
            runs.iter()
                .any(|run| { run.text == "red" && run.style.foreground == Some(0xcd3131) })
        );
    }

    #[test]
    fn wide_terminal_cells_keep_their_two_column_positions() {
        let mut parser = vt100::Parser::new(2, 20, 0);
        parser.process("A中文B".as_bytes());

        let runs = line_runs(parser.screen(), 0, 20, &palette());
        let visible = runs
            .iter()
            .map(|run| (run.start_col, run.columns, run.text.as_str()))
            .collect::<Vec<_>>();

        assert_eq!(
            visible,
            vec![(0, 1, "A"), (1, 2, "中"), (3, 2, "文"), (5, 1, "B")]
        );
    }

    #[test]
    fn selection_text_spans_rows_and_joins_wrapped_lines() {
        let mut parser = vt100::Parser::new(3, 6, 0);
        parser.process(b"abcdefgh\r\nxyz   ");
        let text = selection_text(
            parser.screen_mut(),
            GridPoint { row: 0, col: 2 },
            GridPoint { row: 2, col: 2 },
        );
        assert_eq!(text, "cdefgh\nxy");
    }

    #[test]
    fn selection_text_reads_rows_in_scrollback() {
        let mut parser = vt100::Parser::new(2, 10, 10);
        parser.process(b"one\r\ntwo\r\nthree");
        let text = selection_text(
            parser.screen_mut(),
            GridPoint { row: -1, col: 0 },
            GridPoint { row: 0, col: 3 },
        );
        assert_eq!(text, "one\ntwo");
        assert_eq!(parser.screen().scrollback(), 0);
    }

    #[test]
    fn find_matches_reads_history_and_screen_ignoring_case_without_moving_the_view() {
        let mut parser = vt100::Parser::new(2, 20, 50);
        parser.process(b"Error: one\r\nok\r\nerror two ERROR\r\nfine\r\nlast ERROR");
        parser.screen_mut().set_scrollback(1);
        let found = find_matches(parser.screen_mut(), "error");
        assert_eq!(parser.screen().scrollback(), 1);
        let rows = |m: &(GridPoint, GridPoint)| (m.0.row, m.0.col, m.1.col);
        // The screen holds the last two lines; earlier lines are history.
        assert_eq!(
            found.iter().map(rows).collect::<Vec<_>>(),
            vec![(-3, 0, 5), (-1, 0, 5), (-1, 10, 15), (1, 5, 10)]
        );
        assert!(find_matches(parser.screen_mut(), "").is_empty());
        assert!(find_matches(parser.screen_mut(), "missing").is_empty());
        // The text of a match can be read back through the selection rules.
        let (start, end) = found[1];
        assert_eq!(selection_text(parser.screen_mut(), start, end), "error");
    }

    #[test]
    fn find_matches_uses_cell_columns_for_wide_characters_and_never_overlaps() {
        let mut parser = vt100::Parser::new(2, 20, 0);
        parser.process("中文 aaaa".as_bytes());
        let found = find_matches(parser.screen_mut(), "文 a");
        // "中" and "文" take two columns each, so "文" starts at column 2 and
        // the match ends after the "a" at column 5.
        assert_eq!(
            found,
            vec![(GridPoint { row: 0, col: 2 }, GridPoint { row: 0, col: 6 })]
        );
        let found = find_matches(parser.screen_mut(), "aa");
        assert_eq!(found.len(), 2);
        assert_eq!((found[0].0.col, found[0].1.col), (5, 7));
        assert_eq!((found[1].0.col, found[1].1.col), (7, 9));
    }

    #[test]
    fn word_range_keeps_paths_together_and_splits_on_blanks() {
        let cells: Vec<String> = "cd src/app  x(y)".chars().map(|c| c.to_string()).collect();
        assert_eq!(word_range(&cells, 4), (3, 10));
        assert_eq!(word_range(&cells, 10), (10, 12));
        assert_eq!(word_range(&cells, 13), (13, 14));
        assert_eq!(line_end(&cells), 16);
    }

    #[test]
    fn url_at_finds_links_and_drops_trailing_punctuation() {
        let cells = |text: &str| text.chars().map(|c| c.to_string()).collect::<Vec<_>>();
        let row = cells("see (https://a.io/x_(1)).  ok http://b.c, end");
        assert_eq!(url_at(&row, 10).as_deref(), Some("https://a.io/x_(1)"));
        assert_eq!(url_at(&row, 4), None);
        assert_eq!(url_at(&row, 3), None);
        assert_eq!(url_at(&row, 32).as_deref(), Some("http://b.c"));
        assert_eq!(url_at(&cells("http://"), 2), None);
        assert_eq!(
            url_span(&row, 10),
            Some((5, 23, "https://a.io/x_(1)".into()))
        );
    }

    #[test]
    fn visible_selection_follows_the_scrolled_view() {
        let start = GridPoint { row: -1, col: 2 };
        let end = GridPoint { row: 0, col: 3 };
        assert_eq!(visible_selection(start, end, 0, 2, 10), vec![(0, 0, 3)]);
        assert_eq!(
            visible_selection(start, end, 1, 2, 10),
            vec![(0, 2, 10), (1, 0, 3)]
        );
    }

    #[test]
    fn caller_palette_controls_ansi_and_inverse_default_colors() {
        let mut parser = vt100::Parser::new(2, 20, 0);
        parser.process(b"\x1b[31mA\x1b[0m\x1b[7mB");
        let mut selected = palette();
        selected.ansi[1] = 0x123456;
        selected.foreground = 0xabcdef;
        selected.background = 0x654321;
        let runs = line_runs(parser.screen(), 0, 20, &selected);
        assert_eq!(runs[0].style.foreground, Some(0x123456));
        assert_eq!(runs[1].style.foreground, Some(0x654321));
        assert_eq!(runs[1].style.background, Some(0xabcdef));
        assert_eq!(
            line_runs(parser.screen(), 0, 20, &palette())[0]
                .style
                .foreground,
            Some(0xcd3131)
        );
    }
}
