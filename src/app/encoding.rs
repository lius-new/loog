//! Status bar encoding selection updates the editor and never writes to disk.
use crate::app::context::AppContext;
use crate::domain::{encoding::TextFormat, identity::FileId};
use std::path::Path;
#[derive(Clone)]
pub struct Picker {
    pub document: FileId,
    pub suggested: Option<TextFormat>,
    pub formats: Vec<TextFormat>,
    pub selected: usize,
    pub hovered: Option<usize>,
}
pub fn open(state: &AppContext) {
    state.update(|app| {
        if let Some(id) = app.workspace.active_document() {
            if let Some(doc) = app.workspace.documents.get(&id).filter(|d| d.is_text()) {
                let options = doc
                    .meta
                    .provider_root
                    .as_ref()
                    .map_or_else(
                        || app.settings_at(doc.meta.local_path()),
                        |root| super::resource_project::settings(app, root),
                    )
                    .encoding;
                let bytes = doc
                    .encoding_bytes
                    .clone()
                    .or_else(|| doc.disk_bytes.clone())
                    .or_else(|| {
                        crate::features::encoding::encode(&doc.saved_text, doc.saved_format)
                            .ok()
                            .map(std::sync::Arc::new)
                    });
                let suggested = bytes
                    .as_ref()
                    .and_then(|bytes| crate::features::encoding::suggest(bytes, &options));
                let mut formats = TextFormat::choices();
                if let Some(index) =
                    suggested.and_then(|format| formats.iter().position(|f| *f == format))
                {
                    let format = formats.remove(index);
                    formats.insert(0, format);
                }
                let selected = formats.iter().position(|f| *f == doc.format()).unwrap_or(0);
                app.chrome.encoding = Some(Picker {
                    document: id,
                    suggested,
                    formats,
                    selected,
                    hovered: None,
                });
            }
        }
    });
}
pub fn select(state: &AppContext, format: TextFormat) {
    state.update(|app| {
        let Some(picker) = app.chrome.encoding.as_ref() else {
            return;
        };
        let id = picker.document;
        match app.workspace.set_encoding(id, format) {
            Ok(changed) => {
                if changed {
                    app.workspace.promote_active_preview();
                }
                app.chrome.encoding = None;
            }
            Err(error) => app.show_error(error),
        }
    });
}
pub fn key(state: &AppContext, event: &lgui::core::KeyboardEvent) -> bool {
    use lgui::core::{LogicalKey, NamedKey as Key};
    let LogicalKey::Named(key) = &event.key else {
        return false;
    };
    match key {
        Key::Escape => {
            state.update(|app| app.chrome.encoding = None);
            true
        }
        Key::ArrowDown | Key::ArrowUp => {
            state.update(|app| {
                if let Some(p) = app.chrome.encoding.as_mut() {
                    p.selected = if *key == Key::ArrowDown {
                        (p.selected + 1).min(p.formats.len().saturating_sub(1))
                    } else {
                        p.selected.saturating_sub(1)
                    };
                    p.hovered = Some(p.selected);
                }
            });
            true
        }
        Key::Enter => {
            let app = state.get();
            if let Some(format) = app
                .chrome
                .encoding
                .as_ref()
                .and_then(|picker| picker.formats.get(picker.selected).copied())
            {
                select(state, format);
            }
            true
        }
        _ => false,
    }
}

pub(crate) fn prepare(path: &Path) -> Result<crate::app::workspace::PreparedTextFile, String> {
    if crate::domain::media::is_raster(path) {
        let (image, disk) = crate::infrastructure::image_filesystem::load(path)
            .map_err(|e| format!("Could not open {}: {e}", path.display()))?;
        return Ok(crate::app::workspace::PreparedTextFile::image(
            path.to_path_buf(),
            image,
            disk,
        ));
    }
    let (decoded, state) = crate::infrastructure::document_filesystem::read_encoded(
        path,
        None,
        crate::infrastructure::document_filesystem::MAX_EDITABLE_FILE_BYTES,
    )
    .map_err(|e| format!("Could not open {}: {e}", path.display()))?;
    Ok(
        crate::app::workspace::PreparedTextFile::with_encoding(path.to_path_buf(), decoded, state)
            .with_file_options(crate::infrastructure::editorconfig::load(path)),
    )
}

pub fn binding(state: &AppContext) -> crate::ui::chrome::encoding_binding::Binding {
    use crate::ui::chrome::encoding_binding::{Binding, Command, Frame};
    let reader = state.clone();
    let writer = state.clone();
    Binding::new(
        move || {
            let app = reader.get();
            let picker = app.chrome.encoding.as_ref()?;
            Some(Frame {
                selected: picker.selected,
                hovered: picker.hovered,
                suggested: picker.suggested,
                current: app
                    .workspace
                    .documents
                    .get(&picker.document)
                    .map(|d| d.format()),
                formats: picker.formats.clone(),
            })
        },
        move |command| match command {
            Command::Close => writer.update(|a| a.chrome.encoding = None),
            Command::Select(f) => select(&writer, f),
            Command::Scroll(down) => writer.update(|a| {
                if let Some(p) = a.chrome.encoding.as_mut() {
                    p.selected = if down {
                        (p.selected + 1).min(p.formats.len().saturating_sub(1))
                    } else {
                        p.selected.saturating_sub(1)
                    };
                    p.hovered = Some(p.selected);
                }
            }),
            Command::Hover(hovered) => {
                writer.try_update(|app| {
                    let Some(picker) = app.chrome.encoding.as_mut() else {
                        return false;
                    };
                    if picker.hovered == hovered {
                        return false;
                    }
                    picker.hovered = hovered;
                    if let Some(index) = hovered {
                        picker.selected = index;
                    }
                    true
                });
            }
        },
    )
}
