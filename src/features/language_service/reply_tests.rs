use super::*;
use crate::features::language_service::capabilities::PositionEncoding::{Utf8, Utf16};
use serde_json::json;

fn range(a: (u32, u32), b: (u32, u32)) -> Value {
    json!({ "start": { "line": a.0, "character": a.1 }, "end": { "line": b.0, "character": b.1 } })
}

#[test]
fn spans_turn_into_bytes_in_the_servers_encoding() {
    let text = "a😀b\nxyz";
    // 'b' is at UTF-16 column 3 and UTF-8 column 5.
    let in16 = span(&range((0, 3), (0, 4)), Utf16).unwrap();
    let in8 = span(&range((0, 5), (0, 6)), Utf8).unwrap();
    assert_eq!(in16.bytes(text), 5..6);
    assert_eq!(in8.bytes(text), 5..6);
    // Out of range and backwards spans are clamped, not rejected.
    let wild = span(&range((9, 0), (0, 1)), Utf16).unwrap();
    assert_eq!(wild.bytes(text), 1..text.len());
    let round = Span::of(text, 1..5, Utf16);
    assert_eq!((round.start.character, round.end.character), (1, 3));
    assert_eq!(round.bytes(text), 1..5);
}

#[test]
fn markup_accepts_every_shape_the_protocol_allows() {
    assert_eq!(markup(&json!("plain")).unwrap(), "plain");
    assert_eq!(
        markup(&json!({ "kind": "markdown", "value": "**x**" })).unwrap(),
        "**x**"
    );
    assert_eq!(
        markup(&json!({ "language": "rust", "value": "fn a()" })).unwrap(),
        "```rust\nfn a()\n```"
    );
    assert_eq!(
        markup(&json!(["one", { "language": "c", "value": "int a" }])).unwrap(),
        "one\n\n```c\nint a\n```"
    );
    assert_eq!(markup(&json!([])), None);
    assert_eq!(markup(&json!(3)), None);
}

#[test]
fn an_empty_hover_is_no_hover() {
    assert_eq!(hover(&json!({ "contents": "  " }), Utf16), None);
    assert_eq!(hover(&Value::Null, Utf16), None);
    let h = hover(
        &json!({ "contents": { "kind": "plaintext", "value": "x" }, "range": range((1, 0), (1, 2)) }),
        Utf16,
    )
    .unwrap();
    assert_eq!(h.markdown, "x");
    assert_eq!(h.span.unwrap().end.character, 2);
}

#[test]
fn locations_come_as_one_a_list_or_links() {
    let path = std::env::temp_dir().join("location-a.rs");
    let target = std::env::temp_dir().join("location-b.rs");
    let uri = crate::features::language_service::uri::file_uri(&path).unwrap();
    let target_uri = crate::features::language_service::uri::file_uri(&target).unwrap();
    let one = json!({ "uri": uri, "range": range((1, 2), (1, 5)) });
    assert_eq!(locations(&one, Utf16).len(), 1);
    assert_eq!(
        locations(&json!([one.clone(), one.clone()]), Utf16).len(),
        2
    );
    let link = json!([{
        "originSelectionRange": range((0, 0), (0, 3)),
        "targetUri": target_uri,
        "targetRange": range((4, 0), (9, 1)),
        "targetSelectionRange": range((4, 3), (4, 6)),
    }]);
    let found = &locations(&link, Utf16)[0];
    assert_eq!(found.resource, target.into());
    assert_eq!(found.span.start.character, 3, "the name is selected");
    assert_eq!(found.full.unwrap().end.line, 9);
    assert_eq!(found.origin.unwrap().end.character, 3);
    assert!(locations(&Value::Null, Utf16).is_empty());
    // An unreadable item is dropped without losing the others.
    assert_eq!(locations(&json!([{ "uri": 3 }, one]), Utf16).len(), 1);
}

