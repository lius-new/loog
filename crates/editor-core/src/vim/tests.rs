//! Vim behaviour as key sequences: starting text with `|` at the cursor, keys
//! in Vim notation, and the expected text and cursor. Expectations follow
//! Vim with its default options.

use std::ops::Range;

use crate::identity::FileId;
use crate::identity::PaneId;
use crate::text::{EditorMut, Selection, TextBuffer};

use super::*;

#[derive(Default)]
struct TestHost {
    clipboard: Option<String>,
    requests: Vec<Request>,
}

impl Clipboard for TestHost {
    fn read(&mut self) -> Result<Option<String>, String> {
        Ok(self.clipboard.clone())
    }
    fn write(&mut self, text: &str) -> Result<(), String> {
        self.clipboard = Some(text.to_owned());
        Ok(())
    }
}

impl Host for TestHost {
    fn page_lines(&self) -> usize {
        10
    }
    fn visible_lines(&self) -> Range<usize> {
        0..10
    }
    fn request(&mut self, request: Request) {
        self.requests.push(request);
    }
}

struct Harness {
    buffer: TextBuffer,
    selection: Selection,
    vim: Vim,
    host: TestHost,
}

impl Harness {
    fn new(marked: &str) -> Self {
        let cursor = marked.find('|').expect("a | marks the cursor");
        let text = marked.replacen('|', "", 1);
        let mut harness = Self {
            buffer: TextBuffer::new(text),
            selection: Selection::caret(cursor),
            vim: Vim::default(),
            host: TestHost::default(),
        };
        harness.vim.options.enabled = true;
        harness.buffer.configure(crate::text::EditingOptions {
            auto_pairs: false,
            ..Default::default()
        });
        let mut editor = EditorMut::new(&mut harness.buffer, &mut harness.selection, Vec::new());
        harness.vim.enter_view(
            crate::vim::ViewKey {
                pane: PaneId::new(1),
                document: FileId::new(1),
                view: crate::identity::ViewId::new(1),
                input_generation: 0,
            },
            &mut editor,
        );
        harness
    }

    fn keys(&mut self, keys: &str) -> &mut Self {
        for key in Key::parse(keys) {
            let mut editor = EditorMut::new(&mut self.buffer, &mut self.selection, Vec::new());
            self.vim.feed(&mut editor, &mut self.host, key);
        }
        self
    }

    /// The text with `|` at the cursor.
    fn state(&self) -> String {
        let mut text = self.buffer.text().to_owned();
        text.insert(self.selection.cursor, '|');
        text
    }

    fn selected(&self) -> Vec<String> {
        self.vim
            .highlights(&self.buffer, self.selection)
            .unwrap_or_default()
            .into_iter()
            .map(|range| self.buffer.text()[range].to_owned())
            .collect()
    }
}

/// Run `keys` from `start` and compare with `expected`.
#[track_caller]
fn check(start: &str, keys: &str, expected: &str) {
    let mut harness = Harness::new(start);
    harness.keys(keys);
    assert_eq!(harness.state(), expected, "{start:?} after {keys:?}");
}

#[track_caller]
fn check_all(cases: &[(&str, &str, &str)]) {
    for (start, keys, expected) in cases {
        check(start, keys, expected);
    }
}

#[test]
fn character_and_line_motions() {
    check_all(&[
        ("a|bc", "h", "|abc"),
        ("|abc", "h", "|abc"),
        ("a|bc", "l", "ab|c"),
        ("ab|c", "l", "ab|c"),
        ("|abcdef", "3l", "abc|def"),
        ("|abc", "10l", "ab|c"),
        ("  a|bc", "0", "|  abc"),
        ("  a|bc", "^", "  |abc"),
        ("|abc", "$", "ab|c"),
        ("|ab\ncd\nef", "2$", "ab\nc|d\nef"),
        ("ab|cdef\nx\nabcdef", "jj", "abcdef\nx\nab|cdef"),
        ("abc|def\nx", "j", "abcdef\n|x"),
        ("abcd|ef\nab\nabcdef", "$jj", "abcdef\nab\nabcde|f"),
        ("|a\n  b", "<CR>", "a\n  |b"),
        ("a\n  |b", "-", "|a\n  b"),
        ("|a\nb\nc", "G", "a\nb\n|c"),
        ("a\nb\n|c", "gg", "|a\nb\nc"),
        ("|a\nb\nc", "2G", "a\n|b\nc"),
        ("|a\n  b\nc", "2gg", "a\n  |b\nc"),
        ("ab|c\nd", "<Space>", "abc\n|d"),
        ("abc\n|d", "<BS>", "ab|c\nd"),
        ("a\tb|中文", "l", "a\tb中|文"),
        ("中|文\nabcd", "j", "中文\nab|cd"),
    ]);
}

