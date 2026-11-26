//! Hover: after the pointer rests on a word, ask the servers about it and show
//! what they say in a card beside the word.
use super::{AppContext, AppState, Target, services, target};
use crate::app::ui_timer::{self, Timer};
use crate::domain::document::DocumentStamp;
use crate::domain::identity::{PaneId, ViewId};
use crate::features::language_service::reply;
use crate::features::language_service::request::Feature;
use crate::features::markdown::content::Markup;
use crate::features::providers::{RequestHandle, RequestSpec};
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The word the pointer rests on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rest {
    pub pane: PaneId,
    pub view: ViewId,
    pub input_generation: u64,
    pub word: Range<usize>,
    pub stamp: DocumentStamp,
}

#[derive(Clone)]
struct Pending {
    rest: Rest,
    handle: Arc<RequestHandle>,
    /// The text the request was made for, to read the servers' ranges in.
    text: Arc<String>,
}

/// What the servers said, ready to show.
#[derive(Clone)]
pub struct Card {
    pub rest: Rest,
    /// The text the card describes (the server's range, else the word).
    pub anchor: Range<usize>,
    /// The text at `anchor`, the card's title.
    pub name: String,
    pub kind: Option<(&'static str, &'static str)>,
    /// The servers that answered, for the footer.
    pub server: String,
    pub documentation: Markup,
}

#[derive(Clone, Default)]
pub struct Hover {
    rest: Option<Rest>,
    pending: Option<Pending>,
    timer: Option<Arc<Timer>>,
    /// When the pointer last moved over the word; it has only rested once the
    /// delay has passed since.
    moved_at: Option<Instant>,
    pub card: Option<Card>,
    /// The text the person selected in the card, for Copy.
    pub selection: Option<String>,
    /// A press that began on the card is still held.
    pub pressing: bool,
    view: Option<SourceViewSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceViewSnapshot {
    view: ViewId,
    input_generation: u64,
    stamp: DocumentStamp,
    cursor: usize,
    scroll: (u32, u32),
}

pub fn observe(app: &mut AppState) {
    if app.editor.language.hover.is_idle() && app.editor.editor_diagnostic_hover.is_none() {
        app.editor.language.hover.view = None;
        return;
    }
    let pane = app.workspace.active_pane();
    let now = target(app, pane).map(|t| {
        let (x, y) = app.workspace.scroll(pane);
        SourceViewSnapshot {
            view: t.view,
            input_generation: t.input_generation,
            stamp: t.stamp,
            cursor: t.cursor,
            scroll: (x.to_bits(), y.to_bits()),
        }
    });
    if app.editor.language.hover.view.is_some() && app.editor.language.hover.view != now {
        clear(app);
        app.editor.editor_diagnostic_hover = None;
    } else {
        app.editor.language.hover.view = now;
    }
}

impl Hover {
    /// The word the pointer rests on, once it has moved onto one.
    pub fn rest(&self) -> Option<&Rest> {
        self.rest.as_ref()
    }

    pub fn is_idle(&self) -> bool {
        self.rest.is_none() && self.pending.is_none() && self.card.is_none()
    }

