# Text file encodings

Loog decodes files to Unicode for editing. It preserves their encoding, BOM and
line endings on save. Supported formats are UTF-8 (with or without BOM), UTF-16
LE/BE (with or without BOM), GBK/CP936, GB18030, Big5, Shift_JIS, EUC-JP, EUC-KR
and Windows-1252. New documents default to UTF-8 without BOM.

Click the encoding in the status bar to reinterpret the file immediately in the
editor. The compact selector stays anchored above that status item. A manual
choice always wins: the editor displays the decoded result, including garbled
text or replacement characters, without a separate preview or confirmation.
Selection changes the unsaved state and never writes to disk. Save, Save All and
the close confirmation write the displayed text in the selected format.

When byte evidence supports a suggestion, that encoding appears once at the top
of the selector with a muted `Suggested` label and a separator below it. The
checkmark still indicates the current encoding, and opening the menu or pressing
Enter on its initial selection never adopts the suggestion. Choosing the hint
uses the same immediate display and explicit save flow as every other encoding.
Empty files, plain ASCII and weak or malformed samples have no suggestion.
Suggestions inspect at most 64 KiB of retained source bytes, respect configured
legacy candidates, and do not derive guesses from an intermediate garbled display.

Original bytes are retained until saving, so selecting the original encoding
again restores its text without converting intermediate garbled displays.
Representable unsaved text edits become part of the retained byte interpretation;
edits that cannot fit the previous encoding stay intact when selecting a new save
format. Text and encoding switch together in one undo/redo step. The selected
encoding, unsaved text and retained source survive session restoration.

Automatic opening checks BOM, a conservative BOM-less UTF-16 heuristic, strict
UTF-8, then a legacy detector. Short texts and BOM-less UTF-16 containing mostly
CJK characters can be ambiguous: choose another encoding from the status bar.
If detection is disabled or its result is outside the configured candidates,
opening falls back to UTF-8 display without a recovery dialog. Automatic opening
still rejects UTF-32 and binary files. Malformed sequences display replacement
characters, while the source bytes are retained. Saving text that cannot be represented reports its character,
line and column; the destination is unchanged. Saves are staged in the destination
directory and replaced only after encoding and writing succeed.

User `settings.json` and a project's `.loog/settings.json` accept:

```json
{
  "encoding": {
    "new_file": { "charset": "utf8", "bom": false },
    "auto_detect": true,
    "candidates": ["gbk", "gb18030", "big5", "shift_jis", "euc-jp", "euc-kr", "windows-1252"]
  }
}
```

`new_file` controls new files only. `candidates` limits the detector's accepted
legacy results; it is not proof of a particular encoding. A manual read choice
overrides automatic detection and is retained during reload and restoration.
The nearest project settings override user settings. BOM is valid only for
Unicode encodings. Changing these settings never converts existing files.

Search and read-only file previews use the same decoder and Unicode byte offsets
as the editor. Disk reconciliation compares original bytes, so an external
encoding-only change is detected. Local edits, including pending encoding changes,
are protected by the normal disk-conflict workflow.

Git comparisons decode each side separately, including UTF-16 files Git normally
calls binary. Encoding/BOM-only changes show an `Encoding: ...` row. For decoded
comparisons, whole-file stage/unstage remains available; individual hunk actions
are disabled because the displayed Unicode patch is not a patch of original bytes.
External language servers and tools still need their own support for legacy
encodings when they read files from disk.

Verification: `cargo test --workspace`, including the `encoding` test filter.
An optional private-file regression can be run by setting `LOOG_ENCODING_FIXTURE`
and running `cargo test encoding_private_fixture -- --ignored --nocapture`.
It checks a temporary copy and never changes or prints the original contents.