#[test]
fn word_motions() {
    check_all(&[
        ("|foo.bar baz", "w", "foo|.bar baz"),
        ("|foo.bar baz", "W", "foo.bar |baz"),
        ("|foo.bar baz", "3w", "foo.bar |baz"),
        ("foo |bar\n\nbaz", "w", "foo bar\n|\nbaz"),
        ("foo |bar", "w", "foo ba|r"),
        ("|foo bar", "e", "fo|o bar"),
        ("fo|o bar", "e", "foo ba|r"),
        ("foo |bar", "b", "|foo bar"),
        ("foo.|bar", "B", "|foo.bar"),
        ("foo bar |baz", "ge", "foo ba|r baz"),
        ("|abc def", "$b", "abc |def"),
    ]);
}

#[test]
fn finds_matches_and_paragraphs() {
    check_all(&[
        ("|a,b,c", "f,", "a|,b,c"),
        ("|a,b,c", "2f,", "a,b|,c"),
        ("|a,b,c", "t,", "|a,b,c"),
        ("|a,b,c", "f,;", "a,b|,c"),
        ("|a,b,c", "f,;,", "a|,b,c"),
        ("|a,b,c", "t,;", "a,|b,c"),
        ("a,b,|c", "F,", "a,b|,c"),
        ("a,b,|c", "T,", "a,b,|c"),
        ("|f(a(b)c)", "%", "f(a(b)c|)"),
        ("f(a(b)c|)", "%", "f|(a(b)c)"),
        ("|a\nb\n\nc", "}", "a\nb\n|\nc"),
        ("|a\nb\n\nc", "}}", "a\nb\n\n|c"),
        ("a\n\nb\n|c", "{", "a\n|\nb\nc"),
    ]);
}

#[test]
fn operators_with_motions() {
    check_all(&[
        ("|foo bar baz", "dw", "|bar baz"),
        ("|foo bar baz", "d2w", "|baz"),
        ("|foo bar baz", "2dw", "|baz"),
        ("foo |bar\nbaz", "dw", "foo| \nbaz"),
        ("|foo bar", "de", "| bar"),
        ("foo b|ar", "db", "foo |ar"),
        ("a|bcd", "d$", "|a"),
        ("ab|cd", "D", "a|b"),
        ("ab|cd", "d0", "|cd"),
        ("|a,b,c", "dt,", "|,b,c"),
        ("|a,b,c", "df,", "|b,c"),
        ("|a\nb\nc", "dj", "|c"),
        ("a\nb\n|c", "dk", "|a"),
        ("|a\nb\nc", "dG", "|"),
        ("a\n|b\nc", "dgg", "|c"),
        ("|x f(a, b) y", "d%", "| y"),
        ("x |f(a, b) y", "d%", "x | y"),
        ("x f|(a, b) y", "d%", "x f| y"),
        ("|abc", "x", "|bc"),
        ("ab|c", "x", "a|b"),
        ("|abcd", "3x", "|d"),
        ("ab|c", "X", "a|c"),
        ("|abc", "X", "|abc"),
        ("|a\nb", "x", "|\nb"),
        ("|para one\nline\n\nnext", "d}", "|\nnext"),
    ]);
}

