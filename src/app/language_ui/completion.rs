//! Completion: asking the servers as the person types, keeping the list while
//! typing continues, and turning the chosen item into an edit.
use super::snippet_session;
use super::{AppContext, AppState, Target, services, target};
use crate::app::ui_timer;
use crate::domain::identity::{FileId, PaneId, ViewId};
use crate::features::language_service::completion::{
    Completion, Entry, Shown, commits, filter, insertion, word_start,
};
use crate::features::language_service::reply;
use crate::features::language_service::request::Feature;
use crate::features::language_service::snippet;
use crate::infrastructure::lsp::manager::{RequestHandle, RequestSpec};
use crate::model::text::{Assoc, Edit, EditKind, Selection};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// Typing waits this long before the servers are asked, so a burst is one request.
const DEBOUNCE: Duration = Duration::from_millis(30);

#[derive(Clone)]
pub struct Session {
    pub pane: PaneId,
    pub document: FileId,
    pub view: ViewId,
    pub input_generation: u64,
    pub incarnation: u64,
    /// The text the servers were asked about, to read their ranges in.
    pub requested: Arc<String>,
    local_identity: Option<usize>,
    pub manual: bool,
    /// Started by a trigger character (so an empty word is expected).
    triggered: bool,
    handle: Option<Arc<RequestHandle>>,
    pub data: Completion,
    pub shown: Vec<Shown>,
    pub selected: usize,
    pub scroll: usize,
    resolving: Option<(usize, Arc<RequestHandle>)>,
    last_cursor: usize,
    last_version: u64,
    /// Lists extensions offered for this session, by source. They are merged
    /// with every server answer, so neither replaces the other.
    external: Vec<(u64, usize, reply::CompletionList)>,
    /// Extension requests still in flight; the list is not closed as empty while
    /// one may still answer.
    external_pending: usize,
    /// Items already sent to their extension for resolving: (source, raw JSON).
    resolve_asked: Vec<(u64, String)>,
}

impl Session {
    pub fn set_selected(&mut self, index: usize) {
        if !self.shown.is_empty() {
            self.selected = index.min(self.shown.len() - 1);
            keep_visible(self);
        }
    }

    pub fn scroll_by(&mut self, rows: isize) {
        let max = self.shown.len().saturating_sub(super::popup::VISIBLE) as isize;
        self.scroll = (self.scroll as isize + rows).clamp(0, max) as usize;
    }

    pub fn visible(&self) -> bool {
        !self.shown.is_empty()
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.shown
            .get(self.selected)
            .and_then(|shown| self.data.entries.get(shown.entry))
    }
}

/// Whether the character continues an identifier.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// A key was typed into the document (not pasted, not an undo).
pub fn note_typed(app: &mut AppState, input: &str) {
    app.editor.language.typed = input.chars().last();
}

/// Looks at what changed in the document since the last update and starts,
/// follows or ends a completion accordingly. Runs after every state update, so
/// it is cheap when nothing relevant changed.
pub fn observe(ctx: &AppContext, app: &mut AppState, typed: Option<char>) {
    if crate::app::editor_vim::is_active(app) && !app.editor.vim.is_inserting() {
        close(app);
        app.editor.language.completion_timer = None;
        return;
    }
    if app
        .workspace
        .active_editor()
        .is_some_and(|e| !e.secondary().is_empty())
    {
        close(app);
        return;
    }
    let pane = app.workspace.active_pane();
    let Some(t) = target(app, pane) else {
        close(app);
        return;
    };
    if typed.is_some() {
        // Any intervening input invalidates an outstanding answer. A following
        // typed identifier schedules a fresh, debounced request below.
        close_words(&mut app.editor.language);
    }
    let mut ended = false;
    if let Some(session) = &app.editor.language.completion {
        if session.document != t.document
            || session.view != t.view
            || session.input_generation != t.input_generation
            || session.incarnation != t.stamp.incarnation
        {
            ended = true;
        } else if session.last_version != t.stamp.version || session.last_cursor != t.cursor {
            ended = !follow(app, &t);
        }
    }
    if ended {
        app.editor.language.completion = None;
    }
    let Some(c) = typed else { return };
    super::refactor::on_type(ctx, app, c);
    // A commit character accepts the highlighted item and is then kept.
    let commit = app
        .editor
        .language
        .completion
        .as_ref()
        .and_then(|s| s.selected_entry())
        .is_some_and(|entry| commits(entry, c));
    if commit {
        accept(app, Some(c));
        return;
    }
    if !is_word(c) && app.editor.language.completion.is_some() {
        app.editor.language.completion = None;
    }
    let triggers = trigger_characters(ctx);
    let is_trigger = triggers.iter().any(|trigger| trigger.chars().eq([c]));
    if (is_word(c) || is_trigger) && app.preferences.values().lsp.completion.auto {
        schedule(ctx, app, is_trigger.then_some(c));
    }
}

/// The characters any running server wants completion after.
fn trigger_characters(ctx: &AppContext) -> Vec<String> {
    services(ctx)
        .map(|s| {
            s.infos()
                .iter()
                .filter_map(|info| info.providers.as_ref()?.completion.as_ref())
                .flat_map(|options| options.trigger_characters.clone())
                .collect()
        })
        .unwrap_or_default()
}

fn offers_completion(ctx: &AppContext) -> bool {
    services(ctx).is_some_and(|s| {
        s.infos().iter().any(|info| {
            info.providers
                .as_ref()
                .is_some_and(|p| p.completion.is_some())
        })
    })
}

/// Asks for completions soon, unless more typing comes first.
fn schedule(ctx: &AppContext, app: &mut AppState, trigger: Option<char>) {
    if !offers_completion(ctx) {
        if app.preferences.values().lsp.completion.words_fallback && trigger.is_none() {
            if app
                .editor
                .language
                .completion
                .as_ref()
                .is_none_or(|s| s.local_identity.is_none())
            {
                schedule_words(ctx, app);
            }
        }
        return;
    }
    let Some(application) = ctx.services().and_then(|r| r.application()) else {
        return;
    };
    let timed = ctx.clone();
    app.editor.language.completion_timer = Some(Arc::new(ui_timer::after(
        application,
        DEBOUNCE,
        move || {
            timed.update(|app| request(&timed, app, false, trigger));
        },
    )));
}

