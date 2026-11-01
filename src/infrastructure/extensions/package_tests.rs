use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

static RUN: AtomicUsize = AtomicUsize::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "loom-package-{}-{}",
            std::process::id(),
            RUN.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn manifest(version: &str, extra: &str) -> String {
    format!(
        r##"{{ "schema": 1, "id": "acme.night", "version": "{version}",
        "name": "Acme Night", "publisher": "Acme", "description": "A quieter palette.",
        "categories": ["Themes"], "license": "MIT", "repository": "https://example.com/acme",
        "icon": "icon.svg", "readme": "README.md", "changelog": "CHANGELOG.md",
        "engines": {{ "loom": ">=0.1.0" }}{extra},
        "contributes": {{ "themes": [{{ "id": "acme.night.dark", "path": "themes/dark.json" }}] }} }}"##
    )
}

fn files(version: &str) -> Vec<(String, Vec<u8>)> {
    vec![
        ("manifest.json".into(), manifest(version, "").into_bytes()),
        ("icon.svg".into(), b"<svg/>".to_vec()),
        ("README.md".into(), b"# Acme Night".to_vec()),
        ("CHANGELOG.md".into(), format!("## {version}").into_bytes()),
        ("themes/dark.json".into(), b"{}".to_vec()),
    ]
}

/// A package from `entries`, all inside the root folder named after the extension; a
/// name is written into the header as given, so a hostile one can be made.
fn build(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut tar = tar::Builder::new(Vec::new());
    for (name, bytes) in entries {
        let name = &format!("acme.night/{name}");
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        let field = &mut header.as_old_mut().name;
        field.fill(0);
        field[..name.len()].copy_from_slice(name.as_bytes());
        header.set_cksum();
        tar.append(&header, bytes.as_slice()).unwrap();
    }
    let bytes = tar.into_inner().unwrap();
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gzip.write_all(&bytes).unwrap();
    gzip.finish().unwrap()
}

fn import_files(temp: &Temp, entries: &[(String, Vec<u8>)]) -> Result<Imported, String> {
    let archive = temp.file("pkg.loomext", &build(entries));
    import(&archive, &temp.0.join("extensions"), "0.1.8")
}

#[test]
fn a_complete_package_is_installed_whole_and_remembers_where_it_came_from() {
    let temp = Temp::new();
    let imported = import_files(&temp, &files("1.0.0")).unwrap();
    assert_eq!(
        (
            imported.id.as_str(),
            imported.name.as_str(),
            imported.version.as_str()
        ),
        ("acme.night", "Acme Night", "1.0.0")
    );
    assert_eq!(imported.publisher, "Acme");
    assert_eq!(imported.replaced, None);
    assert!(!imported.needs_restart);
    for file in ["manifest.json", "icon.svg", "README.md", "themes/dark.json"] {
        assert!(imported.directory.join(file).is_file(), "{file}");
    }
    let extensions = temp.0.join("extensions");
    assert_eq!(imported.directory, extensions.join("acme.night/1.0.0"));
    let state = crate::infrastructure::extensions::state::read(&extensions);
    let entry = &state.extensions["acme.night"];
    assert_eq!(entry.version, "1.0.0");
    assert!(matches!(
        &entry.source,
        crate::infrastructure::extensions::state::Source::File { sha256, .. } if *sha256 == imported.sha256
    ));
    assert_eq!(imported.sha256.len(), 64);
    // Nothing of the import is left beside the installed extensions.
    let mut left: Vec<_> = fs::read_dir(&extensions)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(left, ["acme.night", "extensions.json"]);
}

#[test]
fn importing_again_replaces_the_extension_and_a_failure_keeps_the_old_one() {
    let temp = Temp::new();
    import_files(&temp, &files("1.0.0")).unwrap();
    let second = import_files(&temp, &files("1.1.0")).unwrap();
    assert_eq!(second.replaced.as_deref(), Some("1.0.0"));
    let installed = second.directory.join("CHANGELOG.md");
    assert_eq!(fs::read_to_string(&installed).unwrap(), "## 1.1.0");
    // The version before stays beside it, and the record points at the new one.
    let extensions = temp.0.join("extensions");
    assert!(extensions.join("acme.night/1.0.0/manifest.json").is_file());
    let entry =
        &crate::infrastructure::extensions::state::read(&extensions).extensions["acme.night"];
    assert_eq!(
        (entry.version.as_str(), entry.previous.as_deref()),
        ("1.1.0", Some("1.0.0"))
    );

    let mut broken = files("1.2.0");
    broken.retain(|(name, _)| name != "README.md");
    let error = import_files(&temp, &broken).unwrap_err();
    assert!(
        error.contains("`readme`: README.md is not in the package"),
        "{error}"
    );
    assert_eq!(
        fs::read_to_string(&installed).unwrap(),
        "## 1.1.0",
        "untouched"
    );
}

#[test]
fn an_incomplete_manifest_lists_everything_that_is_missing() {
    let temp = Temp::new();
    let mut entries = files("1.0.0");
    entries[0].1 = br#"{ "schema": 1, "id": "acme.night", "version": "1.0.0" }"#.to_vec();
    let error = import_files(&temp, &entries).unwrap_err();
    assert!(error.starts_with("the manifest is incomplete"), "{error}");
    for field in [
        "name",
        "publisher",
        "description",
        "categories",
        "license",
        "repository",
        "icon",
        "readme",
        "changelog",
        "engines.loom",
    ] {
        assert!(error.contains(&format!("`{field}`")), "{field}: {error}");
    }
    assert!(!temp.0.join("extensions/acme.night").exists());
}