    /// The card to show in `pane` for the document in this state.
    pub fn card_for(&self, pane: PaneId, stamp: &DocumentStamp) -> Option<&Card> {
        self.card
            .as_ref()
            .filter(|card| card.rest.pane == pane && card.rest.stamp == *stamp)
    }
}

/// The identifier around `offset`, if the offset touches one.
pub fn word_at(text: &str, offset: usize) -> Option<Range<usize>> {
    if offset > text.len() || !text.is_char_boundary(offset) {
        return None;
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let start = offset
        - text[..offset]
            .chars()
            .rev()
            .take_while(|c| is_word(*c))
            .map(char::len_utf8)
            .sum::<usize>();
    let end = offset
        + text[offset..]
            .chars()
            .take_while(|c| is_word(*c))
            .map(char::len_utf8)
            .sum::<usize>();
    (start < end).then_some(start..end)
}

/// Forgets everything: the card, a request in flight, a timer.
pub fn clear(app: &mut AppState) -> bool {
    let hover = &mut app.editor.language.hover;
    let changed = !hover.is_idle();
    *hover = Hover::default();
    changed
}

/// The pointer is over `offset` of `pane`'s text, or over nothing (`None`).
/// Returns the word to arm the timer for, when it is a new one.
pub fn pointer_over(app: &mut AppState, pane: PaneId, offset: Option<usize>) -> Option<Rest> {
    let Some(offset) = offset else {
        clear(app);
        return None;
    };
    let target = target(app, pane)?;
    let Some(word) = word_at(&target.text, offset) else {
        clear(app);
        return None;
    };
    let rest = Rest {
        pane,
        view: target.view,
        input_generation: target.input_generation,
        word,
        stamp: target.stamp,
    };
    let hover = &mut app.editor.language.hover;
    if hover.rest.as_ref() == Some(&rest) {
        hover.moved_at = Some(Instant::now());
        return None;
    }
    *hover = Hover {
        rest: Some(rest.clone()),
        moved_at: Some(Instant::now()),
        ..Hover::default()
    };
    Some(rest)
}

/// Starts the timer that asks after the pointer has rested.
pub fn arm(state: &AppContext, rest: Rest) {
    // How long the pointer rests on a word before the servers are asked.
    let delay = Duration::from_millis(
        state
            .get()
            .preferences
            .values()
            .language_features
            .hover_delay_ms,
    );
    arm_for(state, rest, delay);
}

fn arm_for(state: &AppContext, rest: Rest, delay: Duration) {
    let Some(application) = state.services().and_then(|runtime| runtime.application()) else {
        return;
    };
    let timed = state.clone();
    let word = rest.clone();
    let timer = ui_timer::after(application, delay, move || fire(&timed, &word));
    state.try_update(|app| {
        let hover = &mut app.editor.language.hover;
        if hover.rest.as_ref() == Some(&rest) {
            hover.timer = Some(Arc::new(timer));
        }
        false
    });
}

/// The pointer rested: ask the servers, if the word is still the one under it.
pub fn fire(state: &AppContext, rest: &Rest) {
    let app = state.get();
    if app.editor.language.hover.rest.as_ref() != Some(rest) {
        return;
    }
    // Still moving over the word: the pointer has not rested, wait out the remainder.
    let delay = Duration::from_millis(app.preferences.values().language_features.hover_delay_ms);
    if let Some(left) = app
        .editor
        .language
        .hover
        .moved_at
        .and_then(|at| delay.checked_sub(at.elapsed()))
        .filter(|left| !left.is_zero())
    {
        arm_for(state, rest.clone(), left);
        return;
    }
    let Some(target) = target(&app, rest.pane).filter(|t| {
        t.view == rest.view && t.input_generation == rest.input_generation && t.stamp == rest.stamp
    }) else {
        return;
    };
    let Some(services) = services(state) else {
        return;
    };
    let handle = Arc::new(services.request(RequestSpec::new(
        Feature::Hover,
        target.resource.clone(),
        target.lsp_stamp(),
        // The middle of the word: a server may not answer at its very edge.
        rest.word.start + (rest.word.end - rest.word.start) / 2,
    )));
    state.try_update(|app| {
        let hover = &mut app.editor.language.hover;
        if hover.rest.as_ref() == Some(rest) {
            hover.pending = Some(Pending {
                rest: rest.clone(),
                handle,
                text: target.text.clone(),
            });
        }
        false
    });
}

/// Turns a finished request into a card.
pub fn tick(app: &mut AppState) -> bool {
    let Some(pending) = app.editor.language.hover.pending.clone() else {
        return false;
    };
    let progress = pending.handle.poll();
    if !progress.finished {
        return false;
    }
    let hover = &mut app.editor.language.hover;
    hover.pending = None;
    if hover.rest.as_ref() != Some(&pending.rest) {
        return false;
    }
    let answers = progress.usable(Feature::Hover);
    let (servers, found): (Vec<_>, Vec<reply::Hover>) = answers
        .iter()
        .filter_map(|part| {
            Some((
                part.server.to_string(),
                reply::hover(&part.value, part.encoding)?,
            ))
        })
        .unzip();
    let mut server = String::new();
    for name in servers {
        if !server.split(", ").any(|known| known == name) {
            if !server.is_empty() {
                server.push_str(", ");
            }
            server.push_str(&name);
        }
    }
    let Some(first) = found.first() else {
        return false;
    };
    let anchor = first
        .span
        .map(|span| span.bytes(&pending.text))
        .filter(|range| !range.is_empty())
        .unwrap_or_else(|| pending.rest.word.clone());
    let source = Markup::join(
        &found
            .iter()
            .map(|hover| hover.markdown.clone())
            .collect::<Vec<_>>(),
    )
    .expect("nonempty hover");
    let name = pending
        .text
        .get(anchor.clone())
        .unwrap_or_default()
        .to_owned();
    let kind = signature(&source.text).and_then(|signature| kind_of(&signature));
    hover.card = Some(Card {
        rest: pending.rest,
        anchor,
        name,
        kind,
        server,
        documentation: source,
    });
    true
}

/// The signature a card shows: the first fenced code block, or failing that
/// the first paragraph, leaving out a module path such as `a::b::C` above it.
pub fn signature(markdown: &str) -> Option<String> {
    let is_path = |text: &str| {
        let text = text.trim();
        text.contains("::") && !text.chars().any(char::is_whitespace)
    };
    let mut blocks: Vec<String> = Vec::new();
    let mut open: Option<(String, Vec<&str>)> = None;
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        let fence = ["```", "~~~"].into_iter().find(|f| trimmed.starts_with(f));
        match (&mut open, fence) {
            (None, Some(fence)) => open = Some((fence.to_owned(), Vec::new())),
            (Some((opened, body)), Some(fence)) if *opened == fence => {
                blocks.push(body.join("\n"));
                open = None;
            }
            (Some((_, body)), _) => body.push(line),
            (None, None) => {}
        }
    }
    if blocks.is_empty() {
        blocks = markdown
            .split("\n\n")
            .map(|paragraph| paragraph.trim().to_owned())
            .collect();
    }
    blocks
        .into_iter()
        .map(|block| block.trim().to_owned())
        .find(|block| !block.is_empty() && !is_path(block))
}

/// What a signature declares, as a chip for the lists' colours and a label: the
/// first declaring keyword of the usual C-family spellings. A function that
/// takes `self`/`this` or sits under an `impl` line is a method.
pub fn kind_of(signature: &str) -> Option<(&'static str, &'static str)> {
    let words: Vec<&str> = signature
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|word| !word.is_empty())
        .collect();
    let has = |names: &[&str]| words.iter().any(|word| names.contains(word));
    if has(&["fn", "function", "def", "func"]) {
        let method = has(&["impl", "self", "this"]) || has(&["method"]);
        return Some(if method { ("m", "METHOD") } else { ("fn", "FUNCTION") });
    }
    let first = words.iter().find_map(|word| match *word {
        "struct" => Some(("S", "STRUCT")),
        "enum" => Some(("E", "ENUM")),
        "trait" | "interface" | "protocol" => Some(("I", "TRAIT")),
        "class" => Some(("C", "CLASS")),
        "mod" | "module" | "namespace" => Some(("{}", "MODULE")),
        "type" | "typedef" | "typealias" => Some(("T", "TYPE")),
        "macro_rules" | "macro" => Some(("fn", "MACRO")),
        "const" | "static" => Some(("c", "CONSTANT")),
        "let" | "var" | "val" => Some(("v", "VARIABLE")),
        _ => None,
    });
    // A field or local: `name: Type` with no declaring keyword.
    first.or_else(|| signature.contains(':').then_some(("f", "FIELD")))
}