/// The words of the document, as completions, for when no server offers any.
#[cfg(test)]
pub fn words(app: &mut AppState) {
    if crate::app::editor_vim::is_active(app) && !app.editor.vim.is_inserting() {
        crate::app::editor_vim::start_multi(app);
    }
    crate::app::multicursor::primary_only(app);
    let pane = app.workspace.active_pane();
    let Some(t) = target(app, pane) else { return };
    let Some(start) = local_start(&t.text, t.cursor) else {
        return;
    };
    let Some(seen) = collect_words(&t.text, t.cursor, &|| false) else {
        return;
    };
    publish_words(app, t, start, seen);
}

pub(super) fn close_words(language: &mut super::LanguageUi) {
    if language.words_request.take().is_some() {
        language.completion_timer = None;
    }
}

fn schedule_words(ctx: &AppContext, app: &mut AppState) {
    let Some(application) = ctx.services().and_then(|r| r.application()) else {
        return;
    };
    let Some(t) = target(app, app.workspace.active_pane()) else {
        return;
    };
    let Some(start) = local_start(&t.text, t.cursor) else {
        return;
    };
    let token = Arc::new(());
    app.editor.language.words_request = Some(token.clone());
    let state = ctx.clone();
    // Retain only a bounded window while debouncing and scanning. Keeping an
    // Arc of the entire source here would force a full copy on the next key.
    let window = word_window(&t.text, t.cursor);
    let truncated_start = window.start > 0;
    let truncated_end = window.end < t.text.len();
    let local_cursor = t.cursor - window.start;
    let text = t.text[window].to_owned();
    let cursor = t.cursor;
    // Keep only metadata in the UI callback: do not retain the source after scanning.
    let stamp = t.stamp;
    let view = t.view;
    let generation = t.input_generation;
    app.editor.language.completion_timer = Some(Arc::new(ui_timer::background_after(
        application,
        DEBOUNCE,
        move |cancelled| {
            collect_window(
                &text,
                local_cursor,
                truncated_start,
                truncated_end,
                cancelled,
            )
        },
        move |seen| {
            state.try_update(|app| {
                if !app
                    .editor
                    .language
                    .words_request
                    .as_ref()
                    .is_some_and(|r| Arc::ptr_eq(r, &token))
                {
                    return false;
                }
                let Some(current) = target(app, app.workspace.active_pane()) else {
                    return false;
                };
                if current.stamp != stamp
                    || current.view != view
                    || current.input_generation != generation
                    || current.cursor != cursor
                {
                    return false;
                }
                close_words(&mut app.editor.language);
                publish_words(app, current, start, seen);
                true
            });
        },
    )));
}

const WORD_BYTES: usize = 256 * 1024;
const IDENTIFIER_BYTES: usize = 128;

fn local_start(text: &str, cursor: usize) -> Option<usize> {
    let mut start = cursor;
    for c in text.get(..cursor)?.chars().rev() {
        if !is_word(c) {
            break;
        }
        start -= c.len_utf8();
        if cursor - start > IDENTIFIER_BYTES {
            return None;
        }
    }
    Some(start)
}

fn word_window(text: &str, cursor: usize) -> std::ops::Range<usize> {
    let mut from = cursor.saturating_sub(WORD_BYTES / 2);
    let mut to = (from + WORD_BYTES).min(text.len());
    from = from.min(to.saturating_sub(WORD_BYTES));
    while !text.is_char_boundary(from) {
        from += 1;
    }
    while !text.is_char_boundary(to) {
        to -= 1;
    }
    from..to
}

#[cfg(test)]
fn collect_words(
    text: &str,
    cursor: usize,
    cancelled: &dyn Fn() -> bool,
) -> Option<std::collections::BTreeSet<String>> {
    local_start(text, cursor)?;
    let window = word_window(text, cursor);
    collect_window(
        &text[window.clone()],
        cursor - window.start,
        window.start > 0,
        window.end < text.len(),
        cancelled,
    )
}

fn collect_window(
    text: &str,
    cursor: usize,
    truncated_start: bool,
    truncated_end: bool,
    cancelled: &dyn Fn() -> bool,
) -> Option<std::collections::BTreeSet<String>> {
    let skip = local_start(text, cursor)?..cursor;
    let mut seen = std::collections::BTreeSet::new();
    for (at, word) in words_of(text) {
        if cancelled() {
            return None;
        }
        // Never offer fragments cut off by the scan window, or the current word.
        if (at == 0 && truncated_start)
            || (at + word.len() == text.len() && truncated_end)
            || (at <= skip.end && at + word.len() >= skip.start)
        {
            continue;
        }
        if word.len() <= IDENTIFIER_BYTES
            && word.chars().take(3).count() == 3
            && !seen.contains(word)
        {
            seen.insert(word.to_owned());
        }
        if seen.len() == 5000 {
            break;
        }
    }
    (!cancelled()).then_some(seen)
}

fn publish_words(
    app: &mut AppState,
    t: Target,
    start: usize,
    seen: std::collections::BTreeSet<String>,
) {
    let pane = t.pane;
    let list = reply::CompletionList {
        items: seen
            .into_iter()
            .map(|word| reply::CompletionItem {
                label: word,
                label_detail: None,
                kind: None,
                detail: None,
                documentation: None,
                deprecated: false,
                preselect: false,
                sort_text: None,
                filter_text: None,
                insert_text: None,
                snippet: false,
                as_is: false,
                edit: None,
                additional_edits: Vec::new(),
                commit_characters: Vec::new(),
                command: None,
                raw: serde_json::Value::Null,
            })
            .collect(),
        incomplete: false,
    };
    let data = Completion::merge(start, t.cursor, vec![(0, usize::MAX, list)]);
    app.editor.language.completion = Some(Session {
        pane,
        document: t.document,
        view: t.view,
        input_generation: t.input_generation,
        incarnation: t.stamp.incarnation,
        requested: Arc::default(),
        local_identity: Some(t.stamp.snapshot_identity),
        manual: false,
        triggered: false,
        handle: None,
        data,
        shown: Vec::new(),
        selected: 0,
        scroll: 0,
        resolving: None,
        last_cursor: t.cursor,
        last_version: t.stamp.version,
        external: Vec::new(),
        external_pending: 0,
        resolve_asked: Vec::new(),
    });
    let typed = t.text[start..t.cursor].to_owned();
    if let Some(session) = app.editor.language.completion.as_mut() {
        refilter(session, &typed);
        if session.shown.is_empty() {
            app.editor.language.completion = None;
        }
    }
}

