//! Generic search consumers. Providers scan their own storage; immutable dirty
//! buffers stay here and are merged only after provider eligibility checks.
use super::{context::AppContext, state::AppState};
use crate::features::search::{self, FileMatches, Matcher, ProviderLabels, Results, SearchSpec};
use loog_workspace_api::{
    Answer, Capability, Operation, PresentedRoot, ResourceRef, SearchOptions, SearchPage,
};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub(super) struct Request {
    pub state: AppContext,
    pub roots: Vec<PresentedRoot>,
    pub buffers: HashMap<ResourceRef, Arc<String>>,
}
impl Request {
    pub(super) fn capture(state: &AppContext, app: &AppState) -> Self {
        Self {
            state: state.clone(),
            roots: app
                .explorer
                .resources
                .roots
                .iter()
                .map(|r| r.descriptor.clone())
                .collect(),
            buffers: app
                .workspace
                .documents
                .values()
                .filter(|d| d.is_dirty() && d.meta.resource.environment().is_some())
                .map(|d| (d.meta.resource.clone(), d.buffer.text_snapshot()))
                .collect(),
        }
    }
}

pub(super) fn run(
    request: Request,
    spec: &SearchSpec,
    cancel: &AtomicBool,
    mut results: Results,
    mut publish: impl FnMut(Results),
) -> Result<(), String> {
    let matcher = Matcher::new(spec)?;
    let mut seen = results
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect::<HashSet<_>>();
    let mut bytes = results
        .files
        .iter()
        .flat_map(|f| &f.matches)
        .map(|m| m.preview.len() + 96)
        .sum();
    for root in request.roots {
        if results.limit_hit || cancel.load(Ordering::Relaxed) {
            break;
        }
        let mut accumulated = Results::default();
        let outcome = scan(
            &request.state,
            &root,
            spec,
            &matcher,
            &request.buffers,
            cancel,
            &mut accumulated,
            &mut seen,
            &mut bytes,
            results.matches,
            |partial| {
                let mut combined = results.clone();
                merge(&mut combined, partial);
                publish(combined);
            },
        );
        merge(&mut results, accumulated);
        if let Err(error) = outcome {
            if !cancel.load(Ordering::Relaxed) {
                results.errors += 1;
                results
                    .first_error
                    .get_or_insert(format!("{}: {error}", root.name));
            }
        }
    }
    results.files.sort_by(|a, b| a.path.cmp(&b.path));
    results.cancelled = cancel.load(Ordering::Relaxed);
    results.complete = true;
    publish(results);
    Ok(())
}

fn merge(results: &mut Results, part: Results) {
    results.files.extend(part.files);
    results.matches += part.matches;
    results.searched += part.searched;
    results.skipped += part.skipped;
    results.skipped_binary += part.skipped_binary;
    results.skipped_encoding += part.skipped_encoding;
    results.skipped_large += part.skipped_large;
    results.errors += part.errors;
    if results.first_error.is_none() {
        results.first_error = part.first_error;
    }
    results.limit_hit |= part.limit_hit;
    results.complete = false;
}