#[test]
fn line_operators() {
    check_all(&[
        ("a\n|b\nc", "dd", "a\n|c"),
        ("a\nb\n|c", "dd", "a\n|b"),
        ("|a\nb\nc", "2dd", "|c"),
        ("|a\nb", "5dd", "|"),
        ("a\n  |b\nc", "cc", "a\n  |\nc"),
        ("|a\nb", "yyp", "a\n|a\nb"),
        ("a\n|b", "yyP", "a\n|b\nb"),
        ("|a\nb", "Yjp", "a\nb\n|a"),
        ("x\n|  a\nb", "yyjp", "x\n  a\nb\n  |a"),
        ("|a\r\nb", "ddp", "b\r\n|a"),
    ]);
}

#[test]
fn change_and_insert() {
    check_all(&[
        ("|foo bar", "cwnew<Esc>", "ne|w bar"),
        ("fo|o bar", "cwX<Esc>", "fo|X bar"),
        ("foo|  bar", "cwX<Esc>", "foo|Xbar"),
        ("|foo bar", "c2wX<Esc>", "|X"),
        ("ab|cd", "Cx<Esc>", "ab|x"),
        ("ab|cd", "sX<Esc>", "ab|Xd"),
        ("|ab", "iX<Esc>", "|Xab"),
        ("|ab", "aX<Esc>", "a|Xb"),
        ("  a|b", "IX<Esc>", "  |Xab"),
        ("|ab", "AX<Esc>", "ab|X"),
        ("  |a\nb", "oX<Esc>", "  a\n  |X\nb"),
        ("a\n  |b", "OX<Esc>", "a\n  |X\n  b"),
        ("|", "3ihi<Esc>", "hihih|i"),
        ("|a", "2oX<Esc>", "a\nX\n|X"),
        ("|abc", "ix<BS>y<Esc>", "|yabc"),
        ("|abc", "a<CR><Esc>", "a\n|bc"),
        ("|ab", "i<Esc>", "|ab"),
        ("a|b", "a<Esc>", "a|b"),
        ("|abc", "Rxy<Esc>", "x|yc"),
        ("|ab", "Rxyz<Esc>", "xy|z"),
        ("|abc", "Rxy<BS><BS>z<Esc>", "|zbc"),
        ("|abc", "rx", "|xbc"),
        ("|abc", "3rx", "xx|x"),
        ("|ab", "3rx", "|ab"),
        ("a|bc", "r<CR>", "a\n|c"),
        ("|aBc", "~", "A|Bc"),
        ("|aBc", "3~", "Ab|C"),
        ("|abc def", "gUiw", "|ABC def"),
        ("|ABC def", "guu", "|abc def"),
        ("|abc", "g~~", "|ABC"),
    ]);
}

#[test]
fn text_objects() {
    check_all(&[
        ("foo b|ar baz", "diw", "foo | baz"),
        ("foo b|ar baz", "daw", "foo |baz"),
        ("foo b|ar baz", "ciwX<Esc>", "foo |X baz"),
        ("x = \"he|llo\";", "di\"", "x = \"|\";"),
        ("x = \"he|llo\";", "da\"", "x =|;"),
        ("x = \"he|llo\";", "ci\"X<Esc>", "x = \"|X\";"),
        ("f(a, |b)", "di(", "f(|)"),
        ("f(a, |b)", "da(", "|f"),
        ("f(a, |b)", "ci)X<Esc>", "f(|X)"),
        ("[1, [|2]]", "d2i[", "[|]"),
        ("{\n    |a;\n    b;\n}", "di{", "{\n|}"),
        // Changing whole lines keeps the first line's indentation, as `cc` does.
        ("{\n    |a;\n}", "ci{x<Esc>", "{\n    |x\n}"),
        ("|a\nb\n\nc", "dip", "|\nc"),
        ("|a\nb\n\nc", "dap", "|c"),
    ]);
}

