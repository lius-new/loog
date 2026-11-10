//! Workspace rules exercised with supplied observations and opaque unit payloads.
use crate::domain::document::{DiskRead, DiskState};
use crate::domain::identity::FileId;
use crate::domain::pane_layout::Direction;
use crate::domain::workspace::{OpenMode, PreparedTextFile, ReconcileResult, Workspace};
use loog_editor_core::normal;

type PureWorkspace = Workspace<(), ()>;
fn open(workspace: &mut PureWorkspace, path: &str, text: &str) -> FileId {
    let prepared =
        PreparedTextFile::with_disk_state(path.into(), text.into(), DiskState::from_contents(text));
    workspace.open_prepared_text(prepared, OpenMode::Permanent)
}
fn observed(text: &str) -> DiskRead {
    DiskRead::Found {
        contents: text.into(),
        state: DiskState::from_contents(text),
        bytes: None,
    }
}

#[test]
fn failed_tabs_never_expose_empty_text_for_editing_or_saving_and_retry_preserves_views() {
    let mut workspace = PureWorkspace::new();
    let path = std::path::PathBuf::from("unreadable.txt");
    let id = workspace.open_prepared_text(
        PreparedTextFile::failed(path.clone(), "Permission denied".into()),
        OpenMode::Permanent,
    );
    let left = workspace.active_pane();
    let left_view = workspace.active().unwrap();
    let right = workspace.split(left, Direction::Right).unwrap();
    let right_view = workspace.active().unwrap();
    assert_eq!(workspace.pane_open_error(left), Some("Permission denied"));
    assert_eq!(workspace.pane_open_error(right), Some("Permission denied"));
    assert!(workspace.editor(left).is_none());
    assert!(workspace.document_editor_mut(id).is_none());
    assert!(workspace.save_snapshot(id).is_none());
    assert!(workspace.text_snapshots().is_empty());
    assert!(!workspace.is_dirty(id));
    let prepared = PreparedTextFile::with_disk_state(
        path,
        "recovered".into(),
        DiskState::from_contents("recovered"),
    );
    assert_eq!(
        workspace.open_prepared_text(prepared, OpenMode::Permanent),
        id
    );
    assert_eq!(workspace.active(), Some(right_view));
    assert_eq!(workspace.pane(left).unwrap().active(), Some(left_view));
    assert_eq!(workspace.editor(left).unwrap().text(), "recovered");
    assert_eq!(workspace.editor(right).unwrap().text(), "recovered");
    assert!(workspace.pane_open_error(left).is_none());
    assert!(workspace.pane_open_error(right).is_none());
    assert_eq!(workspace.documents.len(), 1);
}

#[test]
fn unicode_edits_and_shared_undo_transform_both_pane_selections() {
    let mut workspace = PureWorkspace::new();
    let id = open(&mut workspace, "virtual.txt", "a界b\r\n");
    let left = workspace.active_pane();
    let right = workspace.split(left, Direction::Right).unwrap();
    workspace.editor_mut(left).unwrap().set_cursor(1);
    workspace.editor_mut(right).unwrap().set_cursor(4);
    normal::insert(&mut workspace.editor_mut(left).unwrap(), "你");
    assert_eq!(workspace.editor(right).unwrap().text(), "a你界b\r\n");
    assert_eq!(workspace.editor(right).unwrap().cursor(), 7);
    assert!(workspace.is_dirty(id));
    workspace.editor_mut(right).unwrap().undo();
    assert_eq!(workspace.editor(left).unwrap().text(), "a界b\r\n");
    assert_eq!(workspace.editor(left).unwrap().cursor(), 1);
    // The pane invoking Undo receives the edit's recorded original selection.
    assert_eq!(workspace.editor(right).unwrap().cursor(), 1);
    assert!(!workspace.is_dirty(id));
}