/// Identifiers in `text` with where each starts.
pub fn words_of(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.split(|c: char| !is_word(c)).filter_map(move |word| {
        let at = word.as_ptr() as usize - text.as_ptr() as usize;
        (!word.is_empty() && !word.starts_with(|c: char| c.is_ascii_digit())).then_some((at, word))
    })
}

/// Asks the servers now. `manual` is the person pressing the key.
pub fn request(ctx: &AppContext, app: &mut AppState, manual: bool, trigger: Option<char>) {
    close_words(&mut app.editor.language);
    if manual {
        if crate::app::editor_vim::is_active(app) && !app.editor.vim.is_inserting() {
            crate::app::editor_vim::start_multi(app);
        }
        crate::app::multicursor::primary_only(app);
    } else if app
        .workspace
        .active_editor()
        .is_some_and(|e| !e.secondary().is_empty())
    {
        return;
    }
    app.editor.language.completion_timer = None;
    if !manual && snippet_session::choice_visible(app) {
        return;
    }
    let pane = app.workspace.active_pane();
    let Some(t) = target(app, pane) else { return };
    if !offers_completion(ctx) && app.preferences.values().lsp.completion.words_fallback {
        schedule_words(ctx, app);
        return;
    }
    let Some(services) = services(ctx) else {
        return;
    };
    // The servers must have the text this question is about.
    services.sync(app);
    let start = if trigger.is_some() {
        t.cursor
    } else {
        word_start(&t.text, t.cursor)
    };
    let context = match trigger {
        Some(c) => json!({ "triggerKind": 2, "triggerCharacter": c.to_string() }),
        None if manual => json!({ "triggerKind": 1 }),
        None => json!({ "triggerKind": 1 }),
    };
    let handle = Arc::new(
        services.request(
            RequestSpec::new(Feature::Completion, t.path.clone(), t.lsp_stamp(), t.cursor)
                .extra(json!({ "context": context })),
        ),
    );
    let same = app.editor.language.completion.as_ref().is_some_and(|s| {
        s.document == t.document
            && s.view == t.view
            && s.input_generation == t.input_generation
            && s.data.word_start == start
    });
    if same {
        if let Some(session) = app.editor.language.completion.as_mut() {
            session.resolving = None;
            session.handle = Some(handle);
            session.requested = t.text.clone();
            session.local_identity = None;
            session.data.cursor = t.cursor;
        }
        ask_extensions(ctx, app, start, t.cursor, trigger);
        return;
    }
    app.editor.language.completion = Some(Session {
        pane,
        document: t.document,
        view: t.view,
        input_generation: t.input_generation,
        incarnation: t.stamp.incarnation,
        requested: t.text.clone(),
        local_identity: None,
        manual,
        triggered: trigger.is_some(),
        handle: Some(handle),
        data: Completion {
            word_start: start,
            cursor: t.cursor,
            ..Completion::default()
        },
        shown: Vec::new(),
        selected: 0,
        scroll: 0,
        resolving: None,
        last_cursor: t.cursor,
        last_version: t.stamp.version,
        external: Vec::new(),
        external_pending: 0,
        resolve_asked: Vec::new(),
    });
    ask_extensions(ctx, app, start, t.cursor, trigger);
}

/// Keeps the list in step with the text. Returns false when the list should go.
fn follow(app: &mut AppState, t: &Target) -> bool {
    let Some(session) = app.editor.language.completion.as_mut() else {
        return false;
    };
    let start = session.data.word_start;
    // The text before the word is what the list was asked about; the cursor must
    // still be in the word.
    let prefix_same = if let Some(identity) = session.local_identity {
        local_start(&t.text, t.cursor) == Some(start)
            && app
                .workspace
                .documents
                .get(&t.document)
                .and_then(|doc| doc.buffer.revisions_since(session.last_version, identity))
                .is_some_and(|revisions| {
                    revisions
                        .iter()
                        .all(|r| r.edits.iter().all(|e| e.old.start >= start))
                })
    } else {
        t.text.len() >= start
            && session.requested.len() >= start
            && t.text.is_char_boundary(start)
            && t.text.as_bytes()[..start] == session.requested.as_bytes()[..start]
    };
    if !prefix_same || t.cursor < start || !t.text.is_char_boundary(t.cursor) {
        return false;
    }
    let typed = &t.text[start..t.cursor];
    if !typed.chars().all(is_word) {
        return false;
    }
    if typed.is_empty() && !session.manual && !session.triggered {
        return false;
    }
    if session.local_identity.is_some() {
        session.local_identity = Some(t.stamp.snapshot_identity);
    }
    session.last_cursor = t.cursor;
    let version_changed = session.last_version != t.stamp.version;
    session.last_version = t.stamp.version;
    refilter(session, typed);
    // Typing past the start of a list the servers called incomplete asks again.
    if version_changed && session.data.incomplete && !typed.is_empty() {
        session.data.cursor = t.cursor;
    }
    !(session.shown.is_empty() && session.handle.is_none() && !session.data.incomplete)
}

fn refilter(session: &mut Session, typed: &str) {
    let before = session
        .selected_entry()
        .map(|entry| (entry.item.label.clone(), entry.item.kind));
    session.shown = filter(&session.data, typed);
    session.selected = before
        .and_then(|(label, kind)| {
            session.shown.iter().position(|s| {
                let item = &session.data.entries[s.entry].item;
                item.label == label && item.kind == kind
            })
        })
        .unwrap_or(0);
    session.scroll = 0;
    keep_visible(session);
}