#[test]
fn undo_redo_and_repeat() {
    check_all(&[
        ("|foo bar baz", "dwu", "|foo bar baz"),
        ("foo |bar baz", "dwu<C-r>", "foo |baz"),
        ("|a b c d", "dw..", "|d"),
        ("|a b c d", "dw2.", "|d"),
        ("|a\nb\nc", "ix<Esc>j.", "xa\n|xb\nc"),
        ("|foo foo", "cwbar<Esc>w.", "bar ba|r"),
        ("|a\nb", "Ax<Esc>j.", "ax\nb|x"),
        ("|a b", "3ix<Esc>w.", "xxxa xx|xb"),
        ("|abc", "xu", "|abc"),
        ("|a", "ox<Esc>u", "|a"),
        ("|ab", "iX<Esc>uu", "|ab"),
        ("|x", "3ihi<Esc>u", "|x"),
        ("|abcd", "x.u", "|bcd"),
        ("|abc", "rxl.", "x|xc"),
        ("|a\nb\nc", "ddj.", "|b"),
        ("|aa bb cc", "vlld.", "|cc"),
    ]);
}

#[test]
fn undo_puts_the_cursor_on_the_change() {
    let mut harness = Harness::new("one two |three");
    harness.keys("dwu");
    assert_eq!(harness.state(), "one two |three");
    harness.keys("0dwu");
    assert_eq!(harness.state(), "|one two three");
    harness.keys("<C-r>");
    assert_eq!(harness.state(), "|two three");
}

#[test]
fn registers_and_put() {
    check_all(&[
        ("|ab", "ylp", "a|ab"),
        ("|ab", "ylP", "|aab"),
        ("|ab", "yl3p", "aaa|ab"),
        ("|foo bar", "dwwP", "bafoo| r"),
        ("|a\nb", "\"ayyj\"ap", "a\nb\n|a"),
        ("|a\nb", "\"ayyj\"Ayy\"ap", "a\nb\n|a\nb"),
        ("|one two", "yiwwviwp", "one |one"),
        ("|a\nb", "yyjVp", "a\n|a"),
        ("|x\na\nb", "dd\"1p", "a\n|x\nb"),
        ("|ab", "x\"-p", "b|a"),
        ("|ab", "yl\"_dd\"0p", "|a"),
    ]);
}

#[test]
fn visual_selections() {
    let mut harness = Harness::new("a|bcd");
    harness.keys("vl");
    assert_eq!(harness.vim.mode(), Mode::Visual(VisualKind::Char));
    assert_eq!(harness.selected(), ["bc"]);
    harness.keys("o");
    assert_eq!(harness.state(), "a|bcd");
    harness.keys("<Esc>");
    assert_eq!(harness.vim.mode(), Mode::Normal);
    assert!(harness.selected().is_empty());

    let mut harness = Harness::new("ab\nc|d\nef");
    harness.keys("Vk");
    assert_eq!(harness.selected(), ["ab\ncd\n"]);

    let mut harness = Harness::new("a|bc\ndef\ng\nhij");
    harness.keys("<C-v>jjjl");
    assert_eq!(harness.selected(), ["bc", "ef", "ij"]);

    let mut harness = Harness::new("foo b|ar baz");
    harness.keys("viw");
    assert_eq!(harness.selected(), ["bar"]);
    harness.keys("<Esc>vaw");
    assert_eq!(harness.selected(), ["bar "]);

    check_all(&[
        ("a|bcd", "vld", "a|d"),
        ("a|bcd", "vlc<Esc>", "|ad"),
        ("a|bcd", "vlyP", "ab|cbcd"),
        ("|a\nb\nc", "Vjd", "|c"),
        ("|a\nb\nc", "VjJ", "a| b\nc"),
        ("|ab\ncd", "vjU", "|AB\nCd"),
        ("|ab\ncd\nef", "Vj>", "  |ab\n  cd\nef"),
        ("|abc\ndef", "<C-v>jld", "|c\nf"),
        ("|abc\ndef", "<C-v>jlyGp", "abc\nd|abef\n de"),
        ("|abc\ndef", "<C-v>jIx<Esc>", "|xabc\nxdef"),
        ("|abc\ndef", "<C-v>j$Ax<Esc>", "abc|x\ndefx"),
        ("|abc\ndef", "<C-v>jcX<Esc>", "|Xbc\nXef"),
        ("a|b\n\nc", "vj$d", "a|c"),
        ("|ab", "vrx", "|xb"),
    ]);
}