/// A footer button of the card: look the word up where it is defined or used.
pub fn run(
    state: &AppContext,
    action: crate::ui::editor_binding::HoverAction,
    rect: lgui::prelude::UiRect,
) {
    use crate::ui::editor_binding::HoverAction;
    let mut found = false;
    state.update(|app| {
        let at = app
            .editor
            .language
            .hover
            .card
            .as_ref()
            .map(|card| card.rest.word.start + (card.rest.word.end - card.rest.word.start) / 2);
        clear(app);
        if let Some(offset) = at
            && let Some(mut editor) = app.workspace.active_editor_mut()
        {
            editor.set_cursor(offset);
            found = true;
        }
    });
    if !found {
        return;
    }
    let wanted = match action {
        HoverAction::Definition => super::actions::Wanted::Goto(Feature::Definition),
        HoverAction::References => super::actions::Wanted::References,
    };
    super::actions::begin(state, wanted, rect, None);
}

/// Asks about the cursor position at once (the keyboard form of hovering).
pub fn show_at_cursor(state: &AppContext) {
    let app = state.get();
    let pane = app.workspace.active_pane();
    let Some(Target { cursor, .. }) = target(&app, pane) else {
        return;
    };
    drop(app);
    let mut rest = None;
    state.update(|app| {
        // An explicit command is also a retry. The previous request may have
        // returned ContentModified while the server was loading the project;
        // pointer_over deliberately ignores an unchanged mouse position.
        clear(app);
        rest = pointer_over(app, pane, Some(cursor));
    });
    if let Some(rest) = rest {
        fire(state, &rest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signature_says_what_it_declares() {
        let kind = |text| kind_of(text).map(|(_, label)| label);
        assert_eq!(kind("impl<T> Option<T>
pub const fn filter(self) -> Self"), Some("METHOD"));
        assert_eq!(kind("pub fn new() -> Self"), Some("FUNCTION"));
        assert_eq!(kind("pub struct Entry { }"), Some("STRUCT"));
        assert_eq!(kind("const EMPTY: u8 = 0"), Some("CONSTANT"));
        assert_eq!(kind("in_partial: bool"), Some("FIELD"));
        assert_eq!(kind("plain words"), None);
    }

    #[test]
    fn the_signature_skips_a_module_path() {
        let text = "```rust\nstd::path::Path\n```\n\n```rust\npub fn is_absolute(&self) -> bool\n```\n\nDocs.";
        assert_eq!(
            signature(text).as_deref(),
            Some("pub fn is_absolute(&self) -> bool")
        );
        assert_eq!(
            signature("int f(void)\n\nDocs").as_deref(),
            Some("int f(void)")
        );
        assert_eq!(
            signature("a::b::C\n\nlive: u16").as_deref(),
            Some("live: u16")
        );
        assert_eq!(signature("  "), None);
    }

    #[test]
    fn the_word_under_an_offset_includes_its_edges() {
        let text = "let foo_bar = 1;";
        assert_eq!(word_at(text, 5), Some(4..11));
        assert_eq!(word_at(text, 4), Some(4..11), "at the start");
        assert_eq!(word_at(text, 11), Some(4..11), "at the end");
        assert_eq!(
            word_at(text, 3),
            Some(0..3),
            "the space after let touches let"
        );
        assert_eq!(word_at("a  b", 2), None, "between two spaces");
        assert_eq!(word_at("x", 9), None);
        assert_eq!(word_at("变量名 x", "变量".len()), Some(0.."变量名".len()));
        assert_eq!(word_at("变量", 1), None, "inside a character");
    }
}