fn keep_visible(session: &mut Session) {
    let rows = super::popup::VISIBLE;
    if session.selected < session.scroll {
        session.scroll = session.selected;
    } else if session.selected >= session.scroll + rows {
        session.scroll = session.selected + 1 - rows;
    }
}

/// Replace candidates and their display indices together. Old indices must
/// never be used to read a shorter or reordered response.
fn replace_candidates(session: &mut Session, mut data: Completion, typed: &str) {
    let selected = session
        .selected_entry()
        .map(|entry| (entry.item.label.clone(), entry.item.kind));
    let resolved: std::collections::HashMap<_, _> = session
        .data
        .entries
        .iter()
        .filter(|entry| entry.resolved)
        .map(|entry| {
            (
                (entry.instance, entry.item.label.clone(), entry.item.kind),
                &entry.item,
            )
        })
        .collect();
    let resolving = session.resolving.take().and_then(|(index, handle)| {
        let old = session.data.entries.get(index)?;
        let index = data
            .entries
            .iter()
            .position(|entry| entry.instance == old.instance && entry.item.raw == old.item.raw)?;
        Some((index, handle))
    });
    for entry in &mut data.entries {
        if let Some(item) =
            resolved.get(&(entry.instance, entry.item.label.clone(), entry.item.kind))
            && item.raw == entry.item.raw
        {
            entry.item = (*item).clone();
            entry.documentation =
                crate::features::language_service::completion::item_documentation(&entry.item);
            entry.resolved = true;
        }
    }
    session.shown.clear();
    session.data = data;
    session.resolving = resolving;
    refilter(session, typed);
    if let Some((label, kind)) = selected
        && let Some(at) = session.shown.iter().position(|shown| {
            let item = &session.data.entries[shown.entry].item;
            item.label == label && item.kind == kind
        })
    {
        session.selected = at;
        keep_visible(session);
    }
}

/// Folds in what the servers answered so far.
pub fn tick(app: &mut AppState) -> bool {
    let text = app_text(app);
    let Some(session) = app.editor.language.completion.as_mut() else {
        return false;
    };
    let mut changed = false;
    if let Some(handle) = session.handle.clone() {
        let progress = handle.poll();
        let server_sources: Vec<(u64, usize, reply::CompletionList)> = progress
            .parts
            .iter()
            .map(|part| {
                (
                    part.instance,
                    part.rank,
                    reply::completion(&part.value, part.encoding),
                )
            })
            .collect();
        if !server_sources.is_empty() || progress.finished {
            let mut sources = server_sources;
            sources.extend(session.external.iter().cloned());
            let word_start = session.data.word_start;
            let cursor = session.data.cursor;
            let merged = Completion::merge(word_start, cursor, sources);
            if progress.finished {
                session.handle = None;
            }
            let typed = typed_prefix(text, session);
            replace_candidates(session, merged, &typed);
            if session.shown.is_empty()
                && session.handle.is_none()
                && !session.data.incomplete
                && session.external_pending == 0
            {
                app.editor.language.completion = None;
            }
            changed = true;
        }
    }
    changed |= tick_resolve(app);
    changed
}

fn app_text(app: &AppState) -> Option<(Arc<String>, usize)> {
    let pane = app.workspace.active_pane();
    target(app, pane).map(|t| (t.text, t.cursor))
}

fn typed_prefix(text: Option<(Arc<String>, usize)>, session: &Session) -> String {
    text.and_then(|(text, cursor)| {
        let start = session.data.word_start;
        (start <= cursor && text.is_char_boundary(start) && text.is_char_boundary(cursor))
            .then(|| text[start..cursor].to_owned())
    })
    .unwrap_or_default()
}

fn tick_resolve(app: &mut AppState) -> bool {
    let Some(session) = app.editor.language.completion.as_mut() else {
        return false;
    };
    let Some((index, handle)) = session.resolving.clone() else {
        return false;
    };
    let progress = handle.poll();
    if !progress.finished {
        return false;
    }
    session.resolving = None;
    if let Some(part) = progress.parts.first()
        && let Some(mut item) = reply::completion_item(&part.value, part.encoding)
        && let Some(entry) = session.data.entries.get_mut(index)
    {
        // Keep the unresolved source identity for matching later snapshots.
        item.raw = entry.item.raw.clone();
        entry.item = item;
        entry.documentation =
            crate::features::language_service::completion::item_documentation(&entry.item);
        entry.resolved = true;
    }
    true
}

/// Moves the highlight by `delta` rows, wrapping.
pub fn move_selection(app: &mut AppState, delta: isize) {
    if let Some(session) = app.editor.language.completion.as_mut() {
        let len = session.shown.len() as isize;
        if len > 0 {
            session.selected = (session.selected as isize + delta).rem_euclid(len) as usize;
            keep_visible(session);
        }
    }
}

pub fn page(app: &mut AppState, delta: isize) {
    if let Some(session) = app.editor.language.completion.as_mut() {
        let len = session.shown.len() as isize;
        if len > 0 {
            session.selected = (session.selected as isize + delta).clamp(0, len - 1) as usize;
            keep_visible(session);
        }
    }
}

/// Asks the server of the highlighted item for its documentation, once.
pub fn resolve_selected(ctx: &AppContext, app: &mut AppState) {
    let pane = app.workspace.active_pane();
    let Some(t) = target(app, pane) else { return };
    let Some(session) = app.editor.language.completion.as_mut() else {
        return;
    };
    if session.resolving.is_some() {
        return;
    }
    let Some(shown) = session.shown.get(session.selected) else {
        return;
    };
    let index = shown.entry;
    let Some(entry) = session.data.entries.get(index) else {
        return;
    };
    if entry.resolved || entry.item.documentation.is_some() {
        return;
    }
    if entry.instance & crate::infrastructure::lsp::manager::EXTERNAL_SOURCE != 0 {
        // Offered by an extension: only that extension can complete it.
        let (source, raw) = (entry.instance, entry.item.raw.clone());
        let key = (source, raw.to_string());
        if session.resolve_asked.contains(&key) {
            return;
        }
        session.resolve_asked.push(key);
        if let Some(host) = ctx.services().and_then(|runtime| runtime.extensions()) {
            host.request_completion_resolve(app, source, &raw);
        }
        return;
    }
    let Some(services) = services(ctx) else {
        return;
    };
    let handle = Arc::new(
        services.request(
            RequestSpec::new(Feature::CompletionResolve, t.path.clone(), t.lsp_stamp(), 0)
                .extra(entry.item.raw.clone())
                .target(entry.instance),
        ),
    );
    session.resolving = Some((index, handle));
}

