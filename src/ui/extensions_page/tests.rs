use super::*;

fn entry(id: &str, enabled: bool, update: bool) -> Entry {
    use crate::ui::extensions_binding::{Installed, Update};
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
            bundled: false,
            pinned: false,
            previous: None,
            location: String::new(),
            update: update.then(|| Update {
                version: "2.0.0".to_owned(),
                notes: String::new(),
            }),
            error: None,
            warning: None,
        }),
    }
}

fn frame() -> Frame {
    Frame {
        installed: vec![
            entry("a.on", true, false),
            entry("b.off", false, false),
            entry("c.update", true, true),
        ],
        available: vec![{
            let mut available = entry("d.new", true, false);
            available.installed = None;
            available
        }],
        ..Default::default()
    }
}

fn ids<'a>(entries: &[&'a Entry]) -> Vec<&'a str> {
    entries.iter().map(|entry| entry.id.as_str()).collect()
}

#[test]
fn the_list_shows_what_is_installed_and_a_search_adds_what_the_registry_offers() {
    let frame = frame();
    assert_eq!(ids(&visible(&frame, "")), ["a.on", "b.off", "c.update"]);
    assert_eq!(ids(&visible(&frame, "UPDATE")), ["c.update"]);
    // Installed matches come first, then the registry's extensions.
    assert_eq!(ids(&visible(&frame, ".")), ["a.on", "b.off", "c.update", "d.new"]);
    assert_eq!(ids(&visible(&frame, "d.new")), ["d.new"]);
    assert!(visible(&frame, "nothing like this").is_empty());
}

#[test]
fn the_detail_falls_back_to_the_first_result() {
    let frame = frame();
    let entries = visible(&frame, "");
    assert_eq!(current(&entries, Some("b.off")).unwrap().id, "b.off");
    assert_eq!(current(&entries, Some("gone")).unwrap().id, "a.on");
    assert_eq!(current(&entries, None).unwrap().id, "a.on");
    assert!(current(&[], Some("a.on")).is_none());
}