fn scan(
    state: &AppContext,
    root: &PresentedRoot,
    spec: &SearchSpec,
    matcher: &Matcher,
    buffers: &HashMap<ResourceRef, Arc<String>>,
    cancel: &AtomicBool,
    results: &mut Results,
    seen: &mut HashSet<ResourceRef>,
    bytes: &mut usize,
    prior_matches: usize,
    mut publish: impl FnMut(Results),
) -> Result<(), String> {
    if !root.capabilities.contains(&Capability::Search) {
        return Err("Content search is not supported by this provider".into());
    }
    let ResourceRef::Provided { environment, path } = &root.resource else {
        return Err("Invalid search root".into());
    };
    let buffers: HashMap<_, _> = buffers
        .iter()
        .filter(|(key, _)| key.environment() == Some(environment))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    if buffers.len() > 128 {
        return Err("Search supports at most 128 unsaved buffers per environment".into());
    }
    let cancelled = || cancel.load(Ordering::Relaxed);
    let (encoding, settings_error) = super::resource_project::background_encoding(
        state,
        root.scope,
        &root.resource,
        &cancelled,
    )?;
    let options = SearchOptions {
        spec: spec.clone(),
        encoding,
        buffers: buffers
            .keys()
            .filter_map(|key| {
                if let ResourceRef::Provided { path, .. } = key {
                    Some(path.clone())
                } else {
                    None
                }
            })
            .collect(),
    };
    let handle = super::resource_bytes::background_typed(
        state,
        root.scope,
        &root.resource,
        &cancelled,
        |request, _| match request(Operation::Search {
            path: path.clone(),
            options,
        })
        .map_err(|e| e.message)?
        {
            Answer::Started { handle } => Ok(handle),
            _ => Err("Invalid search start response".into()),
        },
    )?;
    let started = Instant::now();
    let outcome = (|| {
        let mut indices = HashMap::new();
        let mut merged_buffers = HashSet::new();
        let mut buffer_searched = 0;
        let mut buffer_binary = 0;
        let mut buffer_large = 0;
        let mut previous = loog_workspace_api::SearchProgress::default();
        loop {
            if cancelled() {
                return Err("Search cancelled".into());
            }
            if started.elapsed() > Duration::from_secs(150) {
                return Err("Search exceeded its time budget".into());
            }
            let page = super::resource_bytes::background_typed(
                state,
                root.scope,
                &root.resource,
                &cancelled,
                |request, _| match request(Operation::SearchNext {
                    search: handle,
                    limit: 32,
                })
                .map_err(|e| e.message)?
                {
                    Answer::SearchMatches { page } => Ok(page),
                    _ => Err("Invalid search page response".into()),
                },
            )?;
            validate(&page, &previous)?;
            let empty = page.files.is_empty();
            let complete = page.complete;
            previous = page.progress.clone();
            // Provider counters describe disk work. Dirty-buffer counts are
            // added below and retained separately from cumulative disk counters.
            results.searched = previous.searched + buffer_searched;
            results.skipped_binary = previous.skipped_binary + buffer_binary;
            results.skipped_encoding = previous.skipped_encoding;
            results.skipped_large = previous.skipped_large + buffer_large;
            results.skipped =
                previous.skipped_binary + previous.skipped_encoding + previous.skipped_large;
            results.errors = previous.errors + usize::from(settings_error.is_some());
            results.first_error = settings_error
                .clone()
                .or_else(|| previous.first_error.clone());
            results.limit_hit |= previous.limit_hit && complete;
            for mut file in page.files {
                let identity = ResourceRef::Provided {
                    environment: environment.clone(),
                    path: file.path.clone(),
                };
                if file.buffer {
                    let text = buffers
                        .get(&identity)
                        .ok_or("Provider returned an unknown unsaved buffer")?;
                    if !merged_buffers.insert(identity.clone()) {
                        return Err("Provider repeated an unsaved buffer".into());
                    }
                    if text.len() > search::MAX_FILE_BYTES {
                        buffer_large += 1;
                        results.skipped_large += 1;
                        continue;
                    }
                    if text.as_bytes().contains(&0) {
                        buffer_binary += 1;
                        results.skipped_binary += 1;
                        continue;
                    }
                    file.matches = matcher.matches(
                        text,
                        cancel,
                        search::MAX_MATCHES.saturating_sub(prior_matches + results.matches) + 1,
                    );
                    file.content_hash = search::hash(text);
                    results.searched += 1;
                    buffer_searched += 1;
                }
                if file.matches.is_empty() {
                    continue;
                }
                // Distinct roots in one environment may overlap. A result is
                // owned by the first root, while chunks from that root merge.
                if seen.contains(&identity) && !indices.contains_key(&identity) {
                    continue;
                }
                let remaining = search::MAX_MATCHES.saturating_sub(prior_matches + results.matches);
                if file.matches.len() > remaining {
                    file.matches.truncate(remaining);
                    results.limit_hit = true;
                }
                let size = file
                    .matches
                    .iter()
                    .map(|m| m.preview.len() + 96)
                    .sum::<usize>()
                    + file.path.as_str().len()
                    + file.relative.iter().map(String::len).sum::<usize>();
                if *bytes + size > 16 * 1024 * 1024
                    || seen.len() >= search::service::MAX_FILES && !indices.contains_key(&identity)
                {
                    results.limit_hit = true;
                    break;
                }
                *bytes += size;
                results.matches += file.matches.len();
                if let Some(index) = indices.get(&identity).copied() {
                    let stored: &mut Arc<FileMatches> = &mut results.files[index];
                    if stored.content_hash != file.content_hash {
                        return Err("Search file changed between chunks".into());
                    }
                    Arc::make_mut(stored).matches.extend(file.matches);
                } else {
                    indices.insert(identity.clone(), results.files.len());
                    seen.insert(identity.clone());
                    results.files.push(Arc::new(FileMatches {
                        path: identity,
                        root: root.resource.clone(),
                        provider: Some(ProviderLabels {
                            name: file.name,
                            relative: file.relative,
                            root_name: root.name.clone(),
                        }),
                        matches: file.matches,
                        content_hash: file.content_hash,
                    }));
                }
            }
            results.skipped =
                results.skipped_binary + results.skipped_encoding + results.skipped_large;
            publish(results.clone());
            if complete || results.limit_hit {
                return Ok(());
            }
            if empty {
                std::thread::sleep(Duration::from_millis(80));
            }
        }
    })();
    // Cleanup is independent of the cancelled query. The owner route also
    // releases this handle on unload/disconnect; never join the worker here.
    if let Some(host) = state.services().and_then(|runtime| runtime.extensions()) {
        let _ = host.request_resource(
            root.scope,
            Operation::SearchClose { search: handle },
            Box::new(|_, _| {}),
        );
    }
    outcome
}

