use super::servers::*;
use crate::features::language_service::server_set::ServerSet;
use serde::{Deserialize, Serialize};
use serde_json::json;

fn catalog() -> std::sync::Arc<ServerSet> {
    crate::features::language_service::server_set::rust_analyzer_for_tests()
}

fn entry(name: &str, json: &str) -> LanguageServerSetting {
    let mut entry: LanguageServerSetting = serde_json::from_str(json).unwrap();
    entry.server = name.to_owned();
    entry
}

fn run(settings: &[LanguageServerSetting]) -> Option<ServerLaunch> {
    launch_for(catalog().get("rust-analyzer").unwrap(), settings)
}

#[test]
fn no_entry_runs_the_definition_as_written() {
    let launch = run(&[]).unwrap();
    assert_eq!(launch.command, "rust-analyzer");
    assert!(launch.args.is_empty());
    assert_eq!(launch.initialization_options, None);
    assert!(launch.env.is_empty());
}

#[test]
fn an_entry_overrides_only_what_it_names() {
    let launch = run(&[entry(
        "rust-analyzer",
        r#"{ "args": ["--log-file", "x"], "initialization_options": { "checkOnSave": false },
             "env": { "RA_LOG": "info" } }"#,
    )])
    .unwrap();
    assert_eq!(launch.command, "rust-analyzer", "the program never changes");
    assert_eq!(launch.args, ["--log-file", "x"]);
    assert_eq!(
        launch.initialization_options.as_deref(),
        Some(r#"{"checkOnSave":false}"#)
    );
    assert_eq!(launch.env["RA_LOG"], "info");
}