#[test]
fn injected_disk_conflicts_keep_edits_until_explicit_acceptance() {
    let mut workspace = PureWorkspace::new();
    let id = open(&mut workspace, "virtual.txt", "base");
    let left = workspace.active_pane();
    let right = workspace.split(left, Direction::Right).unwrap();
    normal::insert(&mut workspace.editor_mut(left).unwrap(), "edit ");
    workspace.editor_mut(right).unwrap().select_range(2..9);
    workspace.set_scroll(right, 17.0, 48.0);
    assert_eq!(
        workspace.reconcile_with_disk(id, observed("disk")),
        ReconcileResult::Conflict("virtual.txt".into())
    );
    assert_eq!(workspace.editor(left).unwrap().text(), "edit base");
    workspace
        .accept_disk_with(id, "disk".into(), DiskState::from_contents("disk"))
        .unwrap();
    assert_eq!(workspace.editor(left).unwrap().text(), "disk");
    assert_eq!(workspace.editor(right).unwrap().selection(), Some(2..4));
    assert_eq!(workspace.scroll(right), (17.0, 48.0));
    assert!(!workspace.editor(right).unwrap().can_undo());
    assert!(!workspace.is_dirty(id));
    assert!(!workspace.has_disk_conflict(id));
}

#[test]
fn missing_recovered_and_unreadable_observations_remain_distinct() {
    let mut workspace = PureWorkspace::new();
    let id = open(&mut workspace, "virtual.txt", "base");
    normal::insert(&mut workspace.active_editor_mut().unwrap(), "edit ");
    assert_eq!(
        workspace.reconcile_with_disk(id, DiskRead::Missing),
        ReconcileResult::Missing("virtual.txt".into())
    );
    assert!(workspace.is_missing_on_disk(id));
    assert!(workspace.has_disk_conflict(id));
    let version = workspace.active_editor().unwrap().version();
    assert_eq!(
        workspace.reconcile_with_disk(id, DiskRead::Failed("permission denied".into())),
        ReconcileResult::Failed("virtual.txt".into(), "permission denied".into())
    );
    assert_eq!(workspace.active_editor().unwrap().version(), version);
    assert!(workspace.is_missing_on_disk(id));
    workspace.reconcile_with_disk(id, observed("base"));
    assert!(!workspace.is_missing_on_disk(id));
    assert!(!workspace.has_disk_conflict(id));
    assert_eq!(workspace.active_editor().unwrap().text(), "edit base");
    assert!(workspace.is_dirty(id));
}

#[test]
fn a_closed_placeholder_rejects_late_preparation_and_returns_retired_data() {
    let mut workspace = PureWorkspace::new();
    let id = workspace.open_deferred("virtual.txt".into());
    assert!(workspace.active_editor().is_none());
    assert!(!workspace.mark_saved_with_disk(id, DiskState::from_contents("")));
    assert_eq!(
        workspace.reconcile_with_disk(id, observed("disk")),
        ReconcileResult::Unchanged("virtual.txt".into())
    );
    workspace.close(id);
    let prepared = PreparedTextFile::with_disk_state(
        "virtual.txt".into(),
        "late".into(),
        DiskState::from_contents("late"),
    );
    assert!(!workspace.install_restored(id, std::path::Path::new("virtual.txt"), prepared));
    assert!(workspace.meta(id).is_none());
    assert_eq!(workspace.take_retired().len(), 2);
    assert!(workspace.take_retired().is_empty());
}

#[test]
fn retargeting_metadata_keeps_identity_views_and_shared_history() {
    let mut workspace = PureWorkspace::new();
    let id = open(&mut workspace, "virtual.txt", "base");
    let left = workspace.active_pane();
    let right = workspace.split(left, Direction::Right).unwrap();
    normal::insert(&mut workspace.editor_mut(left).unwrap(), "edit ");
    workspace.set_scroll(right, 7.0, 20.0);
    assert!(workspace.move_path_with_disk(
        std::path::Path::new("virtual.txt"),
        "renamed.rs".into(),
        DiskState::from_contents("base")
    ));
    assert_eq!(
        workspace.file_id_for_path(std::path::Path::new("renamed.rs")),
        Some(id)
    );
    assert!(
        workspace
            .file_id_for_path(std::path::Path::new("virtual.txt"))
            .is_none()
    );
    assert_eq!(workspace.pane_document(left), Some(id));
    assert_eq!(workspace.pane_document(right), Some(id));
    assert_eq!(workspace.scroll(right), (7.0, 20.0));
    workspace.editor_mut(right).unwrap().undo();
    assert_eq!(workspace.editor(left).unwrap().text(), "base");
    assert!(workspace.mark_saved_with_disk(id, DiskState::from_contents("base")));
    assert!(!workspace.is_dirty(id));
}