fn validate(
    page: &SearchPage,
    previous: &loog_workspace_api::SearchProgress,
) -> Result<(), String> {
    let p = &page.progress;
    if page.files.len() > 32
        || p.searched < previous.searched
        || p.searched > 1_000_000
        || p.errors < previous.errors
        || p.errors > 1_000_000
        || [p.skipped_binary, p.skipped_encoding, p.skipped_large]
            .iter()
            .any(|n| *n > 1_000_000)
        || p.first_error.as_ref().is_some_and(|s| s.len() > 4096)
    {
        return Err("Search publication exceeds its counter or batch limits".into());
    }
    for file in &page.files {
        if file.relative.is_empty()
            || file.relative.len() > 128
            || file.name.is_empty()
            || file.relative.last() != Some(&file.name)
            || file.relative.iter().map(String::len).sum::<usize>() > 32 * 1024
            || file.relative.iter().any(|p| {
                p.is_empty() || matches!(p.as_str(), "." | "..") || p.contains(['/', '\0'])
            })
            || file.matches.len() > 32
            || file.buffer && !file.matches.is_empty()
        {
            return Err("Invalid search file publication".into());
        }
        for found in &file.matches {
            if found.range.start > found.range.end
                || found.range.end > search::MAX_FILE_BYTES
                || found.preview.len() > loog_text_core::search::MAX_PREVIEW_BYTES + 6
                || found.highlight.start > found.highlight.end
                || found.highlight.end > found.preview.len()
                || !found.preview.is_char_boundary(found.highlight.start)
                || !found.preview.is_char_boundary(found.highlight.end)
                || found.line > search::MAX_FILE_BYTES
                || found.column > search::MAX_FILE_BYTES
            {
                return Err("Invalid search match offsets or preview".into());
            }
        }
    }
    Ok(())
}

#[derive(Clone)]
struct Navigation {
    intent: search::model::NavigationIntent,
    pane: crate::domain::identity::PaneId,
    active: Option<crate::domain::identity::ViewId>,
    input: Option<u64>,
}
impl Navigation {
    fn valid(&self, app: &AppState) -> bool {
        self.intent.key.same_navigation(&super::search::key(app))
            && app
                .search
                .model
                .status()
                .applied_key
                .as_ref()
                .is_some_and(|key| key.same_navigation(&self.intent.key))
            && app.search.ui.selected == Some((self.intent.file.path.clone(), self.intent.index))
            && app.workspace.active_pane() == self.pane
            && app.workspace.active() == self.active
            && self
                .active
                .and_then(|id| app.workspace.view(id))
                .map(|v| v.input_generation)
                == self.input
    }
}

pub(super) fn navigate(
    state: &AppContext,
    application: lgui::ApplicationHandle,
    rect: lgui::prelude::UiRect,
    focus: lgui::core::UiFocusHandle,
    intent: search::model::NavigationIntent,
) {
    let app = state.get();
    let navigation = Navigation {
        pane: app.workspace.active_pane(),
        active: app.workspace.active(),
        input: app
            .workspace
            .active()
            .and_then(|id| app.workspace.view(id))
            .map(|v| v.input_generation),
        intent,
    };
    let existing = app
        .workspace
        .file_id_for_resource(&navigation.intent.file.path)
        .and_then(|id| {
            app.workspace
                .documents
                .get(&id)
                .filter(|doc| doc.is_text())
                .map(|doc| (doc.stamp(id), doc.buffer.text_snapshot()))
        });
    state.update(|app| {
        app.search.ui.selected =
            Some((navigation.intent.file.path.clone(), navigation.intent.index))
    });
    if let Some((stamp, text)) = existing {
        locate(
            state.clone(),
            application,
            rect,
            focus,
            navigation,
            Some(stamp),
            text,
            None,
        );
    } else {
        let guard = navigation.clone();
        let context = state.clone();
        super::resources::read_text(
            state,
            navigation.intent.file.path.clone(),
            search::MAX_FILE_BYTES,
            super::resources::TextConsumer {
                valid: Box::new(move |app| guard.valid(app)),
                ready: Box::new(move |app, result| match result {
                    Ok(prepared) => locate(
                        context,
                        application,
                        rect,
                        focus,
                        navigation,
                        None,
                        prepared.text_snapshot(),
                        Some(prepared),
                    ),
                    Err(error) => app.show_error(format!("Could not open search result: {error}")),
                }),
            },
        );
    }
}

