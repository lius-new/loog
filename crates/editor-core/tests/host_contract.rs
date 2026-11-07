//! Exercise the exported core as an external host, without application modules.
use std::ops::Range;

use loog_editor_core::commands::{Command, apply};
use loog_editor_core::identity::{FileId, PaneId};
use loog_editor_core::normal::Movement;
use loog_editor_core::text::{Edit, EditError, EditKind, EditorMut, Selection, TextBuffer};
use loog_editor_core::vim::{Clipboard, Host, Key, Request, Vim};

#[test]
fn semantic_commands_preserve_unicode_crlf_and_peer_selections() {
    let original = "中🙂\r\nlast";
    let mut buffer = TextBuffer::new(original);
    let mut active = Selection::caret("中🙂".len());
    let mut peer = Selection::caret(original.len());
    {
        let mut editor = EditorMut::new(&mut buffer, &mut active, vec![&mut peer]);
        apply(&mut editor, Command::Move(Movement::Left, false));
        apply(&mut editor, Command::Delete(true, false));
    }
    assert_eq!(buffer.text(), "中\r\nlast");
    assert_eq!(peer.cursor, original.len() - "🙂".len());
    {
        let mut editor = EditorMut::new(&mut buffer, &mut active, vec![&mut peer]);
        apply(&mut editor, Command::Undo);
    }
    assert_eq!(buffer.text(), original);
    assert_eq!(peer.cursor, original.len());
    assert_eq!(active.cursor, "中".len());
}

#[test]
fn rejected_public_edit_leaves_document_and_all_views_unchanged() {
    let mut buffer = TextBuffer::new("中文");
    let mut active = Selection::caret(0);
    let mut peer = Selection::caret(buffer.len());
    let before = (active, peer, buffer.version());
    let mut editor = EditorMut::new(&mut buffer, &mut active, vec![&mut peer]);
    assert!(matches!(
        editor.edit(EditKind::Other, vec![Edit::delete(1..2)], |_, _| {
            panic!("An invalid edit must not evaluate the selection callback")
        }),
        Err(EditError::NotCharBoundary(1))
    ));
    assert_eq!(buffer.text(), "中文");
    assert_eq!((active, peer, buffer.version()), before);
    assert!(!buffer.can_undo());
}

#[derive(Default)]
struct RecordingHost {
    requests: Vec<Request>,
}
impl Clipboard for RecordingHost {
    fn read(&mut self) -> Result<Option<String>, String> {
        Ok(None)
    }
    fn write(&mut self, _: &str) -> Result<(), String> {
        Err("clipboard unavailable".into())
    }
}
impl Host for RecordingHost {
    fn page_lines(&self) -> usize {
        20
    }
    fn visible_lines(&self) -> Range<usize> {
        0..20
    }
    fn request(&mut self, request: Request) {
        self.requests.push(request);
    }
}

#[test]
fn vim_save_and_quit_are_host_requests_without_file_or_window_access() {
    let mut buffer = TextBuffer::new("unchanged");
    let mut selection = Selection::caret(0);
    let mut editor = EditorMut::new(&mut buffer, &mut selection, Vec::new());
    let mut vim = Vim::default();
    vim.options.enabled = true;
    vim.enter_view(
        loog_editor_core::vim::ViewKey {
            pane: PaneId::new(1),
            document: FileId::new(1),
            view: loog_editor_core::identity::ViewId::new(1),
            input_generation: 0,
        },
        &mut editor,
    );
    let mut host = RecordingHost::default();
    vim.feed_keys(&mut editor, &Key::parse(":w<CR>:q<CR>"), &mut host);
    assert_eq!(
        host.requests,
        [Request::Save, Request::Quit { force: false }]
    );
    assert_eq!(editor.text(), "unchanged");
    assert!(!editor.can_undo());
}