#[test]
fn resource_locations_preserve_environment_and_explicit_unicode_units() {
    use loog_workspace_api::{EnvironmentKey, ResourceRef};
    let first = ResourceRef::provided(
        EnvironmentKey::new("provider", "one").unwrap(),
        "/work/a\\中.rs",
    )
    .unwrap();
    let second = ResourceRef::provided(
        EnvironmentKey::new("provider", "two").unwrap(),
        "/work/a\\中.rs",
    )
    .unwrap();
    let mut span = range((0, 3), (0, 4));
    span["x-loog-column-unit"] = json!("utf16");
    let found = locations(
        &json!([{"resource":first,"range":span},{"resource":second,"range":span}]),
        Utf8,
    );
    assert_eq!(found.len(), 2);
    assert_ne!(found[0].resource, found[1].resource);
    assert_eq!(found[0].span.bytes("a😀b"), 5..6);
    assert!(found[0].resource.local_path().is_none());
    assert!(
        locations(
            &json!({"resource":{"kind":"provided","path":"a"},"uri":"file:///a","range":span}),
            Utf8
        )
        .is_empty()
    );
}

#[test]
fn highlights_carry_their_kind() {
    let found = highlights(
        &json!([{ "range": range((0, 0), (0, 1)), "kind": 3 }, { "range": range((1, 0), (1, 1)) }]),
        Utf16,
    );
    assert_eq!(found[0].kind, HighlightKind::Write);
    assert_eq!(found[1].kind, HighlightKind::Text);
}

#[test]
fn symbols_may_be_nested_or_flat() {
    let nested = json!([{
        "name": "Outer", "kind": 5, "range": range((0, 0), (9, 0)),
        "selectionRange": range((0, 6), (0, 11)),
        "children": [{ "name": "inner", "kind": 6, "detail": "fn()", "range": range((1, 0), (2, 0)),
                       "selectionRange": range((1, 3), (1, 8)), "tags": [1] }]
    }]);
    let found = symbols(&nested, Utf16);
    assert_eq!(found[0].selection.start.character, 6);
    assert_eq!(found[0].children[0].detail.as_deref(), Some("fn()"));
    assert!(found[0].children[0].deprecated);

    let flat = json!([{ "name": "f", "kind": 12, "containerName": "m",
                        "location": { "uri": "file:///a.rs", "range": range((3, 0), (3, 4)) } },
                      { "name": "g", "kind": 12, "location": { "uri": "file:///b.rs" } }]);
    let found = symbols(&flat, Utf16);
    assert_eq!(found[0].container.as_deref(), Some("m"));
    assert_eq!(found[0].uri.as_deref(), Some("file:///a.rs"));
    assert_eq!(
        found[1].uri.as_deref(),
        Some("file:///b.rs"),
        "workspace symbol without range"
    );
}

#[test]
fn a_workspace_edit_keeps_its_order_and_refuses_pieces_it_cannot_read() {
    let edit = json!({ "documentChanges": [
        { "kind": "create", "uri": "file:///new.rs", "options": { "ignoreIfExists": true } },
        { "textDocument": { "uri": "file:///a.rs", "version": 4 },
          "edits": [{ "range": range((0, 0), (0, 1)), "newText": "x" }] },
        { "kind": "rename", "oldUri": "file:///a.rs", "newUri": "file:///b.rs" },
        { "kind": "delete", "uri": "file:///c.rs", "options": { "recursive": true } },
    ] });
    let parsed = workspace_edit(&edit, Utf16).unwrap();
    assert_eq!(parsed.changes.len(), 4);
    assert!(matches!(
        &parsed.changes[0],
        Change::Create {
            ignore_if_exists: true,
            ..
        }
    ));
    match &parsed.changes[1] {
        Change::Edit(e) => assert_eq!((e.version, e.edits.len()), (Some(4), 1)),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        &parsed.changes[3],
        Change::Delete {
            recursive: true,
            ..
        }
    ));

    let simple = json!({ "changes": { "file:///a.rs": [{ "range": range((0, 0), (0, 0)), "newText": "y" }] } });
    assert_eq!(workspace_edit(&simple, Utf16).unwrap().changes.len(), 1);

    let broken = json!({ "documentChanges": [
        { "textDocument": { "uri": "file:///a.rs" }, "edits": [{ "range": 3, "newText": "x" }] } ] });
    assert!(workspace_edit(&broken, Utf16).is_err());
    assert!(
        workspace_edit(
            &json!({ "documentChanges": [{ "kind": "teleport" }] }),
            Utf16
        )
        .is_err()
    );
}