#[test]
fn search_and_star() {
    check_all(&[
        ("|foo bar foo", "/foo<CR>", "foo bar |foo"),
        ("|foo bar foo", "/foo<CR>n", "|foo bar foo"),
        ("|foo bar foo", "/foo<CR>N", "|foo bar foo"),
        ("foo bar |foo", "?bar<CR>", "foo |bar foo"),
        ("|foo bar foo", "*", "foo bar |foo"),
        ("|foo bar foo", "#", "foo bar |foo"),
        ("|foo foobar foo", "*", "foo foobar |foo"),
        ("|a b c", "d/c<CR>", "|c"),
        ("|x.y xy", "/x\\.y<CR>", "|x.y xy"),
        ("|ab ab", "/<CR>", "|ab ab"),
        ("|abc", "/zz<CR>", "|abc"),
    ]);
    let mut harness = Harness::new("|abc");
    harness.keys("/zz<CR>");
    assert!(harness.vim.status().message.unwrap().error);
}

#[test]
fn joins_and_counts() {
    check_all(&[
        ("|a\n  b", "J", "a| b"),
        ("|a\nb\nc", "3J", "a b| c"),
        ("|a \nb", "J", "a |b"),
        ("|a\n)b", "J", "a|)b"),
        ("|a\n  b", "gJ", "a|  b"),
        ("|a", "J", "|a"),
        ("x |9 y", "<C-a>", "x 1|0 y"),
        ("|x -3", "5<C-a>", "x |2"),
        ("|7", "<C-x>", "|6"),
    ]);
}

#[test]
fn command_line_and_ex() {
    check_all(&[
        ("|a\nb\nc", ":3<CR>", "a\nb\n|c"),
        ("|foo foo\nfoo", ":s/foo/bar/<CR>", "|bar foo\nfoo"),
        ("|foo foo\nfoo", ":%s/foo/bar/g<CR>", "bar bar\n|bar"),
        ("|a1 b2", ":s/\\(\\w\\)\\(\\d\\)/\\2\\1/g<CR>", "|1a 2b"),
        ("|a,b", ":s/,/\\r/<CR>", "a\n|b"),
        ("|x\ny\nz", "Vj:s/$/!/<CR>", "x!\n|y!\nz"),
        ("|a\nb\nc", ":2,3d<CR>", "|a"),
        ("|ab", ":<Esc>x", "|b"),
        ("|ab", ":xyz<BS><BS><BS><BS>x", "|b"),
    ]);
    let mut harness = Harness::new("|a");
    harness.keys(":w<CR>:q<CR>:q!<CR>:wq<CR>:x<CR>");
    assert_eq!(
        harness.host.requests,
        [
            Request::Save,
            Request::Quit { force: false },
            Request::Quit { force: true },
            Request::SaveAndQuit,
            Request::SaveAndQuit,
        ]
    );
    harness.keys(":nope<CR>");
    assert!(
        harness
            .vim
            .status()
            .message
            .unwrap()
            .text
            .starts_with("E492")
    );
}

#[test]
fn marks_jumps_and_macros() {
    check_all(&[
        ("|a\nb\nc", "majj'a", "|a\nb\nc"),
        ("a|bc\nd", "majd`a", "|a\nd"),
        ("|a\nb\nc", "G''", "|a\nb\nc"),
        ("|a\nb\nc", "G<C-o>", "|a\nb\nc"),
        ("|a\nb\nc", "G<C-o><Tab>", "a\nb\n|c"),
        ("|1 2 3", "qaxlq@a", "  |3"),
        ("|a\nb\nc", "qaI-<Esc>jq2@a", "-a\n-b\n|-c"),
        ("|a b c d", "qadwq@a@@", "|d"),
    ]);
}

#[test]
fn insert_mode_keys() {
    check_all(&[
        ("|", "ifoo bar<C-w><Esc>", "foo| "),
        ("  ab|c", "ax<C-u><Esc>", " | "),
        ("|a", "yiwA <C-r>\"<Esc>", "a |a"),
        ("|ab", "A<C-o>0X<Esc>", "|Xab"),
        ("|ab", "i<Left><Right>x<Esc>", "a|xb"),
    ]);
}