/// Ask the extensions with a completion provider for this language. Their
/// answers arrive later through [`offer_external`].
fn ask_extensions(
    ctx: &AppContext,
    app: &mut AppState,
    start: usize,
    cursor: usize,
    trigger: Option<char>,
) {
    let Some(host) = ctx.services().and_then(|runtime| runtime.extensions()) else {
        return;
    };
    let asked = host.request_completion(app, start, cursor, trigger);
    if let Some(session) = app.editor.language.completion.as_mut() {
        session.external_pending = asked;
    }
}

/// An extension answered a completion request (or could not in time:
/// `items` is `None`). The answer serves the session while it is the same
/// document life and the same word is being completed; typing since is handled
/// by the ordinary filter.
pub fn offer_external(
    app: &mut AppState,
    file: FileId,
    incarnation: u64,
    word_start: usize,
    source: u64,
    rank: usize,
    items: Option<(Vec<serde_json::Value>, bool)>,
) {
    let text = app_text(app);
    let Some(session) = app.editor.language.completion.as_mut() else {
        return;
    };
    if session.document != file
        || session.incarnation != incarnation
        || session.data.word_start != word_start
    {
        return;
    }
    session.external_pending = session.external_pending.saturating_sub(1);
    let mut data = session.data.clone();
    if let Some((json, incomplete)) = items {
        let list = reply::CompletionList {
            items: json
                .iter()
                .filter_map(|item| {
                    reply::completion_item(
                        item,
                        crate::features::language_service::capabilities::PositionEncoding::Utf16,
                    )
                })
                .collect(),
            incomplete,
        };
        session.external.retain(|(known, ..)| *known != source);
        session.external.push((source, rank, list.clone()));
        // A new answer from this source replaces its earlier items.
        data.entries.retain(|entry| entry.instance != source);
        data.add(source, rank, list);
    }
    let typed = typed_prefix(text, session);
    replace_candidates(session, data, &typed);
    if session.shown.is_empty()
        && session.handle.is_none()
        && !session.data.incomplete
        && session.external_pending == 0
    {
        app.editor.language.completion = None;
    }
}

/// The extension that offered an item completed it (documentation, detail).
pub fn offer_resolved(
    app: &mut AppState,
    source: u64,
    raw: &serde_json::Value,
    item: &serde_json::Value,
) {
    let Some(session) = app.editor.language.completion.as_mut() else {
        return;
    };
    let Some(mut resolved) = reply::completion_item(
        item,
        crate::features::language_service::capabilities::PositionEncoding::Utf16,
    ) else {
        return;
    };
    let Some(entry) = session
        .data
        .entries
        .iter_mut()
        .find(|entry| entry.instance == source && entry.item.raw == *raw)
    else {
        return;
    };
    // Keep the original identity so later lists still match this entry.
    resolved.raw = entry.item.raw.clone();
    entry.item = resolved;
    entry.documentation =
        crate::features::language_service::completion::item_documentation(&entry.item);
    entry.resolved = true;
}

/// How the list is drawn.
pub fn view(session: &Session) -> crate::ui::editor::popup::View {
    use crate::ui::editor::popup::{Row, View};
    let rows: Vec<Row> = session
        .shown
        .iter()
        .map(|shown| {
            let item = &session.data.entries[shown.entry].item;
            Row {
                label: item.label.clone(),
                detail: item
                    .label_detail
                    .clone()
                    .or_else(|| item.detail.clone())
                    .unwrap_or_default()
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
                badge: kind_badge(item.kind),
                matches: shown.positions.clone(),
                faint: item.deprecated,
            }
        })
        .collect();
    let docs = session
        .selected_entry()
        .and_then(|entry| entry.documentation.clone())
        .map(Into::into);
    View {
        panel: false,
        rows: rows.into(),
        selected: session.selected,
        scroll: session.scroll,
        visible: super::popup::VISIBLE,
        query: None,
        title: None,
        empty: String::new(),
        anchor: Some(session.data.word_start),
        docs,
    }
}

/// A short name for a `CompletionItemKind`.
pub fn kind_badge(kind: Option<u32>) -> &'static str {
    match kind {
        Some(1) => "t",
        Some(2) => "m",
        Some(3) => "fn",
        Some(4) => "c",
        Some(5) => "f",
        Some(6) => "v",
        Some(7) => "C",
        Some(8) => "I",
        Some(9) => "{}",
        Some(10) => "p",
        Some(11) => "u",
        Some(12) => "V",
        Some(13) => "E",
        Some(14) => "k",
        Some(15) => "sn",
        Some(17) => "F",
        Some(19) => "D",
        Some(20) => "e",
        Some(21) => "k",
        Some(22) => "S",
        Some(23) => "ev",
        Some(24) => "op",
        Some(25) => "T",
        _ => "",
    }
}