#[test]
fn disabling_wins_and_the_last_entry_counts() {
    let off = entry("rust-analyzer", r#"{ "enabled": false }"#);
    assert_eq!(run(std::slice::from_ref(&off)), None);
    let on_again = [off, entry("rust-analyzer", r#"{ "args": ["x"] }"#)];
    assert_eq!(run(&on_again).unwrap().args, ["x"]);
}

#[test]
fn empty_args_replace_the_default_args() {
    assert!(
        run(&[entry("rust-analyzer", r#"{ "args": [] }"#)])
            .unwrap()
            .args
            .is_empty()
    );
}

#[test]
fn a_catalogued_servers_program_cannot_be_changed() {
    let set = catalog();
    for field in [
        r#"{ "command": "/bin/evil" }"#,
        r#"{ "languages": [] }"#,
        r#"{ "root": { "markers": ["x"] } }"#,
        r#"{ "display_name": "x" }"#,
    ] {
        let error = entry("rust-analyzer", field).validate(&set).unwrap_err();
        assert!(error.contains("cannot be changed"), "{field}: {error}");
    }
    assert!(
        entry("rust-analyzer", r#"{ "args": ["x"], "enabled": false }"#)
            .validate(&set)
            .is_ok()
    );
}

#[test]
fn bad_values_are_explained() {
    let set = catalog();
    for (json, wanted) in [
        (r#"{ "initialization_options": [] }"#, "object"),
        (r#"{ "settings": 3 }"#, "object"),
        (r#"{ "env": { "": "x" } }"#, "env"),
        (r#"{ "env": { "A=B": "x" } }"#, "env"),
    ] {
        let error = entry("rust-analyzer", json).validate(&set).unwrap_err();
        assert!(error.contains(wanted), "{json}: {error}");
    }
    assert!(serde_json::from_str::<LanguageServerSetting>(r#"{ "extra": 1 }"#).is_err());
}

fn own(command: &str) -> String {
    format!(
        r#"{{ "command": {command:?}, "args": ["--stdio"],
              "languages": [{{ "language": "foo", "language_id": "foo" }}],
              "root": {{ "markers": ["foo.toml"] }} }}"#
    )
}

#[cfg(windows)]
const ABSOLUTE: &str = "C:\\tools\\foo-server.exe";
#[cfg(not(windows))]
const ABSOLUTE: &str = "/opt/foo-server";

#[test]
fn a_server_of_your_own_is_registered_by_name_with_an_absolute_program() {
    let set = catalog();
    let good = entry("foo-server", &own(ABSOLUTE));
    assert!(good.validate(&set).is_ok());
    let definition = good.definition().unwrap();
    assert_eq!(definition["command"], ABSOLUTE);
    assert_eq!(definition["root"]["fallback"], "workspace");

    // Never a program found on PATH.
    for command in ["foo-server", "./foo-server", "foo/bin/server"] {
        let error = entry("foo-server", &own(command))
            .validate(&set)
            .unwrap_err();
        assert!(error.contains("absolute"), "{command}: {error}");
    }
}

#[test]
fn a_registration_needs_a_proper_name_languages_and_a_root() {
    let set = catalog();
    let error = entry("Bad Name", &own(ABSOLUTE))
        .validate(&set)
        .unwrap_err();
    assert!(error.contains("not a server of the catalog"));
    let error = entry("foo-server", r#"{ "command": "/x" }"#)
        .validate(&set)
        .unwrap_err();
    assert!(
        error.contains("absolute") || error.contains("languages"),
        "{error}"
    );
    let missing_root = format!(
        r#"{{ "command": {ABSOLUTE:?}, "languages": [{{ "language": "foo", "language_id": "foo" }}] }}"#
    );
    assert!(
        entry("foo-server", &missing_root)
            .validate(&set)
            .unwrap_err()
            .contains("root")
    );
    let bad_language = format!(
        r#"{{ "command": {ABSOLUTE:?}, "languages": [{{ "language": "Not Canonical", "language_id": "x" }}],
              "root": {{ "markers": ["m"] }} }}"#
    );
    assert!(entry("foo-server", &bad_language).validate(&set).is_err());
}

#[test]
fn registered_servers_are_collected_and_the_bad_ones_reported() {
    let set = catalog();
    let entries = [
        entry("rust-analyzer", r#"{ "args": [] }"#),
        entry("foo-server", &own(ABSOLUTE)),
        entry("bar-server", r#"{ "command": "bar" }"#),
    ];
    let (found, problems) = registered(&entries, &set);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, "foo-server");
    assert_eq!(problems.len(), 1);
    assert!(problems[0].starts_with("bar-server"));
}

#[test]
fn entries_are_an_object_keyed_by_name_in_the_file() {
    #[derive(Deserialize, Serialize)]
    struct File {
        #[serde(with = "as_object", default)]
        language_servers: Vec<LanguageServerSetting>,
    }
    let file: File = serde_json::from_str(
        r#"{ "language_servers": { "b": { "enabled": false }, "a": { "args": ["x"] } } }"#,
    )
    .unwrap();
    let names: Vec<_> = file
        .language_servers
        .iter()
        .map(|e| e.server.as_str())
        .collect();
    assert_eq!(names, ["a", "b"]);
    let json = serde_json::to_string(&file).unwrap();
    assert!(
        json.contains(r#""a":{"enabled":true,"args":["x"]}"#),
        "{json}"
    );
    assert!(
        !json.contains(r#""server""#),
        "the name is only the key: {json}"
    );
}

#[test]
fn settings_come_from_the_last_matching_entry() {
    let entries = [
        entry("a", r#"{ "settings": { "x": 1 } }"#),
        entry("b", r#"{ "enabled": false }"#),
        entry("a", r#"{ "enabled": true }"#),
    ];
    assert_eq!(
        server_settings(&entries, "a"),
        json!({}),
        "the last entry has none"
    );
    assert_eq!(server_settings(&entries[..1], "a"), json!({ "x": 1 }));
}

#[test]
fn a_language_entry_orders_and_selects_its_servers() {
    let all = vec!["a", "b", "c", "d"];
    let none = LanguageSetting::default();
    assert_eq!(none.arrange(all.clone()), all);
    let setting = |servers: &[&str]| LanguageSetting {
        language_servers: servers.iter().map(|s| s.to_string()).collect(),
        ..LanguageSetting::default()
    };
    assert_eq!(setting(&["c", "a"]).arrange(all.clone()), ["c", "a"]);
    assert_eq!(
        setting(&["c", "..."]).arrange(all.clone()),
        ["c", "a", "b", "d"]
    );
    assert_eq!(
        setting(&["...", "b"]).arrange(all.clone()),
        ["a", "c", "d", "b"]
    );
    assert_eq!(
        setting(&["zzz", "b"]).arrange(all.clone()),
        ["b"],
        "unknown names are skipped"
    );
    assert_eq!(
        setting(&["b", "b", "..."]).arrange(all),
        ["b", "a", "c", "d"]
    );
}

#[test]
fn lsp_settings_have_defaults_and_are_checked() {
    let defaults = LspSettings::default();
    assert!(defaults.validate().is_ok());
    assert_eq!(defaults.hover_delay_ms, 500);
    let parsed: LspSettings =
        serde_json::from_str(r#"{ "completion": { "auto": false }, "inlay_hints": true }"#)
            .unwrap();
    assert!(!parsed.completion.auto && parsed.completion.words_fallback && parsed.inlay_hints);
    for bad in [
        r#"{ "completion": { "accept_on": "space" } }"#,
        r#"{ "semantic_highlighting": "loud" }"#,
        r#"{ "hover_delay_ms": 999999 }"#,
        r#"{ "max_document_bytes": 5 }"#,
    ] {
        let settings: LspSettings = serde_json::from_str(bad).unwrap();
        assert!(settings.validate().is_err(), "{bad}");
    }
    assert!(serde_json::from_str::<LspSettings>(r#"{ "nope": 1 }"#).is_err());
}