#[test]
fn counts_on_simple_commands_and_failures_do_nothing() {
    check_all(&[
        ("|abc", "dl", "|bc"),
        ("|", "dw", "|"),
        ("|", "x", "|"),
        ("|a", "dh", "|a"),
        ("|abc", "d<Esc>", "|abc"),
        ("|abc", "dZ", "|abc"),
        ("|abc", "\"#yl", "|abc"),
    ]);
}

#[test]
fn modes_report_status_and_cursor_shape() {
    let mut harness = Harness::new("|abc");
    assert_eq!(harness.vim.cursor_shape(), CursorShape::Block);
    harness.keys("2d");
    assert_eq!(harness.vim.status().pending, "2d");
    assert_eq!(harness.vim.cursor_shape(), CursorShape::HalfBlock);
    harness.keys("<Esc>i");
    assert_eq!(harness.vim.status().mode, "INSERT");
    assert_eq!(harness.vim.cursor_shape(), CursorShape::Bar);
    assert!(harness.vim.accepts_text());
    harness.keys("<Esc>R");
    assert_eq!(harness.vim.cursor_shape(), CursorShape::Underline);
    harness.keys("<Esc>:s");
    assert_eq!(harness.vim.status().command_line.as_deref(), Some(":s"));
    harness.keys("<Esc>qa");
    assert_eq!(harness.vim.status().recording, Some('a'));
}

#[test]
fn system_clipboard_registers() {
    let mut harness = Harness::new("|line\nnext");
    harness.keys("\"+yy");
    assert_eq!(
        harness.host.clipboard.as_deref(),
        Some(if cfg!(windows) { "line\r\n" } else { "line\n" })
    );
    harness.keys("j\"+p");
    assert_eq!(harness.state(), "line\nnext\n|line");
    harness.host.clipboard = Some("word".into());
    harness.keys("\"*P");
    assert_eq!(harness.state(), "line\nnext\nwor|dline");

    let mut synced = Harness::new("|ab");
    synced.vim.options.system_clipboard = true;
    synced.keys("yl");
    assert_eq!(synced.host.clipboard.as_deref(), Some("a"));
    synced.host.clipboard = Some("Z".into());
    synced.keys("p");
    assert_eq!(synced.state(), "a|Zb");
}

#[test]
fn foreign_selections_become_visual_and_leaving_ends_insert() {
    let mut harness = Harness::new("|abcd");
    // A pointer drag selects 1..3 the way the default editor does.
    harness.selection = Selection::spanning(1, 3);
    let mut editor = EditorMut::new(&mut harness.buffer, &mut harness.selection, Vec::new());
    harness.vim.after_pointer(&mut editor);
    drop(editor);
    assert_eq!(harness.vim.mode(), Mode::Visual(VisualKind::Char));
    assert_eq!(harness.selected(), ["bc"]);
    harness.keys("d");
    assert_eq!(harness.state(), "a|d");

    harness.keys("ix");
    let mut editor = EditorMut::new(&mut harness.buffer, &mut harness.selection, Vec::new());
    harness.vim.leave_view(Some(&mut editor));
    assert_eq!(harness.vim.mode(), Mode::Normal);
    assert!(!editor.buffer().can_redo());
    editor.undo();
    assert_eq!(editor.text(), "ad");
}

#[test]
fn vim_editor_pairing_repeats_counts_dot_and_restores_provenance() {
    let mut h = Harness::new("|");
    h.buffer.configure(crate::text::EditingOptions {
        language: "rust".into(),
        ..Default::default()
    });
    h.keys("i(");
    assert_eq!(h.state(), "(|)");
    h.keys(")<Esc>");
    assert_eq!(h.buffer.text(), "()");
    h.keys("0.");
    assert_eq!(h.buffer.text(), "()()");
    h.keys("u");
    assert_eq!(h.buffer.text(), "()");
    h.keys("<C-r>");
    assert_eq!(h.buffer.text(), "()()");
    let mut h = Harness::new("|");
    h.buffer.configure(crate::text::EditingOptions {
        language: "rust".into(),
        ..Default::default()
    });
    h.keys("3i(<Esc>");
    assert_eq!(h.buffer.text(), "((()))");
    h.keys("u");
    assert_eq!(h.buffer.text(), "");
    let mut h = Harness::new("|");
    h.buffer.configure(crate::text::EditingOptions {
        language: "rust".into(),
        ..Default::default()
    });
    h.keys("i{<BS><Esc>");
    assert_eq!(h.buffer.text(), "");
}