/// Accepts the highlighted item. `then` is a character typed to commit it; it is
/// inserted after the item.
pub fn accept(app: &mut AppState, then: Option<char>) -> bool {
    let Some(session) = app.editor.language.completion.take() else {
        return false;
    };
    let Some(entry) = session.selected_entry().cloned() else {
        return false;
    };
    let pane = session.pane;
    let Some(t) = target(app, pane) else {
        return false;
    };
    if t.view != session.view || t.input_generation != session.input_generation {
        return false;
    }
    // A committing character is already in the text; the item replaces the word
    // before it and the character goes back after.
    let cursor = match then {
        Some(c) => t.cursor.saturating_sub(c.len_utf8()),
        None => t.cursor,
    };
    let plan = insertion(
        &session.data,
        &entry,
        &session.requested,
        &t.text,
        cursor,
        false,
    );
    let ending = app
        .workspace
        .editor(session.pane)
        .map(|e| e.line_ending())
        .unwrap_or("\n");
    let indent = line_indent(&t.text, plan.range.start);
    let mut snippet = plan.snippet.then(|| {
        let resolve = snippet::insertion_variables(
            &t.path,
            t.selection
                .as_ref()
                .map_or("", |range| &t.text[range.clone()]),
            &t.text,
            cursor,
        );
        let mut parsed = snippet::parse(&plan.text, &resolve);
        if !entry.item.as_is {
            snippet::indent_lines(&mut parsed, &indent);
        }
        snippet::normalize_endings(&mut parsed, ending);
        parsed
    });
    let text = snippet.as_ref().map_or_else(
        || loom_editor_core::text::normalize_endings(&plan.text, ending),
        |s| s.text.clone(),
    );
    let mut edits: Vec<Edit> = plan
        .additional
        .iter()
        .map(|(range, new_text)| {
            Edit::replace(
                range.clone(),
                loom_editor_core::text::normalize_endings(new_text, ending),
            )
        })
        .collect();
    edits.push(Edit::replace(plan.range.clone(), text.clone()));
    if let Some(c) = then {
        // Put the committing character back where it was typed.
        edits.push(Edit::replace(cursor..t.cursor, c.to_string()));
    }
    let start = plan.range.start;
    let Some(mut editor) = app.workspace.editor_mut(pane) else {
        return false;
    };
    let mut origin = 0;
    let applied = editor.edit(EditKind::Other, edits, |changes, _| {
        origin = changes.map(start, Assoc::Before);
        let end = origin + text.len();
        Selection::caret(if then.is_some() {
            changes.map(cursor, Assoc::After) + 1
        } else {
            end
        })
    });
    if applied.is_err() {
        return false;
    }
    let version = editor.version();
    drop(editor);
    if let Some(snippet) = snippet.take() {
        snippet_session::begin(app, pane, origin, &snippet, version);
    }
    true
}