#[test]
fn a_component_that_is_named_but_missing_and_a_loom_that_is_too_old_are_refused() {
    let temp = Temp::new();
    let mut entries = files("1.0.0");
    entries[0].1 = manifest(
        "1.0.0",
        r#", "api": "loom:extension@0.1.0", "component": "extension.wasm""#,
    )
    .into_bytes();
    let error = import_files(&temp, &entries).unwrap_err();
    assert!(
        error.contains("`component`: extension.wasm is not in the package"),
        "{error}"
    );

    let archive = temp.file("old.loomext", &build(&files("1.0.0")));
    let error = import(&archive, &temp.0.join("extensions"), "0.0.9").unwrap_err();
    assert!(
        error.contains("needs Loom >=0.1.0; this is Loom 0.0.9"),
        "{error}"
    );
}

#[test]
fn hostile_archives_are_refused_before_anything_is_installed() {
    let temp = Temp::new();
    for (name, expected) in [
        ("../evil.txt", "unusable path"),
        ("/abs.txt", "unusable path"),
        ("a\\b.txt", "unusable path"),
        ("a/./b.txt", "unusable path"),
        ("c:evil.txt", "unusable path"),
        ("trailing.", "unusable path"),
        ("README.MD", "duplicate"),
    ] {
        let mut entries = files("1.0.0");
        entries.push((name.to_owned(), b"x".to_vec()));
        let error = import_files(&temp, &entries).unwrap_err();
        assert!(error.contains(expected), "{name}: {error}");
    }
    assert!(!temp.0.join("evil.txt").exists());
    assert!(!temp.0.join("extensions/acme.night").exists());

    // A link is not a regular file.
    let mut tar = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_size(0);
    header.set_link_name("/etc/passwd").unwrap();
    header.set_cksum();
    tar.append_data(&mut header, "link", std::io::empty())
        .unwrap();
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gzip.write_all(&tar.into_inner().unwrap()).unwrap();
    let archive = temp.file("link.loomext", &gzip.finish().unwrap());
    let error = import(&archive, &temp.0.join("extensions"), "0.1.8").unwrap_err();
    assert!(error.contains("only regular files"), "{error}");

    // Not a package at all, and no manifest.
    let junk = temp.file("junk.loomext", b"not an archive");
    assert!(import(&junk, &temp.0.join("extensions"), "0.1.8").is_err());
    let empty = temp.file(
        "empty.loomext",
        &build(&[("README.md".into(), b"x".to_vec())]),
    );
    let error = import(&empty, &temp.0.join("extensions"), "0.1.8").unwrap_err();
    assert!(error.contains("no manifest.json"), "{error}");
}

/// Packages made by the extensions repository install here. Run on purpose, with
/// `LOOM_EXTENSIONS_PACKAGES` set to its `dist/packages` folder:
/// `cargo test --bins repository_packages -- --ignored`.
#[test]
#[ignore]
fn repository_packages_install() {
    let Some(packages) = std::env::var_os("LOOM_EXTENSIONS_PACKAGES") else {
        return;
    };
    let temp = Temp::new();
    let extensions = temp.0.join("extensions");
    let mut installed = 0;
    for entry in fs::read_dir(packages).unwrap().flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != EXTENSION) {
            continue;
        }
        let imported = import(&path, &extensions, env!("CARGO_PKG_VERSION"))
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        assert!(imported.directory.join("manifest.json").is_file());
        installed += 1;
    }
    assert!(installed > 0, "no packages found");
    // What was installed is read back the way startup reads it.
    let state = crate::infrastructure::extensions::state::read(&extensions);
    assert_eq!(state.extensions.len(), installed);
}

#[test]
fn the_root_folder_must_be_the_extension_and_the_only_one() {
    let temp = Temp::new();
    let extensions = temp.0.join("extensions");
    let rename = |entries: &[(String, Vec<u8>)], root: &str| -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        for (name, bytes) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            tar.append_data(&mut header, format!("{root}/{name}"), bytes.as_slice())
                .unwrap();
        }
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(&tar.into_inner().unwrap()).unwrap();
        gzip.finish().unwrap()
    };
    // A root folder with another name than the extension's id.
    let archive = temp.file("named.loomext", &rename(&files("1.0.0"), "something.else"));
    let error = import(&archive, &extensions, "0.1.8").unwrap_err();
    assert!(
        error.contains("something.else") && error.contains("acme.night"),
        "{error}"
    );
    // Files at the top, with no folder around them (the format before this one).
    let mut tar = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(2);
    header.set_mode(0o644);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    tar.append_data(&mut header, "manifest.json", &b"{}"[..])
        .unwrap();
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gzip.write_all(&tar.into_inner().unwrap()).unwrap();
    let archive = temp.file("flat.loomext", &gzip.finish().unwrap());
    let error = import(&archive, &extensions, "0.1.8").unwrap_err();
    assert!(error.contains("root folder"), "{error}");
    // Two root folders.
    let mut entries = files("1.0.0");
    entries.push(("../other/x.txt".into(), b"x".to_vec()));
    assert!(
        import(
            &temp.file("two.loomext", &build(&entries)),
            &extensions,
            "0.1.8"
        )
        .is_err()
    );
    assert!(!extensions.join("acme.night").exists());
}

#[test]
fn an_extension_with_a_language_pack_asks_for_a_restart() {
    let temp = Temp::new();
    let mut entries = files("1.0.0");
    entries[0].1 = manifest("1.0.0", "")
        .replace("\"themes\": [", "\"languages\": [{ \"id\": \"acme.night.g\", \"path\": \"g/tree-sitter.json\" }], \"themes\": [")
        .into_bytes();
    assert!(import_files(&temp, &entries).unwrap().needs_restart);
}