#[test]
fn completion_items_carry_edits_snippets_and_the_raw_item() {
    let list = json!({ "isIncomplete": true, "items": [
        { "label": "println!", "kind": 3, "insertTextFormat": 2,
          "insertText": "println!(\"$1\")$0", "sortText": "0001",
          "labelDetails": { "detail": "(…)", "description": "macro" },
          "documentation": { "kind": "markdown", "value": "prints" },
          "data": { "id": 7 } },
        { "label": "Vec", "filterText": "vec", "textEdit": { "newText": "Vec",
            "insert": range((0, 0), (0, 2)), "replace": range((0, 0), (0, 5)) },
          "additionalTextEdits": [{ "range": range((0, 0), (0, 0)), "newText": "use x;\n" }],
          "commitCharacters": ["."], "tags": [1], "preselect": true },
        { "kind": 1 },
    ] });
    let list = completion(&list, Utf16);
    assert!(list.incomplete);
    assert_eq!(list.items.len(), 2, "an item without a label is dropped");
    let first = &list.items[0];
    assert!(first.snippet);
    assert_eq!(first.label_detail.as_deref(), Some("(…) macro"));
    assert_eq!(first.documentation.as_deref(), Some("prints"));
    assert_eq!(first.raw["data"]["id"], 7);
    assert_eq!(first.plain_insert(), "println!(\"$1\")$0");
    let second = &list.items[1];
    assert_eq!(second.filter(), "vec");
    let edit = second.edit.as_ref().unwrap();
    assert_eq!(
        (edit.insert.end.character, edit.replace.end.character),
        (2, 5)
    );
    assert_eq!(second.additional_edits.len(), 1);
    assert!(second.deprecated && second.preselect);
    assert_eq!(second.commit_characters, ["."]);
}

#[test]
fn list_defaults_fill_in_what_items_leave_out() {
    let list = completion(
        &json!({ "isIncomplete": false,
            "itemDefaults": { "commitCharacters": [";"], "insertTextFormat": 2,
                              "editRange": range((2, 1), (2, 4)) },
            "items": [{ "label": "a", "textEditText": "a$0" }] }),
        Utf16,
    );
    let item = &list.items[0];
    assert!(item.snippet);
    assert_eq!(item.commit_characters, [";"]);
    let edit = item.edit.as_ref().unwrap();
    assert_eq!(edit.new_text, "a$0");
    assert_eq!(edit.insert.start.line, 2);
    // A bare array is a complete list.
    let bare = completion(&json!([{ "label": "x" }]), Utf16);
    assert!(!bare.incomplete && bare.items.len() == 1);
    assert!(completion(&Value::Null, Utf16).items.is_empty());
}

#[test]
fn signature_labels_are_located_by_text_or_by_offsets() {
    let help = signature_help(&json!({
        "activeSignature": 9, "activeParameter": 1,
        "signatures": [{
            "label": "fn add(a: i32, b: i32)",
            "parameters": [{ "label": "a: i32" }, { "label": [15, 21], "documentation": "second" }],
        }] }))
    .unwrap();
    assert_eq!(help.active_signature, 0, "clamped into range");
    assert_eq!(help.active_parameter, Some(1));
    let params = &help.signatures[0].parameters;
    assert_eq!(params[0].label, 7..13);
    assert_eq!(params[1].label, 15..21);
    assert_eq!(params[1].documentation.as_deref(), Some("second"));
    assert_eq!(signature_help(&json!({ "signatures": [] })), None);
    assert_eq!(signature_help(&Value::Null), None);
    let help = signature_help(&json!({ "activeSignature": 5, "signatures": [
        { "label": "first()" }, { "label": "second()" }
    ] }))
    .unwrap();
    assert_eq!(
        help.active_signature, 0,
        "an invalid overload index defaults to the first signature"
    );
}