/// The whitespace at the start of the line holding `offset`.
fn line_indent(text: &str, offset: usize) -> String {
    let start = text[..offset.min(text.len())]
        .rfind('\n')
        .map_or(0, |at| at + 1);
    text[start..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

pub fn close(app: &mut AppState) {
    close_words(&mut app.editor.language);
    app.editor.language.completion = None;
    app.editor.language.completion_timer = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word_session() -> Session {
        let mut app = AppState::new();
        app.workspace.open_path(
            std::env::temp_dir().join("completion-refresh.rs"),
            "\nalpha0 alpha1 alpha2 alpha3 alpha4 alpha5 alpha6".into(),
        );
        app.workspace.active_editor_mut().unwrap().set_cursor(0);
        words(&mut app);
        app.editor.language.completion.take().unwrap()
    }

    fn candidates(items: serde_json::Value, instance: u64) -> Completion {
        Completion::merge(
            0,
            0,
            vec![(
                instance,
                0,
                reply::completion(
                    &items,
                    crate::features::language_service::capabilities::PositionEncoding::Utf16,
                ),
            )],
        )
    }

    #[test]
    fn replacing_candidates_rebuilds_old_indices_for_every_shorter_list() {
        for count in 0..=7 {
            let mut session = word_session();
            session.set_selected(6);
            let items = (0..count)
                .map(|index| json!({"label": format!("alpha{index}")}))
                .collect::<Vec<_>>();
            replace_candidates(&mut session, candidates(json!(items), 1), "");
            assert_eq!(session.shown.len(), count);
            assert_eq!(view(&session).rows.len(), count);
            assert!(
                session
                    .shown
                    .iter()
                    .all(|shown| shown.entry < session.data.entries.len())
            );
            if count == 0 {
                assert!(session.selected_entry().is_none());
            } else {
                assert!(session.selected_entry().is_some());
                assert_eq!(session.selected, if count == 7 { 6 } else { 0 });
            }
        }
        let mut session = word_session();
        session.set_selected(6);
        replace_candidates(
            &mut session,
            candidates(
                json!([
                    {"label":"alpha9"}, {"label":"alpha6"}, {"label":"alpha8"}
                ]),
                1,
            ),
            "",
        );
        assert_eq!(session.selected_entry().unwrap().item.label, "alpha6");
        assert_eq!(session.selected, 0);
        assert_eq!(session.shown[0].entry, 1);
    }

    #[test]
    fn resolved_candidates_are_reused_only_for_the_same_server_and_payload() {
        let original = json!([{"label":"alpha", "data":1}]);
        let mut session = word_session();
        replace_candidates(&mut session, candidates(original.clone(), 7), "");
        session.data.entries[0].resolved = true;
        session.data.entries[0].item.documentation = Some("old docs".into());
        for (items, instance, reused) in [
            (original.clone(), 7, true),
            (original, 8, false),
            (json!([{"label":"alpha", "data":2}]), 7, false),
        ] {
            let mut refreshed = session.clone();
            replace_candidates(&mut refreshed, candidates(items, instance), "");
            assert_eq!(refreshed.data.entries[0].resolved, reused);
            assert_eq!(
                refreshed.data.entries[0].item.documentation.is_some(),
                reused
            );
        }
    }

    fn extension_item(label: &str, data: &str) -> serde_json::Value {
        json!({
            "label": label,
            "insertText": format!("{label}()"),
            "x-loom": { "extension": "example.tools", "provider": "example.tools.complete", "data": data },
        })
    }

    /// A live session on a document: returns the app and the session's identity.
    fn app_with_session() -> (AppState, FileId, u64, usize) {
        let mut app = AppState::new();
        app.workspace.open_path(
            std::env::temp_dir().join("completion-external.rs"),
            "lo".into(),
        );
        app.workspace.active_editor_mut().unwrap().set_cursor(2);
        let mut session = word_session();
        let file = app.workspace.active_document().unwrap();
        session.document = file;
        session.incarnation = app.workspace.documents[&file].stamp(file).incarnation;
        session.data = Completion {
            word_start: 0,
            cursor: 2,
            ..Completion::default()
        };
        session.external_pending = 1;
        let incarnation = session.incarnation;
        app.editor.language.completion = Some(session);
        (app, file, incarnation, 0)
    }

    #[test]
    fn an_extensions_items_join_the_list_filter_with_typing_and_keep_their_origin() {
        let source = crate::infrastructure::lsp::manager::EXTERNAL_SOURCE | 1;
        let (mut app, file, incarnation, word_start) = app_with_session();
        offer_external(
            &mut app,
            file,
            incarnation,
            word_start,
            source,
            1000,
            Some((
                vec![
                    extension_item("loom_hello", "h"),
                    extension_item("other", "o"),
                ],
                false,
            )),
        );
        let session = app
            .editor
            .language
            .completion
            .as_ref()
            .expect("the list stays open");
        assert_eq!(session.external_pending, 0);
        let shown: Vec<_> = session
            .shown
            .iter()
            .map(|s| session.data.entries[s.entry].item.label.as_str())
            .collect();
        assert_eq!(shown, ["loom_hello"], "filtered by the typed \"lo\"");
        let entry = &session.data.entries[session.shown[0].entry];
        assert_eq!(entry.instance, source);
        assert_eq!(entry.item.plain_insert(), "loom_hello()");
    }

    #[test]
    fn an_answer_for_another_word_or_document_life_is_ignored() {
        let source = crate::infrastructure::lsp::manager::EXTERNAL_SOURCE | 1;
        let (mut app, file, incarnation, _) = app_with_session();
        for (inc, start) in [(incarnation + 1, 0), (incarnation, 5)] {
            offer_external(
                &mut app,
                file,
                inc,
                start,
                source,
                1000,
                Some((vec![extension_item("loom_hello", "h")], false)),
            );
        }
        let session = app.editor.language.completion.as_ref().unwrap();
        assert!(session.data.entries.is_empty());
        assert_eq!(
            session.external_pending, 1,
            "still waiting for the right answer"
        );
    }

    #[test]
    fn a_session_with_nothing_to_show_closes_only_after_every_extension_answered() {
        let source = crate::infrastructure::lsp::manager::EXTERNAL_SOURCE | 1;
        let (mut app, file, incarnation, word_start) = app_with_session();
        app.editor
            .language
            .completion
            .as_mut()
            .unwrap()
            .external_pending = 2;
        offer_external(
            &mut app,
            file,
            incarnation,
            word_start,
            source,
            1000,
            Some((vec![], false)),
        );
        assert!(
            app.editor.language.completion.is_some(),
            "one extension is still working"
        );
        offer_external(
            &mut app,
            file,
            incarnation,
            word_start,
            source | 2,
            1000,
            None,
        );
        assert!(app.editor.language.completion.is_none());
    }

    #[test]
    fn a_new_answer_from_a_source_replaces_its_earlier_items_and_survives_server_merges() {
        let source = crate::infrastructure::lsp::manager::EXTERNAL_SOURCE | 1;
        let (mut app, file, incarnation, word_start) = app_with_session();
        offer_external(
            &mut app,
            file,
            incarnation,
            word_start,
            source,
            1000,
            Some((vec![extension_item("loom_a", "a")], true)),
        );
        offer_external(
            &mut app,
            file,
            incarnation,
            word_start,
            source,
            1000,
            Some((vec![extension_item("loom_b", "b")], false)),
        );
        let session = app.editor.language.completion.as_ref().unwrap();
        let labels: Vec<_> = session
            .data
            .entries
            .iter()
            .map(|e| e.item.label.as_str())
            .collect();
        assert_eq!(labels, ["loom_b"]);
        // The stored lists are what a later server answer is merged with.
        assert_eq!(session.external.len(), 1);
    }

    #[test]
    fn a_resolved_item_updates_only_the_entry_that_matches_its_origin() {
        let source = crate::infrastructure::lsp::manager::EXTERNAL_SOURCE | 1;
        let (mut app, file, incarnation, word_start) = app_with_session();
        let offered = extension_item("loom_hello", "h");
        offer_external(
            &mut app,
            file,
            incarnation,
            word_start,
            source,
            1000,
            Some((vec![offered.clone()], false)),
        );
        let mut done = offered.clone();
        done["documentation"] = json!("docs for h");
        offer_resolved(&mut app, source | 9, &offered, &done);
        assert!(
            !app.editor
                .language
                .completion
                .as_ref()
                .unwrap()
                .data
                .entries[0]
                .resolved,
            "another source"
        );
        offer_resolved(&mut app, source, &offered, &done);
        let entry = &app
            .editor
            .language
            .completion
            .as_ref()
            .unwrap()
            .data
            .entries[0];
        assert!(entry.resolved && entry.documentation.is_some());
        assert_eq!(entry.item.raw, offered, "the original identity is kept");
    }

    #[test]
    fn extension_items_reach_a_live_session_through_the_running_host() {
        use crate::app::extensions::host::ExtensionHost;
        use crate::app::extensions::tests::{language_of, package_dir_with, provider_manifest};
        use crate::app::{
            runtime::AppRuntime,
            test_support::{UiTasks, live},
        };
        use std::time::Instant;

        let path =
            std::env::temp_dir().join(format!("loom-live-completion-{}.txt", std::process::id()));
        let queue = UiTasks::default();
        let runtime = Arc::new(AppRuntime::default());
        runtime.set_application(queue.application());
        let mut app = AppState::new();
        app.workspace.open_path(path.clone(), "lo".into());
        app.workspace.active_editor_mut().unwrap().set_cursor(2);
        let file = app.workspace.active_document().unwrap();
        let mut session = word_session();
        let pane = app.workspace.active_pane();
        let t = target(&app, pane).unwrap();
        session.pane = pane;
        session.document = file;
        session.view = t.view;
        session.input_generation = t.input_generation;
        session.requested = t.text.clone();
        session.last_cursor = t.cursor;
        session.last_version = t.stamp.version;
        session.incarnation = t.stamp.incarnation;
        session.data = Completion {
            word_start: 0,
            cursor: 2,
            ..Completion::default()
        };
        app.editor.language.completion = Some(session);
        let state = live(app).with_runtime(&runtime);

        let host = ExtensionHost::start(
            state.clone(),
            queue.application(),
            std::env::temp_dir().join(format!("loom-live-completion-cache-{}", std::process::id())),
            runtime.trust().clone(),
            None,
        )
        .unwrap();
        host.service().set_clock(|| 0); // cold start time is not under test
        host.load_package(&package_dir_with(
            "live-completion",
            &provider_manifest(&language_of(&path)),
        ))
        .unwrap();
        runtime.install_extensions(host);

        state.update(|app| ask_extensions(&state, app, 0, 2, None));
        assert_eq!(
            state
                .get()
                .editor
                .language
                .completion
                .as_ref()
                .unwrap()
                .external_pending,
            1
        );

        let limit = Instant::now() + Duration::from_secs(60);
        loop {
            queue.run_all();
            let app = state.get();
            let session = app
                .editor
                .language
                .completion
                .as_ref()
                .expect("the list stays open");
            if !session.data.entries.is_empty() {
                let labels: Vec<_> = session
                    .data
                    .entries
                    .iter()
                    .map(|e| e.item.label.clone())
                    .collect();
                assert_eq!(labels, ["loom_hello", "loom_world"]);
                assert_eq!(session.external_pending, 0);
                assert_eq!(session.shown.len(), 2, "both match the typed \"lo\"");
                break;
            }
            assert!(Instant::now() < limit, "the extension never answered");
            std::thread::sleep(Duration::from_millis(10));
        }

        // Resolving the highlighted item goes back to the extension that offered it.
        state.update(|app| resolve_selected(&state, app));
        let limit = Instant::now() + Duration::from_secs(60);
        loop {
            queue.run_all();
            let app = state.get();
            let session = app.editor.language.completion.as_ref().unwrap();
            if session.data.entries.iter().any(|e| e.resolved) {
                let docs = session
                    .data
                    .entries
                    .iter()
                    .find(|e| e.resolved)
                    .and_then(|e| e.item.documentation.clone());
                assert!(docs.is_some(), "the resolved item has documentation");
                break;
            }
            assert!(Instant::now() < limit, "the resolve never came back");
            std::thread::sleep(Duration::from_millis(10));
        }
        runtime.clear_extensions();
    }

    #[test]
    fn background_words_reject_closed_or_changed_requests() {
        use crate::app::{
            runtime::AppRuntime,
            test_support::{UiTasks, live},
        };
        use std::time::Instant;
        for change in 0..3 {
            let mut app = AppState::new();
            app.workspace
                .open_path("words.txt".into(), "alphabet alphanumeric\nal".into());
            app.workspace.active_editor_mut().unwrap().set_cursor(24);
            let queue = UiTasks::default();
            let runtime = Arc::new(AppRuntime::default());
            runtime.set_application(queue.application());
            let state = live(app).with_runtime(&runtime);
            let snapshot = state
                .get()
                .workspace
                .active_editor()
                .unwrap()
                .text_snapshot();
            let owners = Arc::strong_count(&snapshot);
            state.update(|app| schedule_words(&state, app));
            assert_eq!(
                Arc::strong_count(&snapshot),
                owners,
                "debouncing retains only the bounded window"
            );
            let limit = Instant::now() + Duration::from_secs(5);
            while queue.pending_ids().is_empty() {
                assert!(Instant::now() < limit);
                std::thread::yield_now();
            }
            if change == 1 {
                state.update(|app| {
                    app.workspace
                        .active_editor_mut()
                        .unwrap()
                        .replace_selection("x", EditKind::Typing)
                        .unwrap();
                });
            } else if change == 2 {
                state.update(|app| {
                    app.preferences.values_mut().lsp.completion.words_fallback = false;
                    request(&state, app, true, None);
                });
            } else {
                state.update(close);
            }
            queue.run_all();
            assert!(state.get().editor.language.completion.is_none());
        }
    }

    #[test]
    fn local_list_reuses_candidates_without_retaining_source() {
        let mut app = AppState::new();
        app.workspace
            .open_path("words.txt".into(), "alphabet alphanumeric\nal".into());
        app.workspace.active_editor_mut().unwrap().set_cursor(24);
        words(&mut app);
        assert!(
            app.editor
                .language
                .completion
                .as_ref()
                .unwrap()
                .requested
                .is_empty()
        );
        app.workspace
            .active_editor_mut()
            .unwrap()
            .replace_selection("p", EditKind::Typing)
            .unwrap();
        let t = target(&app, app.workspace.active_pane()).unwrap();
        assert!(follow(&mut app, &t));
        assert_eq!(
            app.editor.language.completion.as_ref().unwrap().shown.len(),
            2
        );
        assert!(accept(&mut app, None));
        assert!(
            app.workspace
                .active_editor()
                .unwrap()
                .text()
                .ends_with("alphabet")
        );
    }

    #[test]
    fn local_words_are_bounded_unicode_safe_and_cancellable() {
        let text = format!("distantword {}\nnearbyword ne", "变量 ".repeat(100_000));
        let words = collect_words(&text, text.len(), &|| false).unwrap();
        assert!(words.contains("nearbyword"));
        assert!(!words.contains("distantword"));
        assert!(!words.contains("ne"));
        assert!(collect_words(&text, text.len(), &|| true).is_none());
        assert!(local_start(&"x".repeat(1024 * 1024), 1024 * 1024).is_none());
    }

    #[test]
    fn words_are_identifiers_without_the_numbers() {
        let found: Vec<_> = words_of("let value_1 = 42 + other; 9abc").collect();
        let words: Vec<&str> = found.iter().map(|(_, w)| *w).collect();
        assert_eq!(words, ["let", "value_1", "other"]);
        assert_eq!(found[1].0, 4);
    }

    #[test]
    fn indentation_is_the_leading_whitespace_of_the_line() {
        assert_eq!(line_indent("  \tfoo bar", 6), "  \t");
        assert_eq!(line_indent("a\n    b", 7), "    ");
        assert_eq!(line_indent("x", 0), "");
    }

    #[test]
    fn kinds_have_badges() {
        assert_eq!(kind_badge(Some(3)), "fn");
        assert_eq!(kind_badge(Some(15)), "sn");
        assert_eq!(kind_badge(None), "");
    }
}