#[test]
fn stamps_separate_lifetimes_text_versions_and_languages() {
    use crate::domain::language::builtin;
    let mut workspace = PureWorkspace::new();
    let id = open(&mut workspace, "a.rs", "fn a() {}");
    let first = workspace.documents[&id].stamp(id);
    assert_eq!(
        first,
        workspace.documents[&id].stamp(id),
        "stable until something changes"
    );

    // An edit moves the version and the snapshot.
    normal::insert(
        &mut workspace.editor_mut(workspace.active_pane()).unwrap(),
        "x",
    );
    let edited = workspace.documents[&id].stamp(id);
    assert_ne!(first, edited);
    assert!(edited.version > first.version);
    assert_eq!(edited.language_generation, first.language_generation);

    // Switching language keeps the text but must still invalidate results.
    workspace
        .documents
        .get_mut(&id)
        .unwrap()
        .meta
        .set_language(builtin("plaintext"));
    let relabelled = workspace.documents[&id].stamp(id);
    assert_eq!(relabelled.version, edited.version);
    assert_eq!(relabelled.snapshot_identity, edited.snapshot_identity);
    assert_ne!(relabelled, edited);
    workspace
        .documents
        .get_mut(&id)
        .unwrap()
        .meta
        .set_language(builtin("plaintext"));
    assert_eq!(
        workspace.documents[&id].stamp(id),
        relabelled,
        "setting the same language is not a change"
    );

    // Closing and reopening the same file never reproduces an old stamp, even
    // for identical text, and each install gets its own incarnation.
    let incarnation = workspace.documents[&id].incarnation;
    workspace.close(id);
    let reopened = open(&mut workspace, "a.rs", "fn a() {}");
    let again = workspace.documents[&reopened].stamp(reopened);
    assert_ne!(again.incarnation, incarnation);
    assert_ne!(again, first);
    let other = open(&mut workspace, "b.rs", "fn a() {}");
    assert_ne!(workspace.documents[&other].incarnation, again.incarnation);
}

#[test]
fn renaming_a_file_recomputes_its_language_and_invalidates_old_results() {
    let mut workspace = PureWorkspace::new();
    let id = open(&mut workspace, "a.rs", "fn a() {}");
    let before = workspace.documents[&id].stamp(id);
    let disk = || DiskState::from_contents("fn a() {}");

    // Same language: nothing derived from the old name is stale.
    assert!(workspace.move_path_with_disk("a.rs".as_ref(), "b.rs".into(), disk()));
    let renamed = workspace.documents[&id].stamp(id);
    assert_eq!(
        renamed, before,
        "a rename within one language changes nothing"
    );

    // Another language: the same text and snapshot must still not match.
    assert!(workspace.move_path_with_disk("b.rs".as_ref(), "b.txt".into(), disk()));
    let relabelled = workspace.documents[&id].stamp(id);
    assert_eq!(workspace.documents[&id].meta.lang.name(), "plaintext");
    assert_eq!(relabelled.version, before.version);
    assert_eq!(relabelled.snapshot_identity, before.snapshot_identity);
    assert_ne!(relabelled, before);

    // And back again: still a different stamp from either earlier one.
    assert!(workspace.move_path_with_disk("b.txt".as_ref(), "c.rs".into(), disk()));
    let again = workspace.documents[&id].stamp(id);
    assert_eq!(workspace.documents[&id].meta.lang.name(), "rust");
    assert_ne!(again, before);
    assert_ne!(again, relabelled);
}

