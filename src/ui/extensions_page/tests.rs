use super::*;
use crate::ui::extensions_binding::Source;

fn entry(id: &str, enabled: bool, update: bool) -> Entry {
    use crate::ui::extensions_binding::{Installed, Runtime, Update};
    Entry {
        id: id.to_owned(),
        name: id.to_owned(),
        publisher: "Acme".to_owned(),
        version: "1.0.0".to_owned(),
        tagline: String::new(),
        updated: String::new(),
        icon: "extensions",
        icon_image: None,
        tint: 0,
        categories: vec!["Themes".to_owned()],
        readme_document: None,
        changelog_document: None,
        readme: Vec::new(),
        contributions: Vec::new(),
        changelog: Vec::new(),
        installed: Some(Installed {
            enabled,
            location: String::new(),
            update: update.then(|| Update {
                version: "2.0.0".to_owned(),
                notes: String::new(),
            }),
            error: None,
            warning: None,
            runtime: Runtime {
                summary: String::new(),
                log: Vec::new(),
            },
        }),
        market: None,
    }
}

fn frame() -> Frame {
    Frame {
        installed: vec![
            entry("a.on", true, false),
            entry("b.off", false, false),
            entry("c.update", true, true),
        ],
        marketplace: vec![entry("m.theme", true, false)],
    }
}

fn ids<'a>(entries: &[&'a Entry]) -> Vec<&'a str> {
    entries.iter().map(|entry| entry.id.as_str()).collect()
}

#[test]
fn the_installed_list_follows_the_state_filter_and_the_search() {
    let frame = frame();
    let mut ui = ExtensionsUiState::default();
    assert_eq!(
        ids(&visible(&frame, &ui, "")),
        ["a.on", "b.off", "c.update"]
    );
    ui.filter = StateFilter::Disabled;
    assert_eq!(ids(&visible(&frame, &ui, "")), ["b.off"]);
    ui.filter = StateFilter::All;
    assert_eq!(ids(&visible(&frame, &ui, "UPDATE")), ["c.update"]);
}

#[test]
fn updates_list_only_what_has_one() {
    let frame = frame();
    let ui = ExtensionsUiState {
        source: Source::Updates,
        ..Default::default()
    };
    assert_eq!(ids(&visible(&frame, &ui, "")), ["c.update"]);
}

#[test]
fn the_marketplace_filters_by_category() {
    let frame = frame();
    let mut ui = ExtensionsUiState {
        source: Source::Marketplace,
        ..Default::default()
    };
    assert_eq!(ids(&visible(&frame, &ui, "")), ["m.theme"]);
    ui.category = Some("Linters");
    assert!(visible(&frame, &ui, "").is_empty());
}

#[test]
fn the_detail_falls_back_to_the_first_result() {
    let frame = frame();
    let ui = ExtensionsUiState::default();
    let entries = visible(&frame, &ui, "");
    assert_eq!(current(&entries, Some("b.off")).unwrap().id, "b.off");
    assert_eq!(current(&entries, Some("gone")).unwrap().id, "a.on");
    assert_eq!(current(&entries, None).unwrap().id, "a.on");
    assert!(current(&[], Some("a.on")).is_none());
}
