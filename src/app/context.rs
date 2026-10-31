//! Explicit application command context. Reactive values contain no service handles.
use super::{runtime::AppRuntime, state::AppState};
use std::{
    ops::Deref,
    sync::{Arc, Weak},
};
#[derive(Clone)]
pub(crate) struct AppContext {
    model: lgui::prelude::State<AppState>,
    runtime: Weak<AppRuntime>,
}
impl AppContext {
    pub(crate) fn detached(model: lgui::prelude::State<AppState>) -> Self {
        Self {
            model,
            runtime: Weak::new(),
        }
    }
    pub(crate) fn with_runtime(mut self, runtime: &Arc<AppRuntime>) -> Self {
        self.runtime = Arc::downgrade(runtime);
        self
    }
    fn invalidate_cancelled(&self, before: Option<u64>, app: &AppState) {
        if before.is_some() && app.editor.pending_file.is_none() {
            if let Some(loader) = self.services().and_then(|r| r.file_loader()) {
                loader.invalidate();
            }
        }
    }
    /// Tells the language servers about document changes this update made.
    fn sync_document_services(&self, app: &AppState) {
        // Trust is judged against the folders that are open, before anything asks it.
        if let Some(runtime) = self.services() {
            runtime.trust().set_workspace(app.explorer.model.roots());
        }
        if let Some(services) = self.services().and_then(|r| r.language_services()) {
            services.sync(app);
        }
        if let Some(markdown) = self.services().and_then(|r| r.markdown()) {
            markdown.sync(app);
        }
        if let Some(extensions) = self.services().and_then(|r| r.extensions()) {
            extensions.observe(app);
        }
    }
    pub(crate) fn update(&self, change: impl FnOnce(&mut AppState)) {
        self.model.update(|app| {
            let before = app.editor.pending_file.as_ref().map(|p| p.generation);
            change(app);
            super::markdown_view::sync_sources(app);
            app.editor
                .markdown_views
                .retain(|id, _| app.workspace.views.contains_key(id));
            super::image_preview::cleanup(app);
            self.invalidate_cancelled(before, app);
            self.sync_document_services(app);
            super::language_ui::observe(self, app);
        });
    }
    pub(crate) fn try_update(&self, change: impl FnOnce(&mut AppState) -> bool) -> bool {
        self.model.try_update(|app| {
            let before = app.editor.pending_file.as_ref().map(|p| p.generation);
            let changed = change(app);
            super::image_preview::cleanup(app);
            if changed {
                super::markdown_view::sync_sources(app);
            }
            self.invalidate_cancelled(before, app);
            self.sync_document_services(app);
            super::language_ui::observe(self, app);
            changed
        })
    }
    pub(crate) fn model(&self) -> lgui::prelude::State<AppState> {
        self.model.clone()
    }
    pub(crate) fn services(&self) -> Option<Arc<AppRuntime>> {
        self.runtime.upgrade()
    }
}
impl Deref for AppContext {
    type Target = lgui::prelude::State<AppState>;
    fn deref(&self) -> &Self::Target {
        &self.model
    }
}
