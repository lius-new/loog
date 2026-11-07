//! Links by pointer: with the platform's modifier held, a file or folder path in
//! the text that names something, or a word a server can find the definition of,
//! is underlined and tinted, and a click on it goes there.
use super::hover::Rest;
use super::navigate::Place;
use super::{AppContext, AppState, services, target};
use crate::features::language_service::capabilities::PositionEncoding;
use crate::features::language_service::position::Position;
use crate::features::language_service::reply::Span;
use crate::features::language_service::request::Feature;
use crate::features::providers::{RequestHandle, RequestSpec};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// What lies under the pointer: a path (known at once) or a word a server is
/// asked about.
#[derive(Clone)]
struct Lookup {
    rest: Rest,
    /// The text to underline: the whole path, or the word.
    shown: Range<usize>,
    /// The question to a server; none for a path.
    handle: Option<Arc<RequestHandle>>,
    /// The text the question was asked of, to read the answer's ranges in.
    text: Arc<String>,
    /// Where it leads, once known; empty when nowhere else.
    places: Option<Vec<Place>>,
    /// A folder a path names, to show in the Explorer.
    folder: Option<PathBuf>,
}

/// What a click on a link does.
pub enum Goal {
    /// Go to a place in a file.
    Places(Vec<Place>),
    /// Show a folder in the Explorer.
    Folder(PathBuf),
}

#[derive(Clone, Default)]
pub struct Link {
    lookup: Option<Lookup>,
}

impl Link {
    /// What to show as a link, and where it leads.
    fn active(&self) -> Option<(&Rest, &Range<usize>, Goal)> {
        let lookup = self.lookup.as_ref()?;
        let goal = match (&lookup.folder, lookup.places.as_deref()) {
            (Some(folder), _) => Goal::Folder(folder.clone()),
            (None, Some(places)) if !places.is_empty() => Goal::Places(places.to_vec()),
            _ => return None,
        };
        Some((&lookup.rest, &lookup.shown, goal))
    }
}

/// Whether the key that turns the pointer into a link is held: Cmd on macOS,
/// Ctrl elsewhere.
pub fn held(app: &AppState) -> bool {
    let modifiers = app.editor.modifiers;
    if cfg!(target_os = "macos") {
        modifiers.meta()
    } else {
        modifiers.ctrl()
    }
}

/// The text to underline in `pane`'s `stamp`, if the key is held and it leads
/// somewhere.
pub fn word_for(
    app: &AppState,
    pane: crate::domain::identity::PaneId,
    stamp: &crate::domain::document::DocumentStamp,
) -> Option<Range<usize>> {
    if !held(app) {
        return None;
    }
    let (rest, shown, _) = app.editor.language.link.active()?;
    (rest.pane == pane && rest.stamp == *stamp).then(|| shown.clone())
}

/// Follows the pointer: looks at the word under it while the key is held, and
/// forgets the answer when either is gone.
pub fn observe(ctx: &AppContext, app: &mut AppState) {
    let rest = held(app)
        .then(|| app.editor.language.hover.rest().cloned())
        .flatten();
    let Some(rest) = rest else {
        app.editor.language.link.lookup = None;
        return;
    };
    if app
        .editor
        .language
        .link
        .lookup
        .as_ref()
        .is_some_and(|lookup| lookup.rest == rest)
    {
        return;
    }
    let Some(target) = target(app, rest.pane).filter(|t| {
        t.view == rest.view && t.input_generation == rest.input_generation && t.stamp == rest.stamp
    }) else {
        app.editor.language.link.lookup = None;
        return;
    };
    // A path that names something wins over what a server makes of one of its words.
    let roots = app.explorer.model.roots().to_vec();
    if let Some((shown, found)) = path_at(&target.text, &rest.word, target.path.parent(), &roots) {
        let (places, folder) = match found {
            Found::File(place) => (vec![place], None),
            Found::Folder(path) => (Vec::new(), Some(path)),
        };
        app.editor.language.link.lookup = Some(Lookup {
            rest,
            shown,
            handle: None,
            text: target.text.clone(),
            places: Some(places),
            folder,
        });
        return;
    }
    let Some(services) = services(ctx) else {
        return;
    };
    let handle = Arc::new(services.request(RequestSpec::new(
        Feature::Definition,
        target.path.clone(),
        target.lsp_stamp(),
        // The middle of the word: a server may not answer at its very edge.
        rest.word.start + (rest.word.end - rest.word.start) / 2,
    )));
    app.editor.language.link.lookup = Some(Lookup {
        shown: rest.word.clone(),
        rest,
        handle: Some(handle),
        text: target.text.clone(),
        places: None,
        folder: None,
    });
}