#[test]
fn vim_editor_literal_composition_stays_literal_in_dot_and_macro_replay() {
    let mut h = Harness::new("|");
    h.buffer.configure(crate::text::EditingOptions {
        language: "rust".into(),
        ..Default::default()
    });
    h.keys("qai");
    {
        let mut editor = EditorMut::new(&mut h.buffer, &mut h.selection, vec![]);
        assert!(h.vim.handle_commit(&mut editor, "(中文"));
    }
    h.keys("<Esc>q0@a");
    assert_eq!(h.buffer.text(), "(中文(中文");
    assert!(!h.buffer.text().contains(')'));
    h.keys(".");
    assert_eq!(h.buffer.text().matches('(').count(), 3);
    assert!(!h.buffer.text().contains(')'));
    let keys = [
        Key::Literal('('),
        Key::Literal('中'),
        Key::Literal('\u{301}'),
    ];
    for key in keys {
        assert_eq!(Key::parse(&key.notation()), vec![key]);
    }
}

#[test]
fn vim_editor_shift_and_open_lines_follow_indent_and_crlf_preferences() {
    let mut h = Harness::new("|fn f() {\r\n}\r\n");
    h.buffer.configure(crate::text::EditingOptions {
        language: "rust".into(),
        indent_size: 4,
        ..Default::default()
    });
    h.keys("ox<Esc>");
    assert_eq!(h.buffer.text(), "fn f() {\r\n    x\r\n}\r\n");
    h.keys(">>");
    assert_eq!(h.buffer.line(1), "        x");
    h.keys("<<");
    assert_eq!(h.buffer.line(1), "    x");
    h.buffer.configure(crate::text::EditingOptions {
        language: "rust".into(),
        indent_size: 4,
        use_tabs: true,
        tab_width: 8,
        ..Default::default()
    });
    h.keys(">>");
    assert_eq!(h.buffer.line(1), "\t    x");
    h.keys("<<");
    assert_eq!(h.buffer.line(1), "    x");
}

#[test]
fn vim_editor_folding_requests_are_native_and_do_not_change_undo_history() {
    let mut h = Harness::new("|fn f() {}\n");
    h.keys("zczozMzR");
    assert_eq!(
        h.host.requests,
        vec![
            Request::Fold {
                open: false,
                all: false
            },
            Request::Fold {
                open: true,
                all: false
            },
            Request::Fold {
                open: false,
                all: true
            },
            Request::Fold {
                open: true,
                all: true
            }
        ]
    );
    assert!(!h.buffer.can_undo());
}

#[test]
fn vim_editor_metadata_conversion_splits_insert_history_and_count_replay() {
    let mut h = Harness::new("|a\nb");
    h.keys("i12");
    {
        let mut editor = EditorMut::new(&mut h.buffer, &mut h.selection, vec![]);
        editor
            .set_line_ending(crate::text::LineEnding::Crlf)
            .unwrap();
    }
    h.keys("34<Esc>u");
    assert_eq!(h.buffer.text(), "12a\r\nb");
    h.keys("u");
    assert_eq!(h.buffer.text(), "12a\nb");
    h.keys("u");
    assert_eq!(h.buffer.text(), "a\nb");
    let mut h = Harness::new("|x\ny");
    h.keys("3iA");
    {
        let mut editor = EditorMut::new(&mut h.buffer, &mut h.selection, vec![]);
        editor
            .set_line_ending(crate::text::LineEnding::Crlf)
            .unwrap();
    }
    h.keys("<Esc>");
    assert_eq!(h.buffer.text(), "Ax\r\ny");
}