fn locate(
    state: AppContext,
    application: lgui::ApplicationHandle,
    rect: lgui::prelude::UiRect,
    focus: lgui::core::UiFocusHandle,
    navigation: Navigation,
    stamp: Option<crate::domain::document::DocumentStamp>,
    text: Arc<String>,
    prepared: Option<crate::domain::workspace::PreparedTextFile>,
) {
    let context = state.clone();
    let failed = application.clone();
    let spawned = std::thread::Builder::new()
        .name("loog-resource-search-open".into())
        .spawn(move || {
            let intent = &navigation.intent;
            let located = Matcher::new(&intent.spec).ok().and_then(|matcher| {
                matcher.locate(
                    &text,
                    intent.file.content_hash,
                    &intent.file.matches[intent.index],
                )
            });
            application.post(move || {
                let mut focused = false;
                context.try_update(|app| {
                    if !navigation.valid(app) {
                        return false;
                    }
                    let current = app
                        .workspace
                        .file_id_for_resource(&navigation.intent.file.path)
                        .and_then(|id| app.workspace.documents.get(&id).map(|doc| doc.stamp(id)));
                    if current != stamp {
                        app.search.ui.refresh += 1;
                        return true;
                    }
                    let Some(range) = located else {
                        app.search.ui.refresh += 1;
                        return true;
                    };
                    let mode = if navigation.intent.permanent {
                        super::workspace::OpenMode::Permanent
                    } else {
                        super::workspace::OpenMode::Preview
                    };
                    if let Some(prepared) = prepared {
                        app.workspace.open_prepared_text(prepared, mode);
                    } else if let Some(stamp) = stamp {
                        app.workspace.attach_text_tab(stamp.document, mode);
                    }
                    if let Some(mut editor) = app.workspace.active_editor_mut() {
                        editor.select_range(range);
                    }
                    super::editor::reveal_cursor(app, rect);
                    app.editor.drag = None;
                    app.editor.ime = None;
                    focused = navigation.intent.focus_editor;
                    true
                });
                if focused {
                    focus.focus();
                }
            });
        });
    if let Err(error) = spawned {
        failed.post(move || {
            state.update(|app| app.show_error(format!("Could not locate search result: {error}")))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn search_navigation_survives_unrelated_refresh_but_rejects_query_scope_and_view_changes() {
        let mut app = AppState::new();
        app.search.ui.query.set_text("needle");
        let key = super::super::search::key(&app);
        let results = crate::features::search::tests::result("needle");
        let file = results.files[0].clone();
        app.search.model.begin(key.clone());
        app.search.model.fixture_status().results = Arc::new(results);
        let intent = app
            .search
            .model
            .navigation(&key, file.clone(), 0, false, false)
            .unwrap();
        app.search.ui.selected = Some((file.path.clone(), 0));
        let navigation = Navigation {
            pane: app.workspace.active_pane(),
            active: app.workspace.active(),
            input: app
                .workspace
                .active()
                .and_then(|id| app.workspace.view(id))
                .map(|view| view.input_generation),
            intent,
        };
        assert!(navigation.valid(&app));
        app.search.ui.refresh += 1;
        assert!(navigation.valid(&app));
        app.search.model.begin(super::super::search::key(&app));
        assert!(
            navigation.valid(&app),
            "a new scan of the same query must not cancel an explicit navigation"
        );
        app.search.ui.query.set_text("different");
        assert!(!navigation.valid(&app));
        app.search.ui.query.set_text("needle");
        app.search
            .model
            .fixture_status()
            .applied_key
            .as_mut()
            .unwrap()
            .scopes
            .push(42);
        assert!(!navigation.valid(&app));
        app.search.model.fixture_status().applied_key = Some(key);
        app.search.ui.selected = None;
        assert!(!navigation.valid(&app));
    }
}