/// Takes a server's answer when it is complete.
pub fn tick(app: &mut AppState) -> bool {
    let Some(lookup) = app.editor.language.link.lookup.clone() else {
        return false;
    };
    let Some(handle) = lookup.handle.as_ref().filter(|_| lookup.places.is_none()) else {
        return false;
    };
    let progress = handle.poll();
    if !progress.finished {
        return false;
    }
    let current = app
        .workspace
        .active_path()
        .map(std::path::Path::to_path_buf);
    let places: Vec<Place> = super::actions::locations(&progress.parts, Feature::Definition)
        .into_iter()
        .filter_map(|place| {
            // The word at its own definition has nowhere else to go.
            let here = current.as_deref() == Some(place.path.as_path())
                && place.range(&lookup.text).contains(&lookup.rest.word.start);
            (!here).then_some(place)
        })
        .collect();
    if let Some(current) = app.editor.language.link.lookup.as_mut()
        && current.rest == lookup.rest
    {
        current.places = Some(places);
    }
    true
}

/// A click with the key held on the link: where to go.
pub fn click_target(app: &AppState, offset: usize) -> Option<Goal> {
    if !held(app) {
        return None;
    }
    let (_, shown, goal) = app.editor.language.link.active()?;
    shown.contains(&offset).then_some(goal)
}

/// Goes to the one place the link leads to; with several, puts the caret on the
/// word and lists them as Go to Definition does. A folder is shown in the
/// Explorer.
pub fn follow(state: &AppContext, goal: Goal, offset: usize, rect: lgui::prelude::UiRect) {
    let mut places = match goal {
        Goal::Folder(path) => {
            crate::app::explorer::reveal_folder(state, &path);
            return;
        }
        Goal::Places(places) => places,
    };
    if places.len() == 1 {
        super::navigate::go(state, places.remove(0), rect, true);
        return;
    }
    state.update(|app| {
        if let Some(mut editor) = app.workspace.active_editor_mut() {
            editor.set_cursor(offset);
        }
    });
    super::actions::begin(
        state,
        super::actions::Wanted::Goto(Feature::Definition),
        rect,
        None,
    );
}

/// What a path in the text names.
enum Found {
    File(Place),
    Folder(PathBuf),
}

/// The path around `word` in `text`, if it names a file or folder: the text it
/// spans, and where to go. A relative path is looked for beside the document,
/// then in each workspace folder; `:line` and `:line:column` may follow a file.
fn path_at(
    text: &str,
    word: &Range<usize>,
    directory: Option<&Path>,
    roots: &[PathBuf],
) -> Option<(Range<usize>, Found)> {
    let span = path_span(text, word)?;
    let (spec, line, column) = split_position(&text[span.clone()]);
    if spec.is_empty() || spec.contains("://") || !spec.contains(['/', '\\', '.']) {
        return None;
    }
    let path = resolve(Path::new(spec), directory, roots)?;
    if path.is_dir() {
        return Some((span, Found::Folder(path)));
    }
    let place = match line {
        Some(line) => {
            let at = Position {
                line: line.saturating_sub(1),
                character: column.unwrap_or(1).saturating_sub(1),
            };
            Place::span(
                path,
                Span {
                    start: at,
                    end: at,
                    encoding: PositionEncoding::Utf8,
                },
            )
        }
        None => Place::bytes(path, 0..0),
    };
    Some((span, Found::File(place)))
}

/// The run of path characters around `word`, without the punctuation of the
/// sentence or code it sits in.
fn path_span(text: &str, word: &Range<usize>) -> Option<Range<usize>> {
    let is_path = |c: char| {
        !c.is_whitespace()
            && !matches!(
                c,
                '"' | '\''
                    | '`'
                    | '<'
                    | '>'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '|'
                    | ','
                    | ';'
                    | '*'
                    | '?'
                    | '='
            )
    };
    let start = text.get(..word.start)?;
    let after = text.get(word.end..)?;
    let from = start
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_path(*c))
        .last()
        .map_or(word.start, |(at, _)| at);
    let to = word.end
        + after
            .char_indices()
            .take_while(|(_, c)| is_path(*c))
            .last()
            .map_or(0, |(at, c)| at + c.len_utf8());
    let mut span = from..to;
    while span.end > word.end && text[span.clone()].ends_with(['.', ':', '!']) {
        span.end -= 1;
    }
    (span.start < span.end).then_some(span)
}