mod associations {
    use crate::domain::document::DiskState;
    use crate::domain::identity::FileId;
    use crate::domain::language::{AssociationSpec, LanguageRules, Registry};
    use crate::domain::pane_layout::Direction;
    use crate::domain::workspace::tests::{PureWorkspace, open};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn rules(
        roots: &[&str],
        user: &[(&str, &str)],
        project: &[(&str, &str, &str)],
    ) -> Arc<LanguageRules> {
        let roots: Vec<PathBuf> = roots.iter().map(PathBuf::from).collect();
        let user: Vec<AssociationSpec<'_>> = user
            .iter()
            .map(|&(pattern, language)| AssociationSpec { pattern, language })
            .collect();
        let projects: Vec<(PathBuf, Vec<AssociationSpec<'_>>)> = project
            .iter()
            .map(|&(root, pattern, language)| {
                (
                    PathBuf::from(root),
                    vec![AssociationSpec { pattern, language }],
                )
            })
            .collect();
        let (rules, errors) =
            LanguageRules::compile(&Registry::builtin(), false, &roots, &user, &projects);
        assert!(errors.is_empty(), "{errors:?}");
        Arc::new(rules)
    }
    fn language(workspace: &PureWorkspace, id: FileId) -> &str {
        workspace.documents[&id].meta.lang.name()
    }

    #[test]
    fn a_live_catalog_updates_only_matching_text_and_diff_metadata() {
        use crate::domain::document::FileMeta;
        const PLAIN: &str = r#"{"name":"plaintext","display_name":"Plain Text","badge":"TXT","variants":[{"id":"plain"}]}"#;
        const SAMPLE: &str = r#"{"name":"sample","display_name":"Sample","badge":"S","variants":[{"id":"sample","grammar":"sample"}],"files":[{"extensions":["live"],"variant":"sample"}]}"#;
        let catalog = |enabled| {
            let mut sources = vec![("plaintext", PLAIN)];
            let mut grammars = Vec::new();
            if enabled {
                sources.push(("sample", SAMPLE));
                grammars.push("sample");
            }
            Arc::new(Registry::from_sources(&sources, &grammars).unwrap())
        };
        let refresh = |workspace: &mut PureWorkspace, enabled| {
            let registry = catalog(enabled);
            let user = [AssociationSpec {
                pattern: "*.manual",
                language: "sample",
            }];
            let (rules, _) = LanguageRules::compile(&registry, false, &[], &user, &[]);
            workspace.set_language_rules(registry, Arc::new(rules))
        };

        let mut workspace = PureWorkspace::new();
        refresh(&mut workspace, false);
        let unrelated = open(&mut workspace, "/p/unrelated.txt", "plain");
        let manual = open(&mut workspace, "/p/alias.manual", "manual");
        let affected = open(&mut workspace, "/p/b.live", "original");
        let pane = workspace.active_pane();
        loog_editor_core::normal::insert(&mut workspace.editor_mut(pane).unwrap(), "edited ");
        let diff = workspace.insert_diff(
            pane,
            FileMeta::from_path("/p/change.live".into()),
            Arc::new(()),
        );
        let before = workspace.documents[&affected].stamp(affected);
        let unaffected = workspace.documents[&unrelated].stamp(unrelated);

        assert!(refresh(&mut workspace, true));
        let after = workspace.documents[&affected].stamp(affected);
        assert_ne!(after, before);
        assert_eq!(after.version, before.version);
        assert_eq!(after.snapshot_identity, before.snapshot_identity);
        assert!(workspace.is_dirty(affected));
        assert_eq!(workspace.documents[&unrelated].stamp(unrelated), unaffected);
        assert_eq!(language(&workspace, affected), "sample");
        assert_eq!(language(&workspace, manual), "sample");
        assert_eq!(workspace.view_meta(diff).unwrap().lang.name(), "sample");
        let new = open(&mut workspace, "/p/new.live", "new");
        assert_eq!(language(&workspace, new), "sample");
        assert!(!refresh(&mut workspace, true));
        assert_eq!(workspace.documents[&affected].stamp(affected), after);

        assert!(refresh(&mut workspace, false));
        assert_eq!(language(&workspace, affected), "plaintext");
        assert_eq!(language(&workspace, manual), "plaintext");
        assert_eq!(workspace.view_meta(diff).unwrap().lang.name(), "plaintext");
        assert!(workspace.is_dirty(affected));
    }

    #[test]
    fn a_file_opened_after_the_rules_gets_the_associated_language() {
        let mut workspace = PureWorkspace::new();
        workspace.set_language_rules(
            Registry::builtin(),
            rules(&["/p"], &[("*.custom", "rust")], &[]),
        );
        let custom = open(&mut workspace, "/p/a.custom", "x");
        let plain = open(&mut workspace, "/p/b.txt", "x");
        let builtin = open(&mut workspace, "/p/c.cs", "x");
        assert_eq!(language(&workspace, custom), "rust");
        assert_eq!(language(&workspace, plain), "plaintext");
        assert_eq!(
            language(&workspace, builtin),
            "csharp",
            "built-in recognition is the fallback"
        );
        assert_eq!(workspace.documents[&custom].meta.language_generation, 0);
    }

    #[test]
    fn an_association_beats_the_built_in_recognition_of_the_same_file() {
        let mut workspace = PureWorkspace::new();
        workspace.set_language_rules(
            Registry::builtin(),
            rules(&["/p"], &[("*.rs", "csharp")], &[]),
        );
        let id = open(&mut workspace, "/p/a.rs", "x");
        assert_eq!(language(&workspace, id), "csharp");
    }

    #[test]
    fn installing_rules_re_recognizes_open_files_and_only_changed_ones_get_a_new_stamp() {
        let mut workspace = PureWorkspace::new();
        let custom = open(&mut workspace, "/p/a.custom", "x");
        let other = open(&mut workspace, "/p/b.rs", "x");
        let (custom_before, other_before) = (
            workspace.documents[&custom].stamp(custom),
            workspace.documents[&other].stamp(other),
        );
        assert_eq!(language(&workspace, custom), "plaintext");

        let changed = workspace.set_language_rules(
            Registry::builtin(),
            rules(&["/p"], &[("*.custom", "rust")], &[]),
        );
        assert!(changed);
        assert_eq!(language(&workspace, custom), "rust");
        assert_ne!(workspace.documents[&custom].stamp(custom), custom_before);
        assert_eq!(
            workspace.documents[&other].stamp(other),
            other_before,
            "unaffected files keep their stamp"
        );

        assert!(
            !workspace.set_language_rules(
                Registry::builtin(),
                rules(&["/p"], &[("*.custom", "rust")], &[])
            ),
            "the same rules change nothing"
        );
        assert!(
            workspace.set_language_rules(Registry::builtin(), Arc::default()),
            "removing the rules reverts the file"
        );
        assert_eq!(language(&workspace, custom), "plaintext");
    }

    #[test]
    fn renaming_applies_the_rules_to_the_new_name() {
        let mut workspace = PureWorkspace::new();
        workspace.set_language_rules(
            Registry::builtin(),
            rules(&["/p"], &[("*.custom", "rust")], &[]),
        );
        let id = open(&mut workspace, "/p/a.txt", "x");
        let before = workspace.documents[&id].stamp(id);
        let disk = DiskState::from_contents("x");
        assert!(workspace.move_path_with_disk(
            "/p/a.txt".as_ref(),
            "/p/a.custom".into(),
            disk.clone()
        ));
        assert_eq!(language(&workspace, id), "rust");
        assert_ne!(workspace.documents[&id].stamp(id), before);
        assert!(workspace.move_path_with_disk("/p/a.custom".as_ref(), "/p/a.rs".into(), disk));
        assert_eq!(
            language(&workspace, id),
            "rust",
            "now by the built-in rule; same language"
        );
    }

    #[test]
    fn project_rules_only_reach_their_own_folder() {
        let mut workspace = PureWorkspace::new();
        workspace.set_language_rules(
            Registry::builtin(),
            rules(&["/p", "/q"], &[], &[("/p", "*.x", "csharp")]),
        );
        let inside = open(&mut workspace, "/p/a.x", "x");
        let outside = open(&mut workspace, "/q/a.x", "x");
        assert_eq!(language(&workspace, inside), "csharp");
        assert_eq!(language(&workspace, outside), "plaintext");
    }

    #[test]
    fn every_pane_showing_a_document_sees_the_same_language() {
        let mut workspace = PureWorkspace::new();
        let id = open(&mut workspace, "/p/a.custom", "x");
        let left = workspace.active_pane();
        let right = workspace.split(left, Direction::Right).unwrap();
        workspace.set_language_rules(
            Registry::builtin(),
            rules(&["/p"], &[("*.custom", "rust")], &[]),
        );
        // One document, one metadata record: both panes read the same value.
        assert_eq!(workspace.pane_document(left), Some(id));
        assert_eq!(workspace.pane_document(right), Some(id));
        assert_eq!(language(&workspace, id), "rust");
        assert_eq!(workspace.documents.len(), 1);
    }
}