#[test]
fn code_actions_may_be_commands_or_need_resolving() {
    let actions = code_actions(
        &json!([
            { "title": "Run it", "command": "do.it", "arguments": [1] },
            { "title": "Fix", "kind": "quickfix", "isPreferred": true,
              "edit": { "changes": { "file:///a.rs": [{ "range": range((0, 0), (0, 1)), "newText": "x" }] } } },
            { "title": "Lazy", "kind": "refactor", "data": 1 },
            { "title": "Nope", "disabled": { "reason": "not here" }, "command": { "title": "c", "command": "x" } },
        ]),
        Utf16,
    );
    assert_eq!(actions[0].command.as_ref().unwrap().command, "do.it");
    assert!(actions[1].preferred && actions[1].edit.is_some() && !actions[1].needs_resolve);
    assert!(actions[2].needs_resolve);
    assert_eq!(actions[3].disabled.as_deref(), Some("not here"));
}

#[test]
fn prepare_rename_answers_in_three_shapes() {
    let plain = prepare_rename(&range((0, 1), (0, 4)), Utf16).unwrap();
    assert_eq!(plain.span.unwrap().start.character, 1);
    let with = prepare_rename(
        &json!({ "range": range((0, 1), (0, 4)), "placeholder": "foo" }),
        Utf16,
    )
    .unwrap();
    assert_eq!(with.placeholder.as_deref(), Some("foo"));
    let default = prepare_rename(&json!({ "defaultBehavior": true }), Utf16).unwrap();
    assert!(default.span.is_none());
    assert_eq!(prepare_rename(&Value::Null, Utf16), None);
}

#[test]
fn hints_folds_selection_chains_lenses_and_hierarchies_parse() {
    let hints = inlay_hints(
        &json!([{ "position": { "line": 1, "character": 4 }, "label": [{ "value": ": " }, { "value": "i32" }],
                  "kind": 1, "paddingLeft": true }]),
        Utf16,
    );
    assert_eq!(hints[0].label, ": i32");
    assert!(hints[0].padding_left && !hints[0].padding_right);

    let folds = folding_ranges(
        &json!([{ "startLine": 1, "endLine": 4, "kind": "region" }, { "startLine": 3, "endLine": 3 }]),
    );
    assert_eq!(folds.len(), 1, "a one-line range folds nothing");

    let chain = selection_chain(
        &json!([{ "range": range((0, 2), (0, 3)), "parent": { "range": range((0, 0), (0, 9)) } }]),
        Utf16,
    );
    assert_eq!(chain.len(), 2);
    assert_eq!(chain[1].end.character, 9);

    let lenses = code_lenses(
        &json!([{ "range": range((0, 0), (0, 1)), "command": { "title": "Run", "command": "run" } },
                { "range": range((2, 0), (2, 1)) }]),
        Utf16,
    );
    assert_eq!(lenses[0].command.as_ref().unwrap().title, "Run");
    assert!(lenses[1].command.is_none());

    let item = json!({ "name": "f", "kind": 12, "uri": "file:///a.rs",
                       "range": range((0, 0), (3, 0)), "selectionRange": range((0, 3), (0, 4)), "data": 5 });
    assert_eq!(
        hierarchy_items(&json!([item.clone()]), Utf16)[0].raw["data"],
        5
    );
    let incoming = calls(
        &json!([{ "from": item.clone(), "fromRanges": [range((7, 1), (7, 2))] }]),
        "from",
        Utf16,
    );
    assert_eq!(incoming[0].sites.len(), 1);
    assert_eq!(incoming[0].item.name, "f");
}