/// `path`, `path:line` or `path:line:column`, taken apart. A drive letter's
/// colon is not a position.
fn split_position(spec: &str) -> (&str, Option<u32>, Option<u32>) {
    fn number(rest: &str) -> Option<(&str, u32)> {
        let (head, digits) = rest.rsplit_once(':')?;
        (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
            .then(|| digits.parse().ok().map(|n| (head, n)))
            .flatten()
    }
    let Some((head, last)) = number(spec) else {
        return (spec, None, None);
    };
    match number(head) {
        Some((path, line)) => (path, Some(line), Some(last)),
        None => (head, Some(last), None),
    }
}

/// The file or folder `path` names, if there is one.
fn resolve(path: &Path, directory: Option<&Path>, roots: &[PathBuf]) -> Option<PathBuf> {
    if path.is_absolute() {
        return path.exists().then(|| normalize(path));
    }
    directory
        .into_iter()
        .chain(roots.iter().map(PathBuf::as_path))
        .map(|base| base.join(path))
        .find(|candidate| candidate.exists())
        .map(|found| normalize(&found))
}

/// `path` without `.` and `..` parts, so the same file is not opened twice
/// under two spellings.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span_of(text: &str, word: &str) -> String {
        let at = text.find(word).unwrap();
        let span = path_span(text, &(at..at + word.len())).unwrap();
        text[span].to_owned()
    }

    #[test]
    fn a_path_is_the_run_of_path_characters_around_the_word() {
        assert_eq!(
            span_of("see ./src/app/mod.rs for it", "app"),
            "./src/app/mod.rs"
        );
        // A space ends the path: quote-free paths with spaces are not guessed at.
        assert_eq!(span_of(r#"open("../a b/c.txt")"#, "c"), "b/c.txt");
        assert_eq!(
            span_of("in C:\\dir\\file.rs:12:3.", "file"),
            "C:\\dir\\file.rs:12:3"
        );
        assert_eq!(span_of("mod.rs, then", "mod"), "mod.rs");
    }

    #[test]
    fn a_position_after_the_path_is_split_off_but_a_drive_letter_is_not() {
        assert_eq!(split_position("a/b.rs"), ("a/b.rs", None, None));
        assert_eq!(split_position("a/b.rs:12"), ("a/b.rs", Some(12), None));
        assert_eq!(split_position("a/b.rs:12:3"), ("a/b.rs", Some(12), Some(3)));
        assert_eq!(split_position("C:\\a\\b.rs"), ("C:\\a\\b.rs", None, None));
        assert_eq!(
            split_position("C:\\a\\b.rs:7"),
            ("C:\\a\\b.rs", Some(7), None)
        );
    }

    #[test]
    fn dots_in_a_path_are_resolved_away() {
        assert_eq!(
            normalize(Path::new("a/./b/../c.rs")),
            PathBuf::from("a").join("c.rs")
        );
    }

    #[test]
    fn a_path_that_names_a_file_or_folder_is_a_link() {
        let dir = std::env::temp_dir().join(format!("loog-link-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("note.txt"), "x").unwrap();
        let text = "see sub/note.txt:4:2 and sub/missing.txt and ./sub/ and sub";
        let word = |w: &str| {
            let at = text.find(w).unwrap();
            at..at + w.len()
        };
        let (shown, found) = path_at(text, &word("note"), Some(&dir), &[]).unwrap();
        assert_eq!(&text[shown], "sub/note.txt:4:2");
        let Found::File(place) = found else {
            panic!("a file")
        };
        assert_eq!(place.path, dir.join("sub").join("note.txt"));
        // Found in a workspace folder when not beside the document.
        assert!(path_at(text, &word("note"), None, std::slice::from_ref(&dir)).is_some());
        assert!(path_at(text, &word("missing"), Some(&dir), &[]).is_none());
        // A folder is a link to show in the Explorer.
        let at = text.find("./sub/").unwrap() + 3;
        let (_, found) = path_at(text, &(at..at + 3), Some(&dir), &[]).unwrap();
        assert!(matches!(found, Found::Folder(path) if path == dir.join("sub")));
        // A word with no separator or extension is left to the language server.
        assert!(path_at("sub", &(0..3), Some(&dir), &[]).is_none());
        // An absolute path needs no base.
        let absolute = format!("{}", dir.join("sub").join("note.txt").display());
        assert!(
            path_at(
                &absolute,
                &(absolute.len() - 6..absolute.len() - 2),
                None,
                &[]
            )
            .is_some()
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
