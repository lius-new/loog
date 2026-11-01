//! Project installed packages into the values presented by the extensions page.
use crate::app::extensions::{ExtensionService, Package};
use crate::features::extensions::lifecycle::State;
use crate::ui::extensions_binding::{
    Contribution, Documentation, Entry, Frame, IconImage, Installed, Notice, Table,
};

pub(crate) fn frame(service: &ExtensionService) -> Frame {
    let mut installed: Vec<_> = service.packages().map(entry).collect();
    installed.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.id.cmp(&b.id))
    });
    Frame {
        installed,
        ..Default::default()
    }
}

fn entry(package: &Package) -> Entry {
    let m = &package.manifest;
    let c = &m.contributes;
    let mut contributions = Vec::new();
    let mut table = |title, icon, head, rows: Vec<[String; 3]>| {
        if !rows.is_empty() {
            contributions.push(Contribution::Table(Table {
                title,
                icon,
                head,
                total: rows.len(),
                mono: [true, false, false],
                rows,
                more: 0,
            }));
        }
    };
    table(
        "Commands",
        "command",
        ["Identifier", "Title", ""],
        c.commands
            .iter()
            .map(|command| [command.id.clone(), command.title.clone(), String::new()])
            .collect(),
    );
    for (title, resources) in [
        ("Themes", &c.themes),
        ("Languages", &c.languages),
        ("Snippets", &c.snippets),
    ] {
        table(
            title,
            "extensions",
            ["Identifier", "File", "Status"],
            resources
                .iter()
                .map(|resource| {
                    let status = package
                        .resources
                        .rejected
                        .iter()
                        .find(|(id, _)| id == &resource.id)
                        .map(|(_, error)| error.clone())
                        .unwrap_or_else(|| "Loaded".to_owned());
                    [resource.id.clone(), resource.path.clone(), status]
                })
                .collect(),
        );
    }
    table(
        "Queries",
        "extensions",
        ["Identifier", "Grammar and file", "File"],
        c.queries
            .iter()
            .map(|query| {
                [
                    query.id.clone(),
                    format!("{}/{}", query.grammar, query.file),
                    query.path.clone(),
                ]
            })
            .collect(),
    );
    table(
        "Language features",
        "extension-code",
        ["Identifier", "Feature", "Languages"],
        c.providers
            .iter()
            .map(|provider| {
                [
                    provider.id.clone(),
                    format!("{:?}", provider.kind),
                    provider.selector.languages.join(", "),
                ]
            })
            .collect(),
    );
    table(
        "Language servers",
        "extension-code",
        ["Identifier", "Command", "Languages"],
        c.language_servers
            .iter()
            .map(|server| {
                [
                    server.id.clone(),
                    server.command.clone(),
                    server.languages.join(", "),
                ]
            })
            .collect(),
    );
    table(
        "Settings",
        "extensions",
        ["Key", "Type", "Default"],
        c.settings
            .iter()
            .map(|setting| {
                [
                    setting.key.clone(),
                    format!("{:?}", setting.kind),
                    setting.default.to_string(),
                ]
            })
            .collect(),
    );
    let state = package.lifecycle.state();
    let warning = (!package.resources.rejected.is_empty()).then(|| Notice {
        title: "Some resources could not load".to_owned(),
        body: package
            .resources
            .rejected
            .iter()
            .map(|(id, error)| format!("{id}: {error}"))
            .collect::<Vec<_>>()
            .join("\n"),
    });
    Entry {
        id: m.id.clone(),
        name: m.name.clone().unwrap_or_else(|| m.id.clone()),
        publisher: m.publisher.clone().unwrap_or_default(),
        version: m.version.clone(),
        tagline: m.description.clone().unwrap_or_default(),
        updated: String::new(),
        icon: if c.themes.is_empty() {
            "extension-code"
        } else {
            "extension-theme"
        },
        tint: 0x38bdf8,
        icon_image: package.icon.as_ref().map(|image| IconImage {
            identity: image.identity,
            bytes: image.frames[0].bytes.clone(),
        }),
        categories: m.categories.clone(),
        readme: Vec::new(),
        readme_document: package
            .readme
            .as_ref()
            .zip(m.readme.as_ref())
            .map(|(content, path)| Documentation {
                content: content.clone(),
                path: package.dir.join(path),
            }),
        contributions,
        changelog: Vec::new(),
        changelog_document: package.changelog.as_ref().zip(m.changelog.as_ref()).map(
            |(content, path)| Documentation {
                content: content.clone(),
                path: package.dir.join(path),
            },
        ),
        installed: Some(Installed {
            enabled: !matches!(state, State::Stopped | State::Stopping),
            bundled: package.is_bundled(),
            // The host fills these from what is recorded on disk.
            pinned: false,
            previous: None,
            location: package.dir.display().to_string(),
            update: None,
            error: (state == State::Failed).then(|| Notice {
                title: "Could not start extension".to_owned(),
                body: package
                    .last_error
                    .clone()
                    .unwrap_or_else(|| format!("Failed at {:?}", package.lifecycle.failure())),
            }),
            warning,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabling_hides_the_themes_and_enabling_brings_them_back() {
        let mut service = ExtensionService::new(std::env::temp_dir().join("loom-disable-cache"));
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/extensions/packages");
        service.load_package(&root.join("loom-midnight")).unwrap();
        let themes = service.themes().len();
        assert!(themes > 0);
        service.disable("loom.midnight").unwrap();
        assert!(service.themes().is_empty());
        assert!(
            !super::frame(&service).installed[0]
                .installed
                .as_ref()
                .unwrap()
                .enabled
        );
        service.enable("loom.midnight").unwrap();
        assert_eq!(service.themes().len(), themes);
        assert!(
            super::frame(&service).installed[0]
                .installed
                .as_ref()
                .unwrap()
                .enabled
        );
        let (dir, _) = service.uninstall("loom.midnight").unwrap();
        assert!(dir.ends_with("loom-midnight"));
        assert!(service.package("loom.midnight").is_none());
        assert!(service.themes().is_empty());
    }

    #[test]
    fn installed_details_come_from_the_packages_and_their_documents() {
        let mut service = ExtensionService::new(std::env::temp_dir().join("loom-details-cache"));
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/extensions/packages");
        service.load_package(&root.join("loom-midnight")).unwrap();
        service.load_package(&root.join("loom-properties")).unwrap();
        let frame = frame(&service);
        assert_eq!(frame.installed.len(), 2);
        let entry = frame
            .installed
            .iter()
            .find(|entry| entry.id == "loom.midnight")
            .unwrap();
        assert_eq!(entry.name, "Loom Midnight");
        let image = entry
            .icon_image
            .as_ref()
            .expect("the manifest's SVG artwork");
        let decoded = image::load_from_memory(&image.bytes).unwrap().into_rgba8();
        assert_eq!((decoded.width(), decoded.height()), (128, 128));
        assert!(
            decoded
                .pixels()
                .any(|pixel| pixel.0 == [0x5e, 0xea, 0xd4, 255]),
            "preserve the moon's original teal"
        );
        let readme = entry.readme_document.as_ref().unwrap();
        assert!(readme.content.contains("**Loom Midnight**"));
        assert!(readme.path.ends_with("loom-midnight/README.md"));
        assert_eq!(entry.contribution_count(), 2);
        let changelog = entry.changelog_document.as_ref().unwrap();
        assert!(changelog.content.contains("## 0.1.0"));
        assert!(changelog.content.contains("First release"));
        let again = super::frame(&service);
        assert_eq!(
            readme.content.identity,
            again
                .installed
                .iter()
                .find(|entry| entry.id == "loom.midnight")
                .unwrap()
                .readme_document
                .as_ref()
                .unwrap()
                .content
                .identity
        );
        assert!(entry.installed.as_ref().unwrap().enabled);
        service.stop("loom.midnight");
        assert!(
            !super::frame(&service)
                .installed
                .iter()
                .find(|entry| entry.id == "loom.midnight")
                .unwrap()
                .installed
                .as_ref()
                .unwrap()
                .enabled
        );
    }

    #[test]
    fn png_artwork_updates_on_reload_and_invalid_artwork_has_a_fallback() {
        let dir = std::env::temp_dir().join(format!("loom-icon-reload-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            r#"{"schema":1,"id":"test.icon","version":"1.0.0","icon":"icon.png"}"#,
        )
        .unwrap();
        let path = dir.join("icon.png");
        image::RgbaImage::from_pixel(12, 6, image::Rgba([255, 0, 0, 255]))
            .save(&path)
            .unwrap();
        let mut service = ExtensionService::new(dir.join("cache"));
        let id = service.load_package(&dir).unwrap();
        let first = frame(&service).installed[0].icon_image.clone().unwrap();
        image::RgbaImage::from_pixel(12, 6, image::Rgba([0, 255, 0, 255]))
            .save(&path)
            .unwrap();
        service.reload(&id).unwrap();
        let updated = frame(&service).installed[0].icon_image.clone().unwrap();
        assert_ne!(
            first.identity, updated.identity,
            "image cache follows content, even at the same package version"
        );
        let decoded = image::load_from_memory(&updated.bytes)
            .unwrap()
            .into_rgba8();
        assert_eq!(decoded.dimensions(), (12, 6));
        assert_eq!(decoded.get_pixel(0, 0).0, [0, 255, 0, 255]);
        std::fs::write(path, b"invalid PNG").unwrap();
        service.reload(&id).unwrap();
        let entry = frame(&service).installed.remove(0);
        assert!(entry.icon_image.is_none());
        assert!(
            entry
                .installed
                .unwrap()
                .warning
                .unwrap()
                .body
                .contains("icon")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
